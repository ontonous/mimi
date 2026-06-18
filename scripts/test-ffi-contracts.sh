#!/bin/bash
# ============================================================
# FFI 契约一致性检查 (--verify-ffi)
#   1. 自动运行所有 FFI 测试，验证 requires/ensures
#   2. 错误注入: 故意在 C 端引入违反 ensures 的值
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
source "$SCRIPT_DIR/fuzz-common.sh"

MIMI_BIN=$(ensure_mimi)
FFI_TEST_DIR="$PROJECT_DIR/scripts/fixtures/ffi"

mkdir -p "$FFI_TEST_DIR"

TOTAL=0
PASSED=0
FAILED=0

run_ffi_test() {
    local name="$1"
    local mimi_src="$2"
    local c_src="$3"
    local expected="$4"  # "verify-pass" | "verify-fail"

    TOTAL=$((TOTAL + 1))

    local tmp_dir=$(mktemp -d /tmp/mimi_ffi_test.XXXXXX)
    local mimi_file="$tmp_dir/test_${name}.mimi"
    local c_file="$tmp_dir/test_${name}.c"
    local so_file="$tmp_dir/libtest_${name}.so"

    echo "$mimi_src" > "$mimi_file"
    echo "$c_src" > "$c_file"

    # 编译 C 共享库
    if ! gcc -shared -fPIC -o "$so_file" "$c_file" 2>/dev/null; then
        log_fail "$name (C compilation failed)"
        FAILED=$((FAILED + 1))
        rm -rf "$tmp_dir"
        return
    fi

    export MIMI_FFI_LIB="$so_file"

    # 运行 verify 命令
    local exit_code=0
    local output=""
    output=$("$MIMI_BIN" verify "$mimi_file" 2>&1) || exit_code=$?

    unset MIMI_FFI_LIB
    rm -rf "$tmp_dir"

    if [ "$expected" = "verify-pass" ]; then
        if [ "$exit_code" -eq 0 ]; then
            PASSED=$((PASSED + 1))
            log_pass "$name (verified)"
        else
            FAILED=$((FAILED + 1))
            log_fail "$name (expected verify pass, got failure)"
            echo "    $output"
        fi
    elif [ "$expected" = "verify-fail" ]; then
        if [ "$exit_code" -ne 0 ] && echo "$output" | grep -qi "ensures\|requires\|violation\|failed"; then
            PASSED=$((PASSED + 1))
            log_pass "$name (correctly caught violation)"
        elif [ "$exit_code" -eq 0 ]; then
            FAILED=$((FAILED + 1))
            log_fail "$name (violation NOT detected — FFI verification gap!)"
        else
            FAILED=$((FAILED + 1))
            log_fail "$name (failed but without expected violation message)"
            echo "    $output"
        fi
    fi
}

echo "========================================="
echo "  FFI 契约一致性检查"
echo "========================================="
echo ""

# ---------- 正向测试: 合法 FFI ----------
run_ffi_test "valid-add" '
extern "C" {
    func add(a: i64, b: i64) -> i64;
}

func main() -> i64 {
    add(3, 4)
}
' '
long long add(long long a, long long b) { return a + b; }
' "verify-pass"

# ---------- 错误注入: 违反 ensures ----------
run_ffi_test "violate-ensures-positive" '
extern "C" {
    // ensures: result > 0
    func must_be_positive(x: i64) -> i64;
}

func main() -> i64 {
    must_be_positive(5)
}
' '
long long must_be_positive(long long x) { return -1; }
' "verify-fail"

run_ffi_test "violate-ensures-range" '
extern "C" {
    // ensures: result >= 0 && result <= 100
    func clamp_value(x: i64) -> i64;
}

func main() -> i64 {
    clamp_value(50)
}
' '
long long clamp_value(long long x) { return 999; }
' "verify-fail"

# ---------- 合法合约通过验证 ----------
run_ffi_test "valid-requires-ensures" '
extern "C" {
    // requires: x > 0
    // ensures: result > x
    func process(x: i64) -> i64;
}

func main() -> i64 {
    process(10)
}
' '
long long process(long long x) { return x + 1; }
' "verify-pass"

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
log_pass "All FFI contract checks passed."
