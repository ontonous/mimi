#!/usr/bin/env bash
# Mimi performance gate (0.35.46 — C2 教训闭环：非性能 sprint 不得绕过矩阵)。
#
# 与交互式 benchmarks/quadrant.sh 不同：本脚本是 CI 门禁，用 release 构建 +
# 宽松阈值（只拦截灾难性回退，不拦截共享 runner 噪声）。本地复测细粒度回退
# 仍用 `bash benchmarks/quadrant.sh`。
#
# 用法：
#   bash scripts/perf-gate.sh             # 默认 dsp（最敏感基准）
#   PERF_GATE_THRESHOLD=3.0 bash scripts/perf-gate.sh
#   PERF_GATE_VM_CEILING=20 bash scripts/perf-gate.sh
#
# 门禁断言（任一失败即 exit 1）：
#   1. native dsp O1 相对 C -O2 的比值 ≤ PERF_GATE_THRESHOLD（默认 3.0×）；
#   2. VM `mimi run` dsp ≤ PERF_GATE_VM_CEILING 秒（默认 20s，只拦截灾难性 VM 回退）；
#   3. 构建/运行不得失败。

set -uo pipefail
cd "$(dirname "$0")/.."

BENCH="dsp"
THRESHOLD="${PERF_GATE_THRESHOLD:-3.0}"
VM_CEILING="${PERF_GATE_VM_CEILING:-20}"
RUNS=3
BIN="target/release/mimi"
C_FLAGS="-O2"

fail() { echo "PERF-GATE FAIL: $*" >&2; exit 1; }

if [ ! -f "$BIN" ]; then
    echo "PERF-GATE: building release…"
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo build --release >/dev/null 2>&1 \
        || fail "release build failed"
fi

# ── C -O2 baseline ──────────────────────────────────────────────
C_SRC="benchmarks/${BENCH}.c"
C_BIN="/tmp/perf_gate_c_$$"
if [ ! -f "$C_SRC" ]; then fail "missing ${C_SRC}"; fi
gcc $C_FLAGS -o "$C_BIN" "$C_SRC" -lm >/dev/null 2>&1 || fail "gcc build failed"
c_times=()
for _ in $(seq 1 "$RUNS"); do
    s=$(date +%s%N); timeout 60 "$C_BIN" >/dev/null 2>&1 || fail "C baseline run failed"
    e=$(date +%s%N); c_times+=("$((e - s))")
done
c_ns=$(printf '%s\n' "${c_times[@]}" | sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}')

# ── native dsp O1 ───────────────────────────────────────────────
MIMI_SRC="benchmarks/${BENCH}.mimi"
MIMI_BIN="/tmp/perf_gate_mimi_$$"
MIMI_OPT=1 LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper "$BIN" build "$MIMI_SRC" -o "$MIMI_BIN" >/dev/null 2>&1 \
    || fail "native build failed"
m_times=()
for _ in $(seq 1 "$RUNS"); do
    s=$(date +%s%N); timeout 120 "$MIMI_BIN" >/dev/null 2>&1 || fail "native run failed"
    e=$(date +%s%N); m_times+=("$((e - s))")
done
m_ns=$(printf '%s\n' "${m_times[@]}" | sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}')

ratio=$(awk -v m="$m_ns" -v c="$c_ns" 'BEGIN {if (c>0) printf "%.2f", m/c; else print "inf"}')
echo "PERF-GATE: ${BENCH} native O1 = ${ratio}x C -O2 (threshold ${THRESHOLD}x)"
awk -v r="$ratio" -v t="$THRESHOLD" 'BEGIN {exit !(r+0 <= t+0)}' \
    || fail "${BENCH} native ratio ${ratio}x exceeds threshold ${THRESHOLD}x"

# ── VM dsp ──────────────────────────────────────────────────────
s=$(date +%s%N)
timeout "$((VM_CEILING * 2))" "$BIN" run "$MIMI_SRC" >/dev/null 2>&1 || fail "VM run failed"
e=$(date +%s%N)
vm_s=$(awk -v ns="$((e - s))" 'BEGIN {printf "%.1f", ns/1000000000}')
echo "PERF-GATE: ${BENCH} VM run = ${vm_s}s (ceiling ${VM_CEILING}s)"
awk -v v="$vm_s" -v c="$VM_CEILING" 'BEGIN {exit !(v+0 <= c+0)}' \
    || fail "${BENCH} VM ${vm_s}s exceeds ceiling ${VM_CEILING}s"

rm -f "$C_BIN" "$MIMI_BIN"
echo "PERF-GATE: PASS"
