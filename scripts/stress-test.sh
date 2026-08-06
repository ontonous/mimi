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

# wave1-review §5.11 (closed 2026-08-07): the parser's 128-level recursion
# guard (parser/helpers.rs DEPTH_MAX_DEFAULT) is a STACK-SAFETY boundary —
# stress inputs beyond it must fail LOUD with the recursion diagnostic,
# never pass and never SIGSEGV. This helper asserts exactly that contract.
run_stress_exceed_cap() {
    local name="$1"
    local src="$2"
    local needle="${3:-recursion limit exceeded}"

    TOTAL=$((TOTAL + 1))
    local tmp_file=$(mktemp /tmp/mimi_stress.XXXXXX.mimi)
    echo "$src" > "$tmp_file"

    log_info "Running: $name (expect exceed-cap diagnostic)"

    local start_time=$(date +%s%N)
    local output
    output=$(timeout "$TIMEOUT_DURATION" "$MIMI_BIN" check "$tmp_file" 2>&1) && local exit_code=0 || local exit_code=$?
    local end_time=$(date +%s%N)
    local duration_ms=$(( (end_time - start_time) / 1000000 ))

    rm -f "$tmp_file"

    if [ "$exit_code" -eq 124 ]; then
        FAILED=$((FAILED + 1))
        log_fail "$name — TIMEOUT (${TIMEOUT_DURATION}s)"
    elif [ "$exit_code" -eq 0 ]; then
        FAILED=$((FAILED + 1))
        log_fail "$name — unexpectedly PASSED (depth guard not enforced!)"
    elif echo "$output" | grep -q "$needle"; then
        PASSED=$((PASSED + 1))
        log_pass "$name — loud exceed-cap diagnostic (${duration_ms}ms)"
    else
        FAILED=$((FAILED + 1))
        log_fail "$name — failed without the '$needle' diagnostic (exit $exit_code)"
    fi
}

echo "========================================="
echo "  压力测试套件"
echo "========================================="
echo ""

# ---------- 极限编译: 大函数 ----------
echo "--- 极限编译: 大规模分支 ---"

# wave1-review §5.11 (closed 2026-08-07): 旧参数（2000/10000 分支、500/2000
# 嵌套）结构性超出 parser 128 层递归守卫（栈安全边界），"必败"是设计使然。
# 套件现在断言双向契约：守卫内规模必须通过；越界必须响亮报
# "recursion limit exceeded"，而非静默或 SIGSEGV。
BRANCH_COUNT=100
if ! $CI_MODE; then
    BRANCH_COUNT=120
fi

big_if_src="func main() -> i64 {\n    let x = $((RANDOM % BRANCH_COUNT));\n"
for ((i=0; i<BRANCH_COUNT; i++)); do
    big_if_src+="    if x == $i { $i } else "
done
big_if_src+="{ -1 }\n}"

run_stress_test "big-if-else-${BRANCH_COUNT}" "$(echo -e "$big_if_src")" "check"

# 越界对照：2000 分支的 else-if 链嵌套远超 128 层 → 必须响亮拒绝。
EXCEED_BRANCH=2000
exceed_if_src="func main() -> i64 {\n    let x = 0;\n"
for ((i=0; i<EXCEED_BRANCH; i++)); do
    exceed_if_src+="    if x == $i { $i } else "
done
exceed_if_src+="{ -1 }\n}"

run_stress_exceed_cap "big-if-else-${EXCEED_BRANCH}-exceed-cap" "$(echo -e "$exceed_if_src")"

# ---------- 大 match ----------
MATCH_COUNT=5000
if ! $CI_MODE; then
    MATCH_COUNT=20000
fi

big_match_src="func main() -> i64 {\n    let x = $((RANDOM % MATCH_COUNT));\n    match x {\n"
for ((i=0; i<MATCH_COUNT; i++)); do
    big_match_src+="        $i => $((i * 2)),\n"
done
big_match_src+="        _ => -1\n    }\n}"

run_stress_test "big-match-${MATCH_COUNT}" "$(echo -e "$big_match_src")" "check"

# ---------- 深度嵌套 ----------
# §5.11：守卫内深度必须通过；越界（500/2000）必须响亮拒绝。
# 实测（2026-08-07）：每层 if-block 消耗 ~2 个深度单位，128 预算约容
# 62 层嵌套；60 层留有安全余量。
NEST_DEPTH=60
if ! $CI_MODE; then
    NEST_DEPTH=60
fi

deep_nest_src="func main() -> i64 {\n    let x = 1;\n"
indent=""
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

EXCEED_NEST=500
if ! $CI_MODE; then
    EXCEED_NEST=2000
fi

exceed_nest_src="func main() -> i64 {\n    let x = 1;\n"
indent=""
for ((i=0; i<EXCEED_NEST; i++)); do
    exceed_nest_src+="${indent}if x > 0 {\n${indent}    let y = $i;\n"
    indent="${indent}    "
done
exceed_nest_src+="${indent}x\n"
for ((i=0; i<EXCEED_NEST; i++)); do
    indent="${indent%    }"
    exceed_nest_src+="${indent}} else { 0 }\n"
done
exceed_nest_src+="}"

run_stress_exceed_cap "deep-nest-${EXCEED_NEST}-exceed-cap" "$(echo -e "$exceed_nest_src")"

# ---------- 海量 Actor (CI 模式用 10000, 本地用 100000) ----------
ACTOR_COUNT=10000
if ! $CI_MODE; then
    ACTOR_COUNT=100000
fi

actor_src=""
for ((i=0; i<ACTOR_COUNT; i++)); do
    actor_src+="actor Worker${i} {\n    fn work() -> i64 { ${i} }\n}\n\n"
done
actor_src+="func main() -> i64 { 0 }"

run_stress_test "massive-actor-${ACTOR_COUNT}" "$actor_src" "check"

# ---------- 极限编译: 超大列表字面量 ----------
LIST_SIZE=50000
if $CI_MODE; then
    LIST_SIZE=10000
fi

list_src="func main() -> i64 {\n    let xs = ["
first=true
for ((i=0; i<LIST_SIZE; i++)); do
    $first && first=false || list_src+=", "
    list_src+="$i"
done
list_src+="];\n    len(xs)\n}"

run_stress_test "big-list-${LIST_SIZE}" "$(echo -e "$list_src")" "check"

# ---------- 重复编译稳定性 ----------
log_info "--- 重复编译稳定性 (10 次) ---"
fib_src="func fib(n: i64) -> i64 {
    if n <= 1 { n } else { fib(n-1) + fib(n-2) }
}
func main() -> i64 { fib(20) }"

for ((i=0; i<10; i++)); do
    TOTAL=$((TOTAL + 1))
    tmp_file=$(mktemp /tmp/mimi_fib_stress.XXXXXX.mimi)
    echo "$fib_src" > "$tmp_file"

    # 注意：此循环在脚本顶层（非函数内），不能用 local。
    # 普通赋值不会掩盖命令替换的退出码，`|| exit_code=$?` 才能真正捕获失败。
    exit_code=0
    output=$("$MIMI_BIN" check "$tmp_file" 2>&1) || exit_code=$?
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
