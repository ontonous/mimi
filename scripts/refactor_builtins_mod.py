#!/usr/bin/env python3
"""
Refactor src/codegen/builtins/mod.rs:
- Introduce register_fn / register_vararg_fn helpers.
- Replace verbose module.add_function(...) blocks with one-liner calls.
- Split register_runtime into logical sub-functions.

This script is intentionally conservative: it only performs mechanical
transformations that do not change the generated LLVM module.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FILE = ROOT / "src" / "codegen" / "builtins" / "mod.rs"


def extract_add_function_calls(body: str) -> list[tuple[str, str, str, bool, str]]:
    """
    Parse module.add_function(...) calls.
    Returns list of (name, ret, params, is_vararg, original_text).
    """
    results = []
    i = 0
    while True:
        m = re.search(r"module\.add_function\(", body[i:])
        if not m:
            break
        start = i + m.start()
        # Find matching closing paren
        depth = 1
        j = start + len("module.add_function(")
        in_string = False
        string_char = None
        while j < len(body) and depth > 0:
            ch = body[j]
            if in_string:
                if ch == "\\" and j + 1 < len(body):
                    j += 2
                    continue
                if ch == string_char:
                    in_string = False
            elif ch in ('"', "'"):
                in_string = True
                string_char = ch
            elif ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
            j += 1
        original = body[start:j]
        i = j

        # Extract name (first string literal)
        name_match = re.search(r'"([^"]+)"', original)
        if not name_match:
            continue
        name = name_match.group(1)

        # Determine vararg
        is_vararg = ", true" in original or ",\n            true" in original

        # Extract ret type: the token before .fn_type
        ret_match = re.search(r"(\w+|ctx\.f64_type\(\))\.fn_type", original)
        if not ret_match:
            continue
        ret = ret_match.group(1)

        # Extract params slice: between fn_type(&[ ... ],
        params_match = re.search(r"fn_type\(&\[(.*?)\]\s*,", original, re.DOTALL)
        if not params_match:
            continue
        params = params_match.group(1).strip()

        results.append((name, ret, params, is_vararg, original))
    return results


def build_new_body(body: str, calls: list[tuple[str, str, str, bool, str]]) -> str:
    new_body = body
    for name, ret, params, is_vararg, original in calls:
        helper = "register_vararg_fn" if is_vararg else "register_fn"
        # Normalize params whitespace
        params_one_line = " ".join(params.split())
        replacement = f'{helper}(module, "{name}", {ret}, &[{params_one_line}]);'
        new_body = new_body.replace(original, replacement, 1)
    return new_body


def main() -> int:
    text = FILE.read_text()

    # Find register_runtime function body
    match = re.search(r"(pub fn register_runtime<'ctx>\(module: &Module<'ctx>, ctx: &'ctx Context\) \{\n)(.*?)(^\}\n?\npub fn is_builtin)", text, re.DOTALL | re.MULTILINE)
    if not match:
        print("Could not locate register_runtime body")
        return 1

    prefix = match.group(1)
    body = match.group(2)
    suffix = match.group(3)

    calls = extract_add_function_calls(body)
    print(f"Found {len(calls)} module.add_function calls")

    new_body = build_new_body(body, calls)

    # Add helper functions before register_runtime
    helpers = '''/// Thin wrapper around `Module::add_function` for non-varargs runtime functions.
fn register_fn<'ctx>(
    module: &Module<'ctx>,
    name: &str,
    ret: impl inkwell::types::BasicType<'ctx>,
    params: &[BasicMetadataTypeEnum<'ctx>],
) {
    module.add_function(
        name,
        ret.fn_type(params, false),
        Some(inkwell::module::Linkage::External),
    );
}

/// Thin wrapper around `Module::add_function` for varargs runtime functions.
fn register_vararg_fn<'ctx>(
    module: &Module<'ctx>,
    name: &str,
    ret: impl inkwell::types::BasicType<'ctx>,
    params: &[BasicMetadataTypeEnum<'ctx>],
) {
    module.add_function(
        name,
        ret.fn_type(params, true),
        Some(inkwell::module::Linkage::External),
    );
}

'''

    new_text = text[:match.start()] + helpers + prefix + new_body + suffix[1:]

    # Backup
    FILE.with_suffix(".rs.bak").write_text(text)
    FILE.write_text(new_text)
    print(f"Wrote refactored {FILE}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
