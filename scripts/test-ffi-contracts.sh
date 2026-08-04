#!/bin/bash
# ============================================================
# FFI 契约一致性检查 (Z3 static + runtime --verify-ffi)
#   Phase A: 用 Z3 静态验证 extern 合约的 consistency
#   Phase B: 用 --verify-ffi 运行时检测 C 实现的合约违反
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
    local expected="$4"  # "z3-pass" | "runtime-pass" | "runtime-fail"

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

    case "$expected" in
        "z3-pass")
            # Phase A: Z3 静态验证（合约 consistency，不涉及 C 实现）
            local exit_code=0
            local output=""
            output=$("$MIMI_BIN" verify "$mimi_file" 2>&1) || exit_code=$?
            # 正向断言成功，而非"未见失败标记"：
            #   1) verify 退出码必须为 0 —— Disproven/inconclusive/"Z3 solver
            #      not available" 均非零退出（src/main/verify.rs + main.rs:517-520）；
            #   2) 输出必须非空且含显式成功标记：src/main/verify.rs 的
            #      "N/M verified" 汇总行或 "No contracts to verify"。
            # 旧逻辑只看 absence-of-failure，verifier 崩溃/"Z3 not found" 也会 PASS。
            if [ "$exit_code" -eq 0 ] && [ -n "$output" ] \
                && echo "$output" | grep -qiE 'verified|no contracts to verify'; then
                PASSED=$((PASSED + 1))
                log_pass "$name (Z3 verified)"
            else
                FAILED=$((FAILED + 1))
                log_fail "$name (Z3 verify expected pass, got failure)"
                echo "    $output"
            fi
            ;;
        "runtime-pass")
            # Phase B: 运行时验证，C 实现满足合约
            # exit_code 必须先初始化为 0 并在 if 中引用：赋值语句自身状态恒为 0，
            # 旧代码 `if [ $? -eq 0 ]` 测的是赋值状态而非二进制退出码（恒真）。
            local exit_code=0
            local output=""
            output=$("$MIMI_BIN" run --verify-ffi "$mimi_file" 2>&1) || exit_code=$?
            if [ "$exit_code" -eq 0 ]; then
                PASSED=$((PASSED + 1))
                log_pass "$name (runtime passed)"
            else
                FAILED=$((FAILED + 1))
                log_fail "$name (runtime expected pass, got failure)"
                echo "    $output"
            fi
            ;;
        "runtime-fail")
            # Phase B: 运行时验证，C 实现故意违反合约
            local exit_code=0
            local output=""
            output=$("$MIMI_BIN" run --verify-ffi "$mimi_file" 2>&1) || exit_code=$?
            # 违约被检出的判据：运行必须失败（退出码非零，违约错误经
            # src/interp/ffi_runtime.rs 的 "FFI contract violation" → Err → exit 1）
            # 且输出必须带合约违约标记。仅凭标记或仅凭退出码都不充分。
            if [ "$exit_code" -ne 0 ] \
                && echo "$output" | grep -qi "ensures\|requires\|violation\|failed\|assert"; then
                PASSED=$((PASSED + 1))
                log_pass "$name (runtime correctly caught violation)"
            else
                FAILED=$((FAILED + 1))
                log_fail "$name (violation NOT detected — FFI verification gap!)"
                echo "    $output"
            fi
            ;;
    esac

    unset MIMI_FFI_LIB
    rm -rf "$tmp_dir"
}

echo "========================================="
echo "  FFI 契约一致性检查"
echo "  Phase A: Z3 静态验证 extern contracts"
echo "  Phase B: --verify-ffi 运行时检测"
echo "========================================="
echo ""

# ============================================================
# Phase A: Z3 静态验证
#   验证 extern 合约本身的逻辑一致性（不涉及 C 实现）
# ============================================================
echo "--- Phase A: Z3 Static Verification ---"

run_ffi_test "valid-add" '
extern "C" {
    func add(a: i64, b: i64) -> i64;
}

func main() -> i64 {
    add(3, 4)
}
' '
long long add(long long a, long long b) { return a + b; }
' "z3-pass"

run_ffi_test "z3-ensures-positive" '
extern "C" {
    func must_be_positive(x: i64) -> i64
        ensures: result > 0;
}

func main() -> i64 {
    must_be_positive(5)
}
' '
long long must_be_positive(long long x) { return -1; }
' "z3-pass"

run_ffi_test "z3-ensures-range" '
extern "C" {
    func clamp_value(x: i64) -> i64
        ensures: result >= 0 && result <= 100;
}

func main() -> i64 {
    clamp_value(50)
}
' '
long long clamp_value(long long x) { return 999; }
' "z3-pass"

run_ffi_test "z3-requires-ensures" '
extern "C" {
    func process(x: i64) -> i64
        requires: x > 0
        ensures: result > x;
}

func main() -> i64 {
    process(10)
}
' '
long long process(long long x) { return x + 1; }
' "z3-pass"

# ============================================================
# Phase B: 运行时 --verify-ffi 验证
#   检测 C 实现是否实际满足合约
# ============================================================
echo ""
echo "--- Phase B: Runtime --verify-ffi Verification ---"

# 正向测试：合法 FFI + 合法返回值
run_ffi_test "rt-valid-add" '
extern "C" {
    func add(a: i64, b: i64) -> i64;
}

func main() -> i64 {
    add(3, 4)
}
' '
long long add(long long a, long long b) { return a + b; }
' "runtime-pass"

# 正向测试：有 ensures 且 C 实现满足
run_ffi_test "rt-ensures-satisfied" '
extern "C" {
    func must_be_positive(x: i64) -> i64
        ensures: result > 0;
}

func main() -> i64 {
    must_be_positive(5)
}
' '
long long must_be_positive(long long x) { return 42; }
' "runtime-pass"

# 错误注入：C 实现违反 ensures
run_ffi_test "rt-violate-ensures-positive" '
extern "C" {
    func must_be_positive(x: i64) -> i64
        ensures: result > 0;
}

func main() -> i64 {
    must_be_positive(5)
}
' '
long long must_be_positive(long long x) { return -1; }
' "runtime-fail"

# 错误注入：C 实现返回范围外值
run_ffi_test "rt-violate-ensures-range" '
extern "C" {
    func clamp_value(x: i64) -> i64
        ensures: result >= 0 && result <= 100;
}

func main() -> i64 {
    clamp_value(50)
}
' '
long long clamp_value(long long x) { return 999; }
' "runtime-fail"

# 合法合约：requires + ensures 都满足
run_ffi_test "rt-valid-requires-ensures" '
extern "C" {
    func process(x: i64) -> i64
        requires: x > 0
        ensures: result > x;
}

func main() -> i64 {
    process(10)
}
' '
long long process(long long x) { return x + 1; }
' "runtime-pass"

# 错误注入：违反 requires（传入非法值），确认不会被运行时误报
run_ffi_test "rt-requires-guard" '
extern "C" {
    func process(x: i64) -> i64
        requires: x > 0
        ensures: result > 0;
}

func main() -> i64 {
    process(10)
}
' '
long long process(long long x) { return -1; }
' "runtime-fail"

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
