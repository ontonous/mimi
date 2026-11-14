#!/bin/bash
# ============================================================
# stdlib 矩阵双后端对拍 runner
# 用法: bash scripts/run-stdlib-matrix.sh --generated-dir <external-matrix> --scratch-dir <new-external-scratch> [singles|pairs|mega|traps|all]
# 每个 probe 在干净 scratch cwd 下跑 VM(mimi run) 与 native(mimi build+exec)，
# 对比 stdout + exit code；fs 类 probe 的产物按 backend 隔离清理。
# ============================================================
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(dirname "$SCRIPT_DIR")"
MIMI="${MIMI_BIN:-$REPO/target/debug/mimi}"
GEN_DIR="${MIMI_STDLIB_MATRIX_GENERATED_DIR:-}"
SCRATCH="${MIMI_STDLIB_MATRIX_SCRATCH_DIR:-}"
WHICH="all"
POSITIONAL_SEEN=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --generated-dir)
            [ "$#" -ge 2 ] || { echo "--generated-dir requires a path" >&2; exit 2; }
            GEN_DIR="$2"
            shift 2
            ;;
        --scratch-dir)
            [ "$#" -ge 2 ] || { echo "--scratch-dir requires a path" >&2; exit 2; }
            SCRATCH="$2"
            shift 2
            ;;
        singles|pairs|mega|traps|all)
            [ "$POSITIONAL_SEEN" -eq 0 ] || { echo "only one matrix group may be selected" >&2; exit 2; }
            WHICH="$1"
            POSITIONAL_SEEN=1
            shift
            ;;
        *)
            echo "usage: $0 --generated-dir <external-matrix> --scratch-dir <new-external-scratch> [singles|pairs|mega|traps|all]" >&2
            exit 2
            ;;
    esac
done

if [ -z "$GEN_DIR" ] || [ -z "$SCRATCH" ]; then
    echo "both --generated-dir and --scratch-dir are required; repository-local defaults are disabled" >&2
    exit 2
fi
if [ ! -x "$MIMI" ]; then
    echo "Mimi executable is not executable: $MIMI" >&2
    exit 2
fi
MIMI="$(realpath "$MIMI")" || {
    echo "Mimi executable path cannot be resolved: ${MIMI_BIN:-$REPO/target/debug/mimi}" >&2
    exit 2
}

if [ -L "$GEN_DIR" ]; then
    echo "generated directory must not be a symlink: $GEN_DIR" >&2
    exit 2
fi

GEN_DIR="$(cd "$GEN_DIR" 2>/dev/null && pwd -P)" || {
    echo "generated directory is unavailable: ${MIMI_STDLIB_MATRIX_GENERATED_DIR:-<argument>}" >&2
    exit 2
}
SCRATCH_PARENT="$(cd "$(dirname "$SCRATCH")" 2>/dev/null && pwd -P)" || {
    echo "scratch parent directory is unavailable: $(dirname "$SCRATCH")" >&2
    exit 2
}
SCRATCH="$SCRATCH_PARENT/$(basename "$SCRATCH")"
case "$GEN_DIR/" in "$REPO/"*) echo "generated directory must be outside the repository" >&2; exit 2 ;; esac
case "$SCRATCH/" in "$REPO/"*) echo "scratch directory must be outside the repository" >&2; exit 2 ;; esac
if [ "$GEN_DIR" = "$SCRATCH" ]; then
    echo "generated and scratch directories must be distinct" >&2
    exit 2
fi
if [ -e "$SCRATCH" ] || [ -L "$SCRATCH" ]; then
    echo "refusing to reuse existing scratch directory: $SCRATCH" >&2
    exit 2
fi
if ! python3 "$SCRIPT_DIR/stdlib_matrix_artifacts.py" verify \
    --root "$REPO" --generated-root "$GEN_DIR" > /dev/null; then
    echo "external generated matrix failed source-bound manifest verification" >&2
    exit 2
fi
mkdir "$SCRATCH"
python3 "$SCRIPT_DIR/stdlib_matrix_artifacts.py" verify \
    --root "$REPO" --generated-root "$GEN_DIR" \
    > "$SCRATCH/matrix-artifact-binding.json"
sha256sum "$MIMI" > "$SCRATCH/mimi-executable.sha256"
BUILD_LOG="$SCRATCH/matrix-build.log"
FAILURE_LOGS="$SCRATCH/build-failures"

PASS=0; FAIL=0; declare -a FAILED=()

run_probe() {
    local probe="$1"
    local rel="${probe#$GEN_DIR/}"
    local vm_out nv_out vm_rc nv_rc
    local work="$SCRATCH/work"
    # --- VM ---
    rm -rf "$work"; mkdir -p "$work"
    vm_out=$(cd "$work" && timeout 30 "$MIMI" run "$probe" 2>/dev/null </dev/null); vm_rc=$?
    # --- native ---
    rm -rf "$work"; mkdir -p "$work"
    local bin="$work/probe_bin"
    if (cd "$work" && timeout 120 "$MIMI" build "$probe" -o "$bin" > "$BUILD_LOG" 2>&1); then
        nv_out=$(cd "$work" && timeout 60 "$bin" 2>/dev/null </dev/null); nv_rc=$?
    else
        local build_rc=$?
        nv_out="<build-failed>"; nv_rc=$build_rc
        mkdir -p "$FAILURE_LOGS"
        cp "$BUILD_LOG" "$FAILURE_LOGS/$(basename "$probe").log" 2>/dev/null || true
    fi
    rm -rf "$work"
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
    *) echo "usage: $0 --generated-dir <external-matrix> --scratch-dir <new-external-scratch> [singles|pairs|mega|traps|all]"; exit 2 ;;
esac

echo "=================================="
echo "PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -gt 0 ]; then
    printf 'failed: %s\n' "${FAILED[@]}"
    exit 1
fi
