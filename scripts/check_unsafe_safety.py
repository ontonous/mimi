#!/usr/bin/env python3
"""CI gate: forbid NEW unsafe blocks without a SAFETY comment outside src/runtime/.

Strategy (v0.31.7 止血 II, fix-plan Phase 3d / AGENTS.md §21.2 red line #1):
the runtime carries ~866 unsafe blocks lacking SAFETY comments; those are
deferred to the Phase 4d runtime split and are NOT gated here. Non-runtime
code has a locked baseline (BASELINE_NON_RUNTIME) of such blocks. This gate
fails when the non-runtime count EXCEEDS the baseline — i.e. no new
uncommented unsafe may be introduced outside src/runtime/. Fixing existing
baseline entries is always welcome; when you do, lower BASELINE_NON_RUNTIME.

Detection heuristic (deterministic): a line that opens an unsafe context
(`unsafe {`, `unsafe fn`, `unsafe extern`, `unsafe impl`, `unsafe trait`, or a
bare `unsafe` token ending the line) with no comment containing `SAFETY` in the
preceding LOOKBACK lines counts as a violation. Pure comment lines and text
after `//` on a code line are ignored so doc prose mentioning "unsafe" does not
false-match.

Usage:
  scripts/check_unsafe_safety.py            # gate mode: exit 1 if over baseline
  scripts/check_unsafe_safety.py --list     # print every non-runtime violation
"""
import os
import re
import sys

# Locked baseline of unsafe blocks lacking a SAFETY comment in NON-runtime
# code, measured 2026-07-22 (v0.31.7). Do not raise this number; lower it as
# blocks are documented.
BASELINE_NON_RUNTIME = 48
LOOKBACK = 5

OPENER = re.compile(r"\bunsafe\b\s*(\{|fn\b|extern\b|impl\b|trait\b|$)")
SAFETY = re.compile(r"SAFETY", re.IGNORECASE)
# Strip string-literal contents so prose like `"unsafe extern ..."` in an assert
# message does not false-match. Handles escaped quotes; raw/multiline strings
# are not fully modelled (acceptable for a baseline-lock heuristic).
STRING_LIT = re.compile(r'"(?:\\.|[^"\\])*"')


def is_comment_line(s: str) -> bool:
    t = s.lstrip()
    return t.startswith("//") or t.startswith("/*") or t.startswith("*")


def scan_file(path: str):
    """Return list of (lineno, line) violations in this file."""
    with open(path, encoding="utf-8", errors="replace") as f:
        lines = f.readlines()
    out = []
    for i, line in enumerate(lines):
        if is_comment_line(line):
            continue
        code = line.split("//", 1)[0]
        code = STRING_LIT.sub('""', code)
        if not OPENER.search(code):
            continue
        has_safety = any(SAFETY.search(lines[j]) for j in range(max(0, i - LOOKBACK), i + 1))
        if not has_safety:
            out.append((i + 1, line.rstrip()))
    return out


def is_runtime(path: str) -> bool:
    norm = path.replace(os.sep, "/")
    return "/runtime/" in norm or norm.startswith("src/runtime/")


def main():
    root = "src"
    list_mode = "--list" in sys.argv
    non_runtime = []  # (path, lineno, line)
    for dirpath, _, files in os.walk(root):
        for fn in sorted(files):
            if not fn.endswith(".rs"):
                continue
            p = os.path.join(dirpath, fn)
            if is_runtime(p):
                continue
            for lineno, line in scan_file(p):
                non_runtime.append((p, lineno, line))

    count = len(non_runtime)
    if list_mode:
        for p, lineno, line in non_runtime:
            print(f"{p}:{lineno}: {line.strip()}")
        print(f"\nnon-runtime unsafe-without-SAFETY: {count} (baseline {BASELINE_NON_RUNTIME})")
        return 0

    if count > BASELINE_NON_RUNTIME:
        print(
            f"::error::unsafe SAFETY gate failed: {count} unsafe blocks without a "
            f"SAFETY comment in non-runtime code (baseline {BASELINE_NON_RUNTIME})."
        )
        print("New unsafe blocks outside src/runtime/ must carry a `// SAFETY:` comment.")
        print("Offenders:")
        for p, lineno, line in non_runtime:
            print(f"  {p}:{lineno}: {line.strip()}")
        return 1

    print(
        f"unsafe SAFETY gate OK: {count} non-runtime unsafe blocks without SAFETY "
        f"(baseline {BASELINE_NON_RUNTIME}, runtime excluded — see Phase 4d)."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
