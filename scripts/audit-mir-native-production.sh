#!/usr/bin/env bash
# Canonical MIR native production boundary audit.
#
# The native adapter must fail through NativeMirError before LLVM materializes
# malformed MIR.  Test fixtures may use expect/panic assertions, but the
# production portion of the adapter must not contain those escape hatches or
# direct collection indexing.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
TARGET_DIR="$PROJECT_DIR/src/codegen/mir"

printf 'schema=canonical-mir-native-production-audit-v1\n'
printf 'root=%s\n' "$PROJECT_DIR"

TARGET_DIR="$TARGET_DIR" python3 - <<'PY'
import os
import re
import sys
from pathlib import Path

root = Path(os.environ["TARGET_DIR"])
issues = []

patterns = (
    ("panic_macro", re.compile(r"\b(?:unreachable|panic|todo)!\s*\(")),
    ("panic_method", re.compile(r"\.(?:expect|unwrap)\s*\(")),
    # Keep this intentionally narrow: slices in signatures and array literals
    # are valid, while `values[index]` in production is the unchecked edge.
    (
        "direct_index",
        re.compile(
            r"\b(?:self(?:\.[A-Za-z_][A-Za-z0-9_]*)*|"
            r"[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)\s*\[[^]]+\]"
        ),
    ),
)

for path in sorted(root.glob("*.rs")):
    in_tests = False
    for line_number, line in enumerate(path.read_text().splitlines(), 1):
        if line.strip() == "#[cfg(test)]":
            in_tests = True
        if in_tests:
            continue
        for category, pattern in patterns:
            if pattern.search(line):
                issues.append((category, path, line_number, line.strip()))

if issues:
    for category, path, line_number, line in issues:
        print(f"native_production_issue={category} file={path} line={line_number} text={line}")
    print(f"native_production_issue_count={len(issues)}")
    sys.exit(1)

print("native_production_issue_count=0")
print("native_production_audit_status=ok")
PY
