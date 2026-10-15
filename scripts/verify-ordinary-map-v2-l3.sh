#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"

if ! command -v valgrind >/dev/null 2>&1; then
    printf 'ordinary Map v2 L3 requires Valgrind\n' >&2
    exit 1
fi

if [ ! -d "$LLVM_SYS_181_PREFIX" ]; then
    printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi

source_snapshot_digest() {
    {
        find src tests std -type f -print0
        printf '%s\0' Cargo.toml Cargo.lock scripts/verify-ordinary-map-v2-l3.sh
        if [ -f build.rs ]; then
            printf '%s\0' build.rs
        fi
    } | sort -zu | xargs -0 sha256sum | sha256sum | awk '{print $1}'
}

cd "$ROOT_DIR"
snapshot_before="$(source_snapshot_digest)"
printf 'source_snapshot_sha256=%s\n' "$snapshot_before"
printf 'explicit_mir_census=not_emitted; L3 checks native output and heap status, not consumer receipt equality\n'
test_status=0
if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
    MIMI_REQUIRE_VALGRIND=1 \
    cargo test \
        --manifest-path "$ROOT_DIR/Cargo.toml" \
        --test canonical_mir_ordinary_map_any_owner_v2_memcheck_l3 \
        map_any_owner_v2_default_and_explicit_native_builds_are_strict_memcheck_clean \
        -- \
        --exact \
        --ignored \
        --nocapture \
        --test-threads=1; then
    test_status=0
else
    test_status=$?
fi
snapshot_after="$(source_snapshot_digest)"
printf 'source_snapshot_sha256_after=%s\n' "$snapshot_after"
if [ "$snapshot_before" != "$snapshot_after" ]; then
    printf 'source snapshot changed during the strict Map v2 L3 run\n' >&2
    exit 98
fi
printf 'acceptance_test_exit=%s\n' "$test_status"
exit "$test_status"
