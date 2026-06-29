#!/usr/bin/env python3
"""
Split src/codegen/builtins/mod.rs::register_runtime into logical sub-functions.

Strategy:
- Locate the register_runtime function by bracket matching.
- Split its body on section divider comments.
- Emit one helper function per section and replace the body with calls.

Purely mechanical: no change to the order/content of module.add_function calls.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FILE = ROOT / "src" / "codegen" / "builtins" / "mod.rs"


def slugify(name: str) -> str:
    name = name.lower()
    name = re.sub(r"[^a-z0-9]+", "_", name)
    name = name.strip("_")
    name = name.replace("runtime_functions", "fns")
    name = name.replace("functions", "fns")
    name = name.replace("runtime", "rt")
    return name


def find_function_body(text: str, fn_name: str) -> tuple[int, int, int, int]:
    """
    Find the body of a top-level function `fn_name`.
    Returns (fn_start, body_start, body_end, fn_end) indices in text.
    """
    decl_re = re.compile(rf"^pub fn {re.escape(fn_name)}<" + r"'ctx>\(.*?\) \{", re.MULTILINE)
    m = decl_re.search(text)
    if not m:
        raise RuntimeError(f"Could not find declaration of {fn_name}")
    fn_start = m.start()
    body_start = m.end()  # just after the opening brace

    # Match braces, skipping strings and comments.
    depth = 1
    i = body_start
    while i < len(text) and depth > 0:
        ch = text[i]
        if ch == '"':
            i += 1
            while i < len(text) and text[i] != '"':
                if text[i] == "\\" and i + 1 < len(text):
                    i += 2
                else:
                    i += 1
            i += 1
            continue
        if ch == "'":
            # Lifetime or char; skip until next quote
            i += 1
            while i < len(text) and text[i] != "'":
                i += 1
            i += 1
            continue
        if text[i : i + 2] == "//":
            end = text.find("\n", i)
            i = end if end != -1 else len(text)
            continue
        if text[i : i + 2] == "/*":
            end = text.find("*/", i + 2)
            i = end + 2 if end != -1 else len(text)
            continue
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
        i += 1

    fn_end = i
    body_end = fn_end - 1  # position of the closing brace
    return fn_start, body_start, body_end, fn_end


def split_sections(body: str) -> list[tuple[str, str]]:
    lines = body.splitlines(keepends=True)
    sections: list[tuple[str, list[str]]] = []
    current_name = "libc"
    current_lines: list[str] = []

    header_re = re.compile(r"^\s*//\s*[-=]+\s*(.*?)\s*[-=]+\s*$")
    simple_header_re = re.compile(r"^\s*//\s*(.*(?:functions|runtime|stubs).*)\s*$")

    for line in lines:
        m = header_re.match(line) or simple_header_re.match(line)
        if m:
            if current_lines:
                sections.append((current_name, current_lines))
            current_name = m.group(1).strip()
            current_lines = [line]
        else:
            current_lines.append(line)
    if current_lines:
        sections.append((current_name, current_lines))
    return [(n, "".join(ls)) for n, ls in sections]


def main() -> int:
    text = FILE.read_text()

    fn_start, body_start, body_end, fn_end = find_function_body(text, "register_runtime")
    body = text[body_start:body_end]

    # The first part of the body is local variable declarations; keep them in
    # register_runtime and split everything after the first blank line.
    local_end = body.find("\n\n")
    if local_end == -1:
        local_end = len(body)
    local_decls = body[: local_end + 1]
    rest = body[local_end + 1 :]

    sections = split_sections(rest)
    print(f"Split into {len(sections)} sections: {[n for n, _ in sections]}")

    helper_funcs = []
    runtime_calls = []
    for name, sec_body in sections:
        fn_name = f"register_{slugify(name)}"
        used = {
            p
            for p in ("ctx", "i8_ptr", "i32", "i64", "void")
            if re.search(r"\b" + p + r"\b", sec_body)
        }
        ctx_param = "ctx" if "ctx" in used else "_ctx"
        i8_ptr_param = "i8_ptr" if "i8_ptr" in used else "_i8_ptr"
        i32_param = "i32" if "i32" in used else "_i32"
        i64_param = "i64" if "i64" in used else "_i64"
        void_param = "void" if "void" in used else "_void"
        helper_funcs.append(
            f"fn {fn_name}<'ctx>(\n"
            f"    module: &Module<'ctx>,\n"
            f"    {ctx_param}: &'ctx Context,\n"
            f"    {i8_ptr_param}: inkwell::types::PointerType<'ctx>,\n"
            f"    {i32_param}: inkwell::types::IntType<'ctx>,\n"
            f"    {i64_param}: inkwell::types::IntType<'ctx>,\n"
            f"    {void_param}: inkwell::types::VoidType<'ctx>,\n"
            f") {{\n"
            f"{sec_body}"
            f"}}\n"
        )
        runtime_calls.append(f"    {fn_name}(module, ctx, i8_ptr, i32, i64, void);")

    new_runtime = (
        text[fn_start:body_start]
        + local_decls
        + "\n"
        + "\n".join(runtime_calls)
        + "\n}\n\n"
    )

    new_text = text[:fn_start] + new_runtime + "\n".join(helper_funcs) + "\n" + text[fn_end + 1 :]

    FILE.with_suffix(".rs.bak2").write_text(text)
    FILE.write_text(new_text)
    print(f"Wrote {FILE}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
