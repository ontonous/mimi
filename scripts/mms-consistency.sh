#!/bin/bash
# ============================================================
# 0.1.8: MimiSpec removal consistency guard
#
# Formerly ran a MimiSpec conformance oracle against an external
# OntomimiSE repo.  After Phase E removed `mimispec` from the
# repository, the guard now asserts that stale `.mms` / `mimispec`
# artifacts stay gone.
# ============================================================
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_DIR"

fail=0

mms_files="$(find . -type f -name '*.mms' \
    -not -path './.git/*' \
    -not -path './.llvm-wrapper/*' 2>/dev/null || true)"
if [ -n "$mms_files" ]; then
    echo "mms-consistency: unexpected .mms files after 0.1.8 removal:" >&2
    printf '%s\n' "$mms_files" >&2
    fail=1
fi

mimispec_dirs="$(find . -maxdepth 2 -type d -name 'mimispec' \
    -not -path './.git/*' -not -path './.llvm-wrapper/*' 2>/dev/null || true)"
if [ -n "$mimispec_dirs" ]; then
    echo "mms-consistency: unexpected mimispec directory after 0.1.8 removal:" >&2
    printf '%s\n' "$mimispec_dirs" >&2
    fail=1
fi

if [ -f readme/09-mms-integration.md ]; then
    echo "mms-consistency: readme/09-mms-integration.md must be removed" >&2
    fail=1
fi

if [ "$fail" -ne 0 ]; then
    exit 1
fi

echo "mms-consistency: MimiSpec is removed; no .mms/mimispec/readme-09 leftovers"
