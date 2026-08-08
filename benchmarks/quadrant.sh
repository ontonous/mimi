#!/usr/bin/env bash
# Mimi 0.1.5 Performance Quadrant Matrix (devdocs/v0.35/README.md Phase A)
# Matrix: benchmark × {MIMI_OPT=0, MIMI_OPT=1} × {default, ieee_float}
# Plus: trap call-site static count (MIMI_DUMP_MODULE IR dump + grep).
# Usage: bash benchmarks/quadrant.sh            # full matrix
#        bash benchmarks/quadrant.sh fib        # single benchmark
#
# Output: timing table (seconds, RUNS median) + x/C -O2 ratio + trap counts.

set -uo pipefail
cd "$(dirname "$0")/.."

BENCH_DIR="benchmarks"
MIMI_BIN="target/debug/mimi"
RUNS=3
C_FLAGS="-O2"

# Colors
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'

if [ ! -f "$MIMI_BIN" ]; then
    echo "Building mimi..."
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo build 2>/dev/null
fi

# benchmark list: default file + optional ieee variant
declare -A IEEE_VARIANT=( [mandelbrot]=mandelbrot_ieee [dsp]=dsp_ieee )

names=("$@")
if [ ${#names[@]} -eq 0 ]; then
    names=(fib mandelbrot dsp)
fi

median_ms() {
    # $@ = raw durations (nanoseconds, one per line) -> median in ms (1 dp)
    printf '%s\n' "$@" | sort -n | awk '{a[NR]=$1} END {if (NR%2) m=a[(NR+1)/2]; else m=(a[NR/2]+a[NR/2+1])/2; printf "%.1f", m/1000000}'
}

run_cmd_ns() {
    # $@ = command; prints wall time in nanoseconds (date +%s%N bracketing)
    local s e
    s=$(date +%s%N)
    "$@" >/dev/null 2>&1
    e=$(date +%s%N)
    echo $((e - s))
}

trap_count() {
    # $1 = .mimi source; -> trap call sites in pre-opt IR (O1 only: dump
    # hook runs inside the optimize branch; trap EMISSION is opt-level
    # independent, so the O1 count is representative for both quadrants).
    local src="$1" dump="/tmp/mimi_trap_dump_$$.ll"
    rm -f "$dump"
    MIMI_OPT=1 MIMI_DUMP_MODULE="$dump" LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper \
        "$MIMI_BIN" build "$src" -o /tmp/mimi_trap_cnt_$$ >/dev/null 2>&1 || { echo "?"; rm -f "$dump"; return; }
    if [ -f "$dump" ]; then
        grep -c "mimi_trap" "$dump" || echo 0
        rm -f "$dump"
    else
        echo "?"
    fi
}

run_mimi() {
    # $1 = src; $2 = MIMI_OPT; prints "median_ms ratio"
    local src="$1" opt="$2" bin="/tmp/mimi_q_$$" times=() t
    MIMI_OPT="$opt" LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper \
        "$MIMI_BIN" build "$src" -o "$bin" >/dev/null 2>&1 || { echo "BUILD_FAIL -"; return; }
    for _ in $(seq 1 "$RUNS"); do
        t=$(run_cmd_ns timeout 60 "$bin") || t="TIMEOUT"
        times+=("$t")
    done
    times=($(printf '%s\n' "${times[@]}" | grep -v TIMEOUT))
    [ ${#times[@]} -eq 0 ] && { echo "TIMEOUT -"; return; }
    local ms c_ms ratio
    ms=$(median_ms "${times[@]}")
    c_ms=$(run_c_ms "$name")
    ratio=$(awk -v m="$ms" -v c="$c_ms" 'BEGIN {if (c>0) printf "%.2f", m/c; else print "-"}')
    echo "${ms} ${ratio}"
}

run_c_ms() {
    # $1 = benchmark name; prints C -O2 median ms
    local c_src="$BENCH_DIR/$name.c"
    if [ ! -f "$c_src" ]; then echo "-"; return; fi
    local c_bin="/tmp/c_q_$$" c_times=() t
    gcc $C_FLAGS -o "$c_bin" "$c_src" -lm 2>/dev/null || { echo "-"; return; }
    for _ in $(seq 1 "$RUNS"); do
        t=$(run_cmd_ns "$c_bin")
        c_times+=("$t")
    done
    median_ms "${c_times[@]}"
}

echo "=================================================="
echo " Mimi 0.1.5 Performance Quadrant Matrix"
echo " $(date)"
echo " RUNS=$RUNS median, ratios vs C gcc -O2"
echo "=================================================="

for name in "${names[@]}"; do
    src="$BENCH_DIR/$name.mimi"
    [ -f "$src" ] || { echo "SKIP: $name (no .mimi)"; continue; }
    ieee="${IEEE_VARIANT[$name]:-}"

    echo ""
    echo "--- $name ---"
    c_ms=$(run_c_ms "$name")
    [ "$c_ms" != "-" ] && echo "  C -O2:            ${c_ms} ms (baseline)"

    # O1 quadrant
    read -r ms ratio <<< "$(run_mimi "$src" 1)"
    traps=$(trap_count "$src")
    echo "  O1 default:      ${ms} ms   xC=${ratio}   traps=${traps}"
    if [ -n "$ieee" ] && [ -f "$BENCH_DIR/$ieee.mimi" ]; then
        read -r ms ratio <<< "$(run_mimi "$BENCH_DIR/$ieee.mimi" 1)"
        traps=$(trap_count "$BENCH_DIR/$ieee.mimi")
        echo "  O1 + ieee_float: ${ms} ms   xC=${ratio}   traps=${traps}"
    fi
    # O0 quadrant (trap emission identical to O1 — same count)
    read -r ms ratio <<< "$(run_mimi "$src" 0)"
    echo "  O0 default:      ${ms} ms   xC=${ratio}   traps=${traps}"
    if [ -n "$ieee" ] && [ -f "$BENCH_DIR/$ieee.mimi" ]; then
        read -r ms ratio <<< "$(run_mimi "$BENCH_DIR/$ieee.mimi" 0)"
        echo "  O0 + ieee_float: ${ms} ms   xC=${ratio}   traps=${traps}"
    fi
done

echo ""
echo "=================================================="
echo " trap count = static mimi_trap* call sites in pre-optimization IR"
echo " (MIMI_DUMP_MODULE dump, O1 pipeline; emission is opt-independent)"
echo "=================================================="