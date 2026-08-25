#!/bin/bash
# ============================================================
# stdlib 矩阵双后端对拍 runner
# 用法: bash scripts/run-stdlib-matrix.sh [singles|pairs|mega|traps|all]
# 每个 probe 在干净 scratch cwd 下跑 VM(mimi run) 与 native(mimi build+exec)，
# 对比 stdout + exit code；fs 类 probe 的产物按 backend 隔离清理。
# ============================================================
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(dirname "$SCRIPT_DIR")"
MIMI="${MIMI_BIN:-$REPO/target/debug/mimi}"
GEN_DIR="$REPO/tests/stdlib_matrix_generated"
SCRATCH="$REPO/tests/stdlib_matrix_scratch"
WHICH="${1:-all}"

rm -rf "$SCRATCH"
mkdir -p "$SCRATCH"

PASS=0; FAIL=0; declare -a FAILED=()

run_probe() {
    local probe="$1"
    local rel="${probe#$GEN_DIR/}"
    local vm_out nv_out vm_rc nv_rc
    # --- VM ---
    rm -rf "$SCRATCH"; mkdir -p "$SCRATCH"
    vm_out=$(cd "$SCRATCH" && timeout 30 "$MIMI" run "$probe" 2>/dev/null </dev/null); vm_rc=$?
    # --- native ---
    rm -rf "$SCRATCH"; mkdir -p "$SCRATCH"
    local bin="$SCRATCH/probe_bin"
    if (cd "$SCRATCH" && timeout 120 "$MIMI" build "$probe" -o "$bin" > /tmp/mm_matrix_build.log 2>&1); then
        nv_out=$(cd "$SCRATCH" && timeout 60 "$bin" 2>/dev/null </dev/null); nv_rc=$?
    else
        nv_out="<build-failed>"; nv_rc=$?
        # Keep the failing build's stderr for post-mortem (last failure wins).
        cp /tmp/mm_matrix_build.log "/tmp/mm_matrix_build_fail_$(basename "$probe").log" 2>/dev/null || true
    fi
    rm -rf "$SCRATCH"; mkdir -p "$SCRATCH"
    if [ "$vm_out" == "$nv_out" ] && [ "$vm_rc" == "$nv_rc" ]; then
        PASS=$((PASS+1))
        [ -n "${VERBOSE:-}" ] && echo "OK   $rel"
    else
        FAIL=$((FAIL+1))
        echo "DIFF $rel  vm_rc=$vm_rc nv_rc=$nv_rc"
        diff <(printf '%s\n' "$vm_out") <(printf '%s\n' "$nv_out") | head -8 | sed 's/^/     /'
        FAILED+=("$rel")
    fi
}

shopt -s nullglob
case "$WHICH" in
    singles) for p in "$GEN_DIR"/singles/*.mimi; do run_probe "$p"; done ;;
    pairs)   for p in "$GEN_DIR"/pairs/*.mimi; do run_probe "$p"; done ;;
    mega)    run_probe "$GEN_DIR/mega.mimi" ;;
    traps)   for p in "$GEN_DIR"/traps/*.mimi; do run_probe "$p"; done ;;
    all)
        for p in "$GEN_DIR"/singles/*.mimi; do run_probe "$p"; done
        for p in "$GEN_DIR"/traps/*.mimi; do run_probe "$p"; done
        run_probe "$GEN_DIR/mega.mimi"
        for p in "$GEN_DIR"/pairs/*.mimi; do run_probe "$p"; done
        ;;
    *) echo "usage: $0 [singles|pairs|mega|traps|all]"; exit 2 ;;
esac

echo "=================================="
echo "PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -gt 0 ]; then
    printf 'failed: %s\n' "${FAILED[@]}"
    exit 1
fi
