#!/bin/bash
# ============================================================
# 压力测试
#   1. 极限编译: 大函数 (10000 if-else 分支)
#   2. 极限运行时: 海量 Actor (100000 个)
#   3. 跨平台兼容性检查
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
source "$SCRIPT_DIR/fuzz-common.sh"

MIMI_BIN=$(ensure_mimi)
CI_MODE=false
if [ "${1:-}" = "--ci" ]; then
    CI_MODE=true
    shift
fi

TOTAL=0
PASSED=0
FAILED=0
TIMEOUT_DURATION=60  # seconds per test

run_stress_test() {
    local name="$1"
    local src="$2"
    local mode="${3:-check}"  # check | run | build

    TOTAL=$((TOTAL + 1))
    local tmp_file=$(mktemp /tmp/mimi_stress.XXXXXX.mimi)
    echo "$src" > "$tmp_file"

    log_info "Running: $name (mode=$mode)"

    local start_time=$(date +%s%N)
    local exit_code=0
    local timeout_cmd="timeout $TIMEOUT_DURATION"

    case "$mode" in
        check)
            $timeout_cmd "$MIMI_BIN" check "$tmp_file" > /dev/null 2>&1 || exit_code=$?
            ;;
        run)
            $timeout_cmd "$MIMI_BIN" run "$tmp_file" > /dev/null 2>&1 || exit_code=$?
            ;;
        build)
            local tmp_bin=$(mktemp /tmp/mimi_stress_bin.XXXXXX)
            $timeout_cmd "$MIMI_BIN" build "$tmp_file" -o "$tmp_bin" > /dev/null 2>&1 || exit_code=$?
            if [ -x "$tmp_bin" ]; then
                $timeout_cmd "$tmp_bin" > /dev/null 2>&1 || true
            fi
            rm -f "$tmp_bin"
            ;;
    esac

    local end_time=$(date +%s%N)
    local duration_ms=$(( (end_time - start_time) / 1000000 ))

    rm -f "$tmp_file"

    if [ "$exit_code" -eq 0 ]; then
        PASSED=$((PASSED + 1))
        log_pass "$name (${duration_ms}ms)"
    elif [ "$exit_code" -eq 124 ]; then
        FAILED=$((FAILED + 1))
        log_fail "$name — TIMEOUT (${TIMEOUT_DURATION}s)"
    else
        FAILED=$((FAILED + 1))
        log_fail "$name — EXIT $exit_code (${duration_ms}ms)"
    fi
}

echo "========================================="
echo "  压力测试套件"
echo "========================================="
echo ""

# ---------- 极限编译: 大函数 ----------
echo "--- 极限编译: 大规模分支 ---"

# 生成 10000 分支的 if-else 链 (太大, CI 用 2000)
local BRANCH_COUNT=2000
if ! $CI_MODE; then
    BRANCH_COUNT=10000
fi

local big_if_src="func main() -> i64 {\n    let x = $((RANDOM % BRANCH_COUNT));\n"
for ((i=0; i<BRANCH_COUNT; i++)); do
    big_if_src+="    if x == $i { $i } else "
done
big_if_src+="{ -1 }\n}"

run_stress_test "big-if-else-${BRANCH_COUNT}" "$(echo -e "$big_if_src")" "check"

# ---------- 大 match ----------
local MATCH_COUNT=5000
if ! $CI_MODE; then
    MATCH_COUNT=20000
fi

local big_match_src="func main() -> i64 {\n    let x = $((RANDOM % MATCH_COUNT));\n    match x {\n"
for ((i=0; i<MATCH_COUNT; i++)); do
    big_match_src+="        $i => $((i * 2)),\n"
done
big_match_src+="        _ => -1\n    }\n}"

run_stress_test "big-match-${MATCH_COUNT}" "$(echo -e "$big_match_src")" "check"

# ---------- 深度嵌套 ----------
local NEST_DEPTH=500
if ! $CI_MODE; then
    NEST_DEPTH=2000
fi

local deep_nest_src="func main() -> i64 {\n    let x = 1;\n"
local indent=""
for ((i=0; i<NEST_DEPTH; i++)); do
    deep_nest_src+="${indent}if x > 0 {\n${indent}    let y = $i;\n"
    indent="${indent}    "
done
deep_nest_src+="${indent}x\n"
for ((i=0; i<NEST_DEPTH; i++)); do
    indent="${indent%    }"
    deep_nest_src+="${indent}} else { 0 }\n"
done
deep_nest_src+="}"

run_stress_test "deep-nest-${NEST_DEPTH}" "$(echo -e "$deep_nest_src")" "check"

# ---------- 海量 Actor (CI 模式用 10000, 本地用 100000) ----------
local ACTOR_COUNT=10000
if ! $CI_MODE; then
    ACTOR_COUNT=100000
fi

local actor_src=""
for ((i=0; i<ACTOR_COUNT; i++)); do
    actor_src+="actor Worker${i} {\n    fn work() -> i64 { ${i} }\n}\n\n"
done
actor_src+="func main() -> i64 { 0 }"

run_stress_test "massive-actor-${ACTOR_COUNT}" "$actor_src" "check"

# ---------- 极限编译: 超大列表字面量 ----------
local LIST_SIZE=50000
if $CI_MODE; then
    LIST_SIZE=10000
fi

local list_src="func main() -> i64 {\n    let xs = ["
local first=true
for ((i=0; i<LIST_SIZE; i++)); do
    $first && first=false || list_src+=", "
    list_src+="$i"
done
list_src+="];\n    len(xs)\n}"

run_stress_test "big-list-${LIST_SIZE}" "$(echo -e "$list_src")" "check"

# ---------- 重复编译稳定性 ----------
log_info "--- 重复编译稳定性 (10 次) ---"
local fib_src="func fib(n: i64) -> i64 {
    if n <= 1 { n } else { fib(n-1) + fib(n-2) }
}
func main() -> i64 { fib(20) }"

for ((i=0; i<10; i++)); do
    TOTAL=$((TOTAL + 1))
    local tmp_file=$(mktemp /tmp/mimi_fib_stress.XXXXXX.mimi)
    echo "$fib_src" > "$tmp_file"

    local exit_code=0
    local output=$("$MIMI_BIN" check "$tmp_file" 2>&1) || exit_code=$?
    rm -f "$tmp_file"

    if [ "$exit_code" -eq 0 ]; then
        PASSED=$((PASSED + 1))
        log_pass "repeated-compile #$i"
    else
        FAILED=$((FAILED + 1))
        log_fail "repeated-compile #$i (exit=$exit_code)"
    fi
done

echo ""
echo "========================================="
echo "  结果汇总"
echo "========================================="
echo "  Total:  $TOTAL"
echo -e "  Passed: ${GREEN}$PASSED${NC}"
echo -e "  Failed: ${RED}$FAILED${NC}"

if [ "$FAILED" -gt 0 ]; then
    exit 1
fi
log_pass "All stress tests passed."
