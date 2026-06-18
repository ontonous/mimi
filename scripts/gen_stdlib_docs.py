#!/usr/bin/env python3
"""Generate STDLIB_API.md from Mimi standard library .mimi files."""

import os
import re

STDLIB_DIR = os.path.join(os.path.dirname(__file__), '..', 'std')
HEADER = """# Mimi Standard Library API Reference

> Auto-generated from `mimi/std/*.mimi`. Do not edit manually.

"""


def extract_functions(filepath):
    """Extract (doc_comment, signature) pairs from a .mimi file.

    Doc comments are consecutive // lines immediately above a pub func/const,
    separated from preceding comments by a blank line.
    """
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()

    functions = []
    lines = content.split('\n')
    i = 0
    while i < len(lines):
        line = lines[i]
        is_func = line.startswith('pub func ')
        is_const = line.startswith('pub const ')
        if is_func or is_const:
            sig = line
            doc_parts = []
            # Walk up, stopping at blank line or non-comment
            j = i - 1
            while j >= 0:
                stripped = lines[j].strip()
                if stripped.startswith('// '):
                    doc_parts.insert(0, stripped[3:].strip())
                elif stripped.startswith('//'):
                    doc_parts.insert(0, stripped[2:].strip())
                elif stripped == '':
                    break  # blank line = boundary between comment blocks
                else:
                    break  # non-comment, non-blank = stop
                j -= 1
            doc = ' '.join(doc_parts)
            functions.append((doc.strip(), sig.strip(), is_const))
        i += 1

    return functions


def main():
    modules = {}
    for fname in sorted(os.listdir(STDLIB_DIR)):
        if not fname.endswith('.mimi'):
            continue
        path = os.path.join(STDLIB_DIR, fname)
        name = fname[:-5]
        functions = extract_functions(path)
        if functions:
            modules[name] = functions

    output = [HEADER]

    total_funcs = 0
    for mod_name in sorted(modules.keys()):
        funcs = modules[mod_name]
        total_funcs += len(funcs)
        output.append(f'\n## `{mod_name}` ({len(funcs)})\n')

        for doc, sig, is_const in funcs:
            clean_sig = re.sub(r'\s*\{.*', '', sig).strip()
            entry = f'- `{clean_sig}`'
            if doc:
                entry += f' — {doc}'
            output.append(entry)

    output.insert(1, f'> **{total_funcs} public functions + constants across {len(modules)} modules.**\n')
    output.append('')

    out_path = os.path.join(
        os.path.dirname(__file__), '..', '..', 'mimispecref', 'stdlib_api.md'
    )
    out_dir = os.path.dirname(out_path)
    os.makedirs(out_dir, exist_ok=True)

    with open(out_path, 'w', encoding='utf-8') as f:
        f.write('\n'.join(output) + '\n')

    print(f'Wrote {total_funcs} items from {len(modules)} modules to {out_path}')


if __name__ == '__main__':
    main()
