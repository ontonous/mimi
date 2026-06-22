#!/usr/bin/env python3
"""MimiSpec error code coverage scanner.

Scans the codebase to produce a mapping of:
  - All defined error codes (from codes.rs)
  - Where each code is emitted in production code
  - Which tests verify each code
  - Coverage gaps
"""

import re
import subprocess
import sys
from pathlib import Path
from collections import defaultdict

SRC_DIR = Path(__file__).resolve().parents[1] / "src"

# ── Step 1: Parse all defined codes ──────────────────────────────────

def parse_defined_codes() -> dict[str, str]:
    """Return {CODE: description} for all pub const definitions in codes.rs."""
    codes_file = SRC_DIR / "diagnostic" / "codes.rs"
    text = codes_file.read_text()
    # Match: pub const E0123: &str = "E0123"; // description
    pattern = re.compile(r'^pub const ([A-Z]+\d+):.*?= ["\'](.+?)["\'].*?// (.+)$', re.MULTILINE)
    codes = {}
    for m in pattern.finditer(text):
        const_name = m.group(1)
        code_value = m.group(2)
        desc = m.group(3).strip()
        codes[code_value] = desc
    return codes


# ── Step 2: Find all emit sites in production code ───────────────────

def find_emit_sites() -> dict[str, list[str]]:
    """Return {CODE: [list of file:line locations]} for production emit_code/emit_warning_code calls."""
    sites = defaultdict(list)

    # Pattern 1: emit_code(codes::E0123, ...)
    # Pattern 2: emit_warning_code(codes::W005, ...)
    # Pattern 3: Diagnostic::error_code("E0123", ...)
    # Pattern 4: CompileError variant → code() method

    result = subprocess.run(
        ["grep", "-rn", 
         r'emit_code\|emit_warning_code\|emit_warning\|Diagnostic::error_code\|Diagnostic::warning_code',
         str(SRC_DIR)],
        capture_output=True, text=True
    )
    # Filter out test files
    for line in result.stdout.splitlines():
        if "/tests/" in line:
            continue
        file_line = line.split(":", 2)
        if len(file_line) < 2:
            continue
        path = file_line[0]
        lineno = file_line[1]
        content = file_line[2] if len(file_line) > 2 else ""

        # Match codes::CONSTANT
        for m in re.finditer(r'codes::([A-Z]+\d+)', content):
            code_value = m.group(1)
            # Normalize: E0600, W005 etc are already code values except
            # we need to map const name → code string, but const name *is* the code string
            # Actually pub const E0123 = "E0123", so const name == code string
            sites[code_value].append(f"{path}:{lineno}")

    # Pattern: CompileError::code() mapping — all variants in error.rs
    # These are "always available" but only triggered when the variant is constructed.
    # We'll mark them with note.

    return dict(sites)


# ── Step 3: Find test references ─────────────────────────────────────

def find_test_refs() -> dict[str, list[str]]:
    """Return {CODE: [list of test locations]}."""
    refs = defaultdict(list)
    result = subprocess.run(
        ["grep", "-rn", r'E[0-9]\{4\}\|W[0-9]\{3\}', str(SRC_DIR / "tests")],
        capture_output=True, text=True
    )
    for line in result.stdout.splitlines():
        file_line = line.split(":", 2)
        if len(file_line) < 2:
            continue
        path = file_line[0]
        lineno = file_line[1]
        content = file_line[2] if len(file_line) > 2 else ""

        for m in re.finditer(r'(E\d{4}|W\d{3})', content):
            code = m.group(1)
            refs[code].append(f"{path}:{lineno}")

    return dict(refs)


# ── Step 4: Also find CompileError variant → code mappings ───────────

def parse_compile_error_mapping() -> dict[str, str]:
    """Return {E0CODE: [variant names]} from error.rs code() method."""
    error_file = SRC_DIR / "error.rs"
    text = error_file.read_text()
    mapping = defaultdict(list)
    # Match: Self::FooBar(_) => E0123,
    pattern = re.compile(r'Self::(\w+)\(.*?\) => ([A-Z]+\d+),?')
    for m in pattern.finditer(text):
        variant = m.group(1)
        code = m.group(2)
        mapping[code].append(variant)
    return dict(mapping)


# ── Main ─────────────────────────────────────────────────────────────

def main():
    codes = parse_defined_codes()
    emit_sites = find_emit_sites()
    test_refs = find_test_refs()
    compile_err_map = parse_compile_error_mapping()

    # Sort codes by number
    def sort_key(code):
        prefix = code[0]
        num = int(code[1:])
        return (prefix, num)

    sorted_codes = sorted(codes.keys(), key=sort_key)

    # Stats
    total = len(codes)
    emitted = set(emit_sites.keys()) | set(compile_err_map.keys())
    tested = set(test_refs.keys())
    uncovered = sorted(emitted - tested, key=sort_key)
    dead = sorted(set(codes.keys()) - emitted, key=sort_key)
    tested_but_not_emitted = sorted(tested - emitted, key=sort_key)

    # Print report
    print("=" * 120)
    print("MimiSpec Error Code Coverage Report")
    print("=" * 120)

    print(f"\n{'Code':<10} {'Emitted?':<10} {'Tested?':<10} {'Description'}")
    print("-" * 120)
    for code in sorted_codes:
        desc = codes[code]
        in_prod = "YES" if code in emitted else "no"
        in_test = "YES" if code in test_refs else "NO"
        # Highlight uncovered
        marker = " *** UNCOVERED ***" if in_prod == "YES" and in_test == "NO" else ""
        print(f"{code:<10} {in_prod:<10} {in_test:<10} {desc}{marker}")

    print(f"\n{'─' * 120}")
    print(f"Total codes defined: {total}")
    print(f"Codes emitted in production: {len(emitted)}")
    print(f"Codes with test coverage: {len(tested)}")
    print(f"Codes emitted but NOT tested: {len(uncovered)}")
    print(f"Codes defined but NEVER emitted (dead): {len(dead)}")
    print(f"Codes tested but NOT emitted (stale tests): {len(tested_but_not_emitted)}")

    if uncovered:
        print(f"\n{'─' * 120}")
        print(f"UNCOVERED CODES (emitted but no test):")
        for code in uncovered:
            desc = codes.get(code, "?")
            variants = compile_err_map.get(code, [])
            variant_str = f" (via CompileError: {', '.join(variants)})" if variants else ""
            emit_locs = emit_sites.get(code, [])
            print(f"  {code}: {desc}{variant_str}")
            for loc in emit_locs[:5]:
                print(f"    └─ {loc}")

    if dead:
        print(f"\n{'─' * 120}")
        print(f"DEAD CODES (defined but never emitted):")
        for code in dead:
            desc = codes.get(code, "?")
            print(f"  {code}: {desc}")

    if tested_but_not_emitted:
        print(f"\n{'─' * 120}")
        print(f"STALE TEST REFS (test references code not emitted):")
        for code in tested_but_not_emitted:
            print(f"  {code}")
            for loc in test_refs[code]:
                print(f"    └─ {loc}")

    # ── Detailed coverage table (Markdown) ──
    print(f"\n\n{'=' * 120}")
    print("MARKDOWN TABLE")
    print("=" * 120)
    print()
    print("| Code | Description | Emitted In | Test Coverage |")
    print("|------|-------------|------------|---------------|")
    for code in sorted(set(codes.keys()) | set(test_refs.keys()), key=sort_key):
        desc = codes.get(code, "???")
        emit_locs = emit_sites.get(code, []) + compile_err_map.get(code, [])
        emit_str = ", ".join(emit_locs[:3]) if emit_locs else "—"
        if len(emit_locs) > 3:
            emit_str += f" (+{len(emit_locs)-3} more)"
        test_locs = test_refs.get(code, [])
        test_str = ", ".join(test_locs[:3]) if test_locs else "—"
        if len(test_locs) > 3:
            test_str += f" (+{len(test_locs)-3} more)"
        print(f"| {code} | {desc} | {emit_str} | {test_str} |")

    # ── Test file inventory ──
    print(f"\n\n{'=' * 120}")
    print("TEST FILES CONTAINING ERROR CODE REFERENCES")
    print("=" * 120)
    test_files = set()
    for locs in test_refs.values():
        for loc in locs:
            test_files.add(loc.split(":")[0])
    for tf in sorted(test_files):
        print(f"  {tf}")

    return 0 if not uncovered else 1


if __name__ == "__main__":
    sys.exit(main())
