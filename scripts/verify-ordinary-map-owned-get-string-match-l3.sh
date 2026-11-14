#!/usr/bin/env bash
set -euo pipefail

script_path="${BASH_SOURCE[0]}"
script_dir="${script_path%/*}"
[[ "$script_dir" != "$script_path" ]] || script_dir=.
root_dir="$(cd -- "$script_dir/.." && pwd -P)"
cd "$root_dir"

if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper
fi
export LLVM_SYS_181_PREFIX
export MIMI_REQUIRE_VALGRIND=1

if [[ ! -v MIMI_MAP_STRING_MATCH_L3_EVIDENCE_DIR || -z "$MIMI_MAP_STRING_MATCH_L3_EVIDENCE_DIR" ]]; then
    MIMI_MAP_STRING_MATCH_L3_EVIDENCE_DIR="${TMPDIR:-/tmp}/mimi-owned-get-string-match-l3-$$"
fi
if [[ -e "$MIMI_MAP_STRING_MATCH_L3_EVIDENCE_DIR" || -L "$MIMI_MAP_STRING_MATCH_L3_EVIDENCE_DIR" ]]; then
    printf 'refusing to overwrite strict L3 evidence directory: %s\n' "$MIMI_MAP_STRING_MATCH_L3_EVIDENCE_DIR" >&2
    exit 2
fi
export MIMI_MAP_STRING_MATCH_L3_EVIDENCE_DIR

cargo test --test canonical_mir_ordinary_map_owned_get_string_match_memcheck_l3 \
    -- --ignored --exact \
    ordinary_map_owned_get_string_match_default_and_explicit_native_builds_are_strict_memcheck_clean \
    --nocapture
