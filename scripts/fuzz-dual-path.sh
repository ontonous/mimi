#!/bin/bash
# ============================================================
# 双路径一致性 Fuzzer
#
# 生成随机 Mimi 代码 → 同时在解释器和编译器下运行 → 对比结果
# 重点比较纯数值程序的返回值:
#   - 解释器: stdout 输出 "-> <value>"
#   - 编译器: 二进制 exit code = main 的返回值
# 不一致即警报，保存最小复现样本
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/fuzz-common.sh"

ROUNDS="${1:-1000}"
OUTPUT_DIR="${FUZZ_DUAL_OUTPUT:-/tmp/mimi_fuzz_dual}"
mkdir -p "$OUTPUT_DIR"
CRASH_DIR="$OUTPUT_DIR/crashes"
mkdir -p "$CRASH_DIR"

MIMI_BIN=$(ensure_mimi)
log_info "Using mimi binary: $MIMI_BIN"
log_info "Running $ROUNDS dual-path consistency rounds"
echo ""

MISMATCH_COUNT=0
FAIL_COUNT=0

generate_consistency_test() {
    local id=$1
    local file="$OUTPUT_DIR/dual_${id}.mimi"

    # 使用 i32 类型 (语言原生类型)
    # 所有模板必须返回 i32 值，且解释器=编译器结果应一致
    local templates=(
        # 纯算术
        "func main() -> i32 {
    let x = %d + %d;
    x * 2
}"
        # 条件
        "func main() -> i32 {
    let a = %d;
    let b = %d;
    if a > b { a - b } else { b - a }
}"
        # 简单函数
        "func square(x: i32) -> i32 {
    x * x
}

func main() -> i32 {
    square(%d) + square(%d)
}"
        # match
        "func main() -> i32 {
    let x = %d;
    match x {
        0 => 100,
        1 => 200,
        2 => 300,
        _ => -1
    }
}"
        # 多重 let
        "func main() -> i32 {
    let a = %d;
    let b = a + %d;
    let c = b * 2;
    c - a
}"
        # 斐波那契 (小n)
        "func fib(n: i32) -> i32 {
    if n <= 1 { n } else { fib(n-1) + fib(n-2) }
}

func main() -> i32 {
    fib(%d)
}"
    )

    local idx=$((id % ${#templates[@]}))
    local a=$((RANDOM % 30 + 1))
    local b=$((RANDOM % 30 + 1))
    local c=$((RANDOM % 15 + 1))

    case $idx in
        0) printf "${templates[$idx]}" "$a" "$b" > "$file" ;;
        1) printf "${templates[$idx]}" "$a" "$b" > "$file" ;;
        2) printf "${templates[$idx]}" "$a" "$b" > "$file" ;;
        3) printf "${templates[$idx]}" "$((a % 4))" > "$file" ;;
        4) printf "${templates[$idx]}" "$a" "$b" > "$file" ;;
        5) printf "${templates[$idx]}" "$((a % 10))" > "$file" ;;
    esac
}

for ((i=0; i<ROUNDS; i++)); do
    generate_consistency_test "$i"
    f="$OUTPUT_DIR/dual_${i}.mimi"

    # 类型检查 — 跳过不合法的
    if ! "$MIMI_BIN" check "$f" > /dev/null 2>&1; then
        rm -f "$f"
        continue
    fi

    # 解释器执行
    interp_output=""
    interp_ok=false
    if interp_output=$("$MIMI_BIN" run "$f" 2>/dev/null); then
        interp_ok=true
    fi

    # 编译器执行 — 编译 + 运行二进制
    compiled_exit_code=""
    compiled_ok=false
    MIMI_FFI_LIB="" "$MIMI_BIN" build "$f" -o /tmp/mimi_fuzz_bin > /dev/null 2>&1 && {
        if [ -x /tmp/mimi_fuzz_bin ]; then
            set +e
            /tmp/mimi_fuzz_bin > /dev/null 2>&1
            compiled_exit_code=$?
            set -e
            compiled_ok=true
            rm -f /tmp/mimi_fuzz_bin
        fi
    } || true
    rm -f /tmp/mimi_fuzz_bin.o /tmp/mimi_fuzz_bin 2>/dev/null || true

    # 提取解释器的数值结果: "-> 42" → "42"
    interp_value=$(echo "$interp_output" | sed -n 's/^-> *//p' | xargs)

    if $interp_ok && $compiled_ok; then
        # 注意: Unix exit code 只有 8 位 (0-255), 所以需要 % 256
        interp_mod=$((interp_value % 256))
        if [ "$interp_mod" != "$compiled_exit_code" ]; then
            log_fail "MISMATCH at round $i!"
            log_fail "  Interpreter: '$interp_value' (mod 256 = $interp_mod)"
            log_fail "  Compiled:    exit code $compiled_exit_code"
            crash_file="$CRASH_DIR/mismatch_${i}_$(date +%s).mimi"
            cp "$f" "$crash_file"
            echo "# Interpreter: $interp_value" >> "$crash_file"
            echo "# Compiled exit: $compiled_exit_code" >> "$crash_file"
            MISMATCH_COUNT=$((MISMATCH_COUNT + 1))
        fi
    elif $interp_ok && ! $compiled_ok; then
        log_fail "COMPILER CRASH at round $i (interpreter succeeded)"
        crash_file="$CRASH_DIR/compile_crash_${i}_$(date +%s).mimi"
        cp "$f" "$crash_file"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    elif ! $interp_ok && $compiled_ok; then
        log_fail "INTERPRETER CRASH at round $i (compiler succeeded)"
        crash_file="$CRASH_DIR/interp_crash_${i}_$(date +%s).mimi"
        cp "$f" "$crash_file"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi

    rm -f "$f"

    if ((i % 200 == 0)) && ((i > 0)); then
        log_info "Progress: $i / $ROUNDS (${MISMATCH_COUNT} mismatches, ${FAIL_COUNT} crashes)"
    fi
done

echo ""
log_info "Dual-path fuzz complete: $ROUNDS rounds"
echo "  Mismatches: $MISMATCH_COUNT"
echo "  Crashes:    $FAIL_COUNT"
if [ "$MISMATCH_COUNT" -gt 0 ] || [ "$FAIL_COUNT" -gt 0 ]; then
    log_fail "Samples saved to: $CRASH_DIR"
    exit 1
fi
log_pass "All consistent — interpreter and compiler agree."
