#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EVIDENCE_ROOT="${MIMI_DIRECT_TUPLE_GET_L3_ARTIFACT_DIR:?set to a new external evidence directory}"
LLVM_WRAPPER="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
CARGO_TARGET="${CARGO_TARGET_DIR:-/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/build-targets/current}"

if [[ -e "$EVIDENCE_ROOT" ]]; then
    printf 'refusing to reuse existing evidence directory: %s\n' "$EVIDENCE_ROOT" >&2
    exit 2
fi
mkdir -p "$EVIDENCE_ROOT"

cd "$REPO_ROOT"
printf '%s\n' \
  'LLVM_SYS_181_PREFIX=<recorded below>' \
  'CARGO_BUILD_JOBS=1' \
  'CARGO_TARGET_DIR=<recorded below>' \
  'MIMI_REQUIRE_VALGRIND=1' \
  'MIMI_DIRECT_TUPLE_GET_L3_ARTIFACT_DIR=<EVIDENCE_ROOT>' \
  'cargo test --locked --test canonical_mir_direct_tuple_get_memcheck_l3 direct_tuple_get_default_and_explicit_native_builds_are_strict_memcheck_clean -- --exact --ignored --nocapture --test-threads=1' \
  > "$EVIDENCE_ROOT/command.txt"
printf 'LLVM_SYS_181_PREFIX=%s\nCARGO_BUILD_JOBS=1\nCARGO_TARGET_DIR=%s\nMIMI_REQUIRE_VALGRIND=1\n' \
  "$LLVM_WRAPPER" "$CARGO_TARGET" > "$EVIDENCE_ROOT/environment.txt"
git rev-parse HEAD > "$EVIDENCE_ROOT/head.txt"
sha256sum tests/canonical_mir_direct_tuple_get_memcheck_l3.rs tests/fixtures/mir_ordinary_map_owned_any_v2_direct_tuple_get_l1.mimi scripts/verify-ordinary-map-direct-tuple-get-l3.sh \
  > "$EVIDENCE_ROOT/source-before.sha256"
rustc --version --verbose > "$EVIDENCE_ROOT/rustc-version.txt"
"$LLVM_WRAPPER/bin/llvm-config" --version > "$EVIDENCE_ROOT/llvm-version.txt"
valgrind --version > "$EVIDENCE_ROOT/valgrind-version.txt"

set +e
LLVM_SYS_181_PREFIX="$LLVM_WRAPPER" \
CARGO_BUILD_JOBS=1 \
CARGO_TARGET_DIR="$CARGO_TARGET" \
MIMI_REQUIRE_VALGRIND=1 \
MIMI_DIRECT_TUPLE_GET_L3_ARTIFACT_DIR="$EVIDENCE_ROOT" \
cargo test --locked --test canonical_mir_direct_tuple_get_memcheck_l3 \
  direct_tuple_get_default_and_explicit_native_builds_are_strict_memcheck_clean \
  -- --exact --ignored --nocapture --test-threads=1 \
  > "$EVIDENCE_ROOT/cargo-test.log" 2>&1
STATUS=$?
set -e
printf '%s\n' "$STATUS" > "$EVIDENCE_ROOT/cargo-test.exit-status"
sha256sum tests/canonical_mir_direct_tuple_get_memcheck_l3.rs tests/fixtures/mir_ordinary_map_owned_any_v2_direct_tuple_get_l1.mimi scripts/verify-ordinary-map-direct-tuple-get-l3.sh \
  > "$EVIDENCE_ROOT/source-after.sha256"
diff -u "$EVIDENCE_ROOT/source-before.sha256" "$EVIDENCE_ROOT/source-after.sha256" \
  > "$EVIDENCE_ROOT/source-hash-comparison.txt"
find "$EVIDENCE_ROOT" -type f ! -name SHA256SUMS -print0 \
  | sort -z \
  | xargs -0 sha256sum > "$EVIDENCE_ROOT/SHA256SUMS"

printf 'strict DirectTupleGet L3 exit=%s evidence=%s\n' "$STATUS" "$EVIDENCE_ROOT"
exit "$STATUS"
