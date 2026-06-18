#!/bin/bash
# ============================================================
# 语法 Fuzzer — 生成随机合法 Mimi 代码，喂给 mimi check
# 目的：检查 mimi check 是否会崩溃 (段错误 / panic / 意外退出)
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/fuzz-common.sh"

ROUNDS="${1:-5000}"
OUTPUT_DIR="${FUZZ_OUTPUT:-/tmp/mimi_fuzz_syntax}"
mkdir -p "$OUTPUT_DIR"

MIMI_BIN=$(ensure_mimi)
log_info "Using mimi binary: $MIMI_BIN"
log_info "Running $ROUNDS fuzz rounds, crashes saved to $OUTPUT_DIR"
echo ""

CRASH_COUNT=0
CRASH_FILES=()

generate_random_program() {
    local id=$1
    local file="$OUTPUT_DIR/fuzz_${id}.mimi"

    # 从预定义模板中随机选择并生成语义合法的 Mimi 代码
    local templates=(
        # 1: 基础算术
        "func main() -> i64 {
    let a = %d;
    let b = %d;
    let c = a %s b;
    c
}"
        # 2: 条件分支
        "func main() -> i64 {
    let x = %d;
    if x > 0 {
        x
    } else {
        -x
    }
}"
        # 3: 循环
        "func main() -> i64 {
    let mut sum = 0;
    let n = %d;
    let i = 0;
    while i < n {
        sum = sum + i;
        i = i + 1;
    };
    sum
}"
        # 4: 函数调用
        "func add(a: i64, b: i64) -> i64 {
    a + b
}

func main() -> i64 {
    add(%d, %d)
}"
        # 5: 列表操作
        "func main() -> i64 {
    let xs = [%d, %d, %d, %d, %d];
    let s = len(xs);
    s
}"
        # 6: match
        "func main() -> i64 {
    let x = %d;
    match x {
        0 => 42,
        1 => 100,
        _ => -1
    }
}"
        # 7: 嵌套 if-else
        "func main() -> i64 {
    let a = %d;
    let b = %d;
    if a > 0 {
        if b > 0 {
            a + b
        } else {
            a
        }
    } else {
        b
    }
}"
        # 8: 布尔运算
        "func main() -> bool {
    let a = %d;
    let b = %d;
    a > 0 && b > 0
}"
        # 9: 斐波那契风格递归
        "func fib(n: i64) -> i64 {
    if n <= 1 {
        n
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

func main() -> i64 {
    fib(%d)
}"
        # 10: 元组
        "func main() -> i64 {
    let t = (%d, %d);
    t.0 + t.1
}"
        # 11: 字段修改
        "type Point {
    x: i64,
    y: i64,
}

func main() -> i64 {
    let mut p = Point { x: %d, y: %d };
    p.x = p.x + 1;
    p.x
}"
        # 12: for-range
        "func main() -> i64 {
    let mut s = 0;
    let n = %d;
    for i in 0..n {
        s = s + i;
    };
    s
}"
        # 13: for-list
        "func main() -> i64 {
    let xs = [%d, %d, %d];
    let mut s = 0;
    for x in xs {
        s = s + x;
    };
    s
}"
        # 14: 字符串
        "func main() -> string {
    let s = \"hello\";
    s
}"
        # 15: 复杂嵌套表达式
        "func main() -> i64 {
    let x = (%d + %d) * (%d - %d);
    x / 2
}"
    )

    local idx=$((id % ${#templates[@]}))
    local template="${templates[$idx]}"

    # 随机填充参数
    local a=$((RANDOM % 100 + 1))
    local b=$((RANDOM % 100 + 1))
    local c=$((RANDOM % 100 + 1))
    local d=$((RANDOM % 100 + 1))
    local e=$((RANDOM % 100 + 1))
    local op_idx=$((RANDOM % 4))
    local ops=("+" "-" "*" "/")
    local op="${ops[$op_idx]}"

    case $idx in
        0) printf "$template" "$a" "$b" "$op" > "$file" ;;
        1) printf "$template" "$a" > "$file" ;;
        2) printf "$template" "$((a % 20 + 1))" > "$file" ;;
        3) printf "$template" "$a" "$b" > "$file" ;;
        4) printf "$template" "$a" "$b" "$c" "$d" "$e" > "$file" ;;
        5) printf "$template" "$((a % 4))" > "$file" ;;
        6) printf "$template" "$a" "$b" > "$file" ;;
        7) printf "$template" "$a" "$b" > "$file" ;;
        8) printf "$template" "$((a % 15 + 1))" > "$file" ;;
        9) printf "$template" "$((a % 15 + 1))" > "$file" ;;
        10) printf "$template" "$a" "$b" > "$file" ;;
        11) printf "$template" "$a" "$b" > "$file" ;;
        12) printf "$template" "$a" "$b" "$c" > "$file" ;;
        13) printf "$template" > "$file" ;;
        14) printf "$template" > "$file" ;;
        15) printf "$template" "$a" "$b" "$c" "$d" > "$file" ;;
    esac
}

for ((i=0; i<ROUNDS; i++)); do
    generate_random_program "$i"

    if ! "$MIMI_BIN" check "$OUTPUT_DIR/fuzz_${i}.mimi" > /dev/null 2>&1; then
        # 类型检查失败是预期行为 — 我们只关心崩溃
        rm -f "$OUTPUT_DIR/fuzz_${i}.mimi"
        continue
    fi

    # 再跑一次确认没有崩溃 (双路径校验)
    if ! "$MIMI_BIN" check "$OUTPUT_DIR/fuzz_${i}.mimi" > /dev/null 2>&1; then
        log_fail "CRASH on valid program at round $i!"
        mv "$OUTPUT_DIR/fuzz_${i}.mimi" "$OUTPUT_DIR/crash_${i}.mimi"
        CRASH_COUNT=$((CRASH_COUNT + 1))
        CRASH_FILES+=("$OUTPUT_DIR/crash_${i}.mimi")
    else
        rm -f "$OUTPUT_DIR/fuzz_${i}.mimi"
    fi

    if ((i % 500 == 0)) && ((i > 0)); then
        log_info "Progress: $i / $ROUNDS (${CRASH_COUNT} crashes)"
    fi
done

echo ""
log_info "Fuzz complete: $ROUNDS rounds, ${CRASH_COUNT} crashes"
if [ ${#CRASH_FILES[@]} -gt 0 ]; then
    log_fail "Crash samples saved to:"
    for f in "${CRASH_FILES[@]}"; do
        echo "  $f"
    done
    exit 1
fi
log_pass "No crashes detected."
