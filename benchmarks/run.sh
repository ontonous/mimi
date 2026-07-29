#!/usr/bin/env bash
# Mimi Performance Benchmark Runner
# Compares Mimi (codegen + interpreter) vs C (gcc -O2) vs CPython
# Usage: bash benchmarks/run.sh [benchmark_name]
#
# Output: timing table + anomaly detection (>2x deviation from C baseline)

set -euo pipefail
cd "$(dirname "$0")/.."

BENCH_DIR="benchmarks"
MIMI_BIN="target/debug/mimi"
RUNS=3

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Ensure mimi is built
if [ ! -f "$MIMI_BIN" ]; then
    echo "Building mimi..."
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo build 2>/dev/null
fi

benchmarks=("$@")
if [ ${#benchmarks[@]} -eq 0 ]; then
    benchmarks=($(ls "$BENCH_DIR"/*.mimi | xargs -I{} basename {} .mimi))
fi

echo "=========================================="
echo " Mimi Performance Benchmark"
echo " $(date)"
echo "=========================================="
echo ""

anomalies=()

for name in "${benchmarks[@]}"; do
    mimi_src="$BENCH_DIR/$name.mimi"
    c_src="$BENCH_DIR/$name.c"
    py_src="$BENCH_DIR/$name.py"

    if [ ! -f "$mimi_src" ]; then
        echo "SKIP: $name (no .mimi source)"
        continue
    fi

    echo "--- $name ---"

    # 1. Mimi codegen (compiled binary)
    mimi_bin="/tmp/mimi_bench_$name"
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper "$MIMI_BIN" build "$mimi_src" -o "$mimi_bin" 2>/dev/null || true
    if [ -f "$mimi_bin" ]; then
        mimi_times=()
        for i in $(seq 1 $RUNS); do
            t=$( { /usr/bin/time -f "%e" "$mimi_bin" > /dev/null; } 2>&1 )
            mimi_times+=("$t")
        done
        mimi_avg=$(echo "${mimi_times[@]}" | tr ' ' '\n' | awk '{s+=$1}END{printf "%.3f", s/NR}')
        echo "  Mimi codegen:  ${mimi_avg}s  [${mimi_times[*]}]"
    else
        mimi_avg="N/A"
        echo "  Mimi codegen:  BUILD FAILED"
    fi

    # 2. Mimi interpreter (with 30s timeout)
    interp_times=()
    for i in $(seq 1 $RUNS); do
        t=$( { /usr/bin/time -f "%e" timeout 30 "$MIMI_BIN" run "$mimi_src" > /dev/null; } 2>&1 ) || t="TIMEOUT"
        interp_times+=("$t")
    done
    if [[ "${interp_times[0]}" == "TIMEOUT" ]]; then
        interp_avg="TIMEOUT"
        echo "  Mimi interp:   TIMEOUT (>30s)"
    else
        interp_avg=$(echo "${interp_times[@]}" | tr ' ' '\n' | awk '{s+=$1}END{printf "%.3f", s/NR}')
        echo "  Mimi interp:   ${interp_avg}s  [${interp_times[*]}]"
    fi

    # 3. C (gcc -O2)
    if [ -f "$c_src" ]; then
        c_bin="/tmp/c_bench_$name"
        gcc -O2 -o "$c_bin" "$c_src" -lm 2>/dev/null
        c_times=()
        for i in $(seq 1 $RUNS); do
            t=$( { /usr/bin/time -f "%e" "$c_bin" > /dev/null; } 2>&1 )
            c_times+=("$t")
        done
        c_avg=$(echo "${c_times[@]}" | tr ' ' '\n' | awk '{s+=$1}END{printf "%.3f", s/NR}')
        echo "  C (gcc -O2):   ${c_avg}s  [${c_times[*]}]"
    else
        c_avg="N/A"
        echo "  C (gcc -O2):   no source"
    fi

    # 4. CPython
    if [ -f "$py_src" ]; then
        py_times=()
        for i in $(seq 1 $RUNS); do
            t=$( { /usr/bin/time -f "%e" python3 "$py_src" > /dev/null; } 2>&1 )
            py_times+=("$t")
        done
        py_avg=$(echo "${py_times[@]}" | tr ' ' '\n' | awk '{s+=$1}END{printf "%.3f", s/NR}')
        echo "  CPython:       ${py_avg}s  [${py_times[*]}]"
    else
        py_avg="N/A"
        echo "  CPython:       no source"
    fi

    # Anomaly detection: Mimi codegen vs C
    if [ "$mimi_avg" != "N/A" ] && [ "$c_avg" != "N/A" ]; then
        ratio=$(echo "$mimi_avg $c_avg" | awk '{if ($2 > 0) printf "%.1f", $1/$2; else print "inf"}')
        if (( $(echo "$ratio > 2.0" | bc -l 2>/dev/null || echo 0) )); then
            echo -e "  ${RED}ANOMALY: Mimi/C = ${ratio}x (>2x)${NC}"
            anomalies+=("$name: Mimi/C = ${ratio}x")
        elif (( $(echo "$ratio > 1.5" | bc -l 2>/dev/null || echo 0) )); then
            echo -e "  ${YELLOW}WARNING: Mimi/C = ${ratio}x (>1.5x)${NC}"
        else
            echo -e "  ${GREEN}OK: Mimi/C = ${ratio}x${NC}"
        fi
    fi

    # Anomaly detection: Mimi interp vs CPython
    if [ "$interp_avg" != "N/A" ] && [ "$py_avg" != "N/A" ]; then
        ratio=$(echo "$interp_avg $py_avg" | awk '{if ($2 > 0) printf "%.1f", $1/$2; else print "inf"}')
        if (( $(echo "$ratio > 2.0" | bc -l 2>/dev/null || echo 0) )); then
            echo -e "  ${RED}ANOMALY: Interp/CPython = ${ratio}x (>2x)${NC}"
            anomalies+=("$name: Interp/CPython = ${ratio}x")
        else
            echo -e "  ${GREEN}OK: Interp/CPython = ${ratio}x${NC}"
        fi
    fi

    echo ""
done

echo "=========================================="
if [ ${#anomalies[@]} -gt 0 ]; then
    echo -e "${RED}ANOMALIES DETECTED (${#anomalies[@]}):${NC}"
    for a in "${anomalies[@]}"; do
        echo "  - $a"
    done
else
    echo -e "${GREEN}No anomalies detected.${NC}"
fi
echo "=========================================="
