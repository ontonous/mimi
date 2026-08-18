#!/usr/bin/env bash
# 0.1.7 nightly sanitizer replay script.
#
# Usage:
#   scripts/sanitize_ci.sh asan   # AddressSanitizer replay
#   scripts/sanitize_ci.sh tsan   # ThreadSanitizer replay
#   scripts/sanitize_ci.sh both   # both (default)
#
# Requires rustup nightly with rust-src installed:
#   rustup toolchain install nightly --component rust-src
set -euo pipefail

LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-$PWD/.llvm-wrapper}"
export LLVM_SYS_181_PREFIX
TARGET=x86_64-unknown-linux-gnu
THREADS=1

run_asan_group() {
  local filter="$1"
  RUSTFLAGS="-Z sanitizer=address" \
  ASAN_OPTIONS="detect_leaks=1:halt_on_error=1" \
  cargo +nightly test -Z build-std --target "$TARGET" --lib "$filter" -- --test-threads="$THREADS"
}

run_tsan_group() {
  local filter="$1"
  RUSTFLAGS="-Z sanitizer=thread" \
  TSAN_OPTIONS="halt_on_error=1" \
  cargo +nightly test -Z build-std --target "$TARGET" --lib "$filter" -- --test-threads="$THREADS"
}

asan() {
  echo "== ASan: lexer =="
  run_asan_group 'lexer::flow::tests::test_flow_lexer_comments'
  echo "== ASan: parser fuzz =="
  run_asan_group 'tests::fuzz::target_parser::'
  echo "== ASan: property suite =="
  run_asan_group 'tests::property::'
}

tsan() {
  echo "== TSan: runtime future =="
  run_tsan_group 'runtime::future::tests::'
  echo "== TSan: FFI runtime =="
  run_tsan_group 'ffi::runtime::tests::'
  echo "== TSan: actor concurrent =="
  run_tsan_group 'tests::actor_concurrent::'
}

case "${1:-both}" in
  asan) asan ;;
  tsan) tsan ;;
  both) asan; tsan ;;
  *) echo "usage: $0 [asan|tsan|both]" >&2; exit 2 ;;
esac
