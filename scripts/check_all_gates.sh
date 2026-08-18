#!/usr/bin/env bash
# 0.1.7 full gate replay.
#
# Runs the standard DoD gates that can be completed in a CI job:
#   - fmt / clippy / lib tests
#   - dispatcher zero-fallback
#   - stress smoke + heavy
#   - dogfood projects
#   - real-world CLI suite
#   - bin CLI tests
#
# Usage: scripts/check_all_gates.sh
set -euo pipefail

LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-$PWD/.llvm-wrapper}"
export LLVM_SYS_181_PREFIX

echo "== fmt =="
cargo fmt -- --check

echo "== clippy =="
cargo clippy --all-targets -- -D warnings

echo "== cargo test --lib =="
cargo test --lib

echo "== dispatch zero =="
python3 scripts/dispatch_stat.py check --zero

echo "== stress smoke =="
make test-stress

echo "== stress heavy =="
make test-stress-heavy

echo "== dogfood =="
make test-dogfood

echo "== bin CLI tests =="
cargo test --bin mimi -- --test-threads=1

echo "== real-world CLI =="
cargo test --test real_world_cli -- --test-threads=1

echo "ALL GATES PASSED"
