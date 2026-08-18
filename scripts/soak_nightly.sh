#!/usr/bin/env bash
# 0.1.7 nightly 24h soak runner.
#
# Runs the native memory-stability soak gate for a full day by default.
# The test samples VmRSS every 500ms and asserts bounded peak growth.
#
# Usage:
#   scripts/soak_nightly.sh                 # 24h (86400s)
#   MIMI_SOAK_SECONDS=900 scripts/soak_nightly.sh   # 15min short soak
set -euo pipefail

LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-$PWD/.llvm-wrapper}"
export LLVM_SYS_181_PREFIX
DURATION_SECS="${MIMI_SOAK_SECONDS:-86400}"

mkdir -p devdocs
LOG="devdocs/soak-${DURATION_SECS}s-$(date +%Y%m%d-%H%M%S).log"
echo "soak start duration_secs=${DURATION_SECS} log=${LOG}"

# The test itself reads MIMI_SOAK_SECONDS from the environment.
MIMI_SOAK_SECONDS="$DURATION_SECS" \
  cargo test --test stress stress_soak_native_memory_stability_heavy \
    -- --ignored --nocapture 2>&1 | tee "$LOG"
