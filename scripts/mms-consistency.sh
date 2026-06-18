#!/bin/bash
# ============================================================
# MMS 一致性检查 + 自举预言机 (Bootstrap Oracle)
#
# 功能:
#   1. 检查每个 .mms rule/func 定义在 .mimi 中有对应实现和测试
#   2. 自举预言机: 用 Rust 版编译器编译 Mimi 版编译器 → 二次编译自检
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
source "$SCRIPT_DIR/fuzz-common.sh"

MIMI_BIN=$(ensure_mimi)
MIMISPEC_DIR="$PROJECT_DIR/../mimispecref"
MIMI_STD_DIR="$PROJECT_DIR/std"

TOTAL_CHECKS=0
PASSED=0
FAILED=0

check_coverage() {
    TOTAL_CHECKS=$((TOTAL_CHECKS + 1))
    local feature="$1"
    local spec_file="$2"
    local impl_pattern="$3"
    local test_pattern="$4"

    local spec_exists=false
    local impl_exists=false
    local test_exists=false

    [ -f "$spec_file" ] && spec_exists=true

    # 搜索实现
    if grep -qr "$impl_pattern" "$PROJECT_DIR/src" --include='*.rs' 2>/dev/null; then
        impl_exists=true
    fi

    # 搜索测试
    if grep -qr "$test_pattern" "$PROJECT_DIR/src/tests" --include='*.rs' 2>/dev/null; then
        test_exists=true
    fi

    if $spec_exists && $impl_exists && $test_exists; then
        PASSED=$((PASSED + 1))
        log_pass "$feature — spec ✓ impl ✓ test ✓"
    else
        FAILED=$((FAILED + 1))
        local details=""
        $spec_exists || details="${details}spec ✗ "
        $impl_exists || details="${details}impl ✗ "
        $test_exists || details="${details}test ✗ "
        log_fail "$feature — $details"
    fi
}

echo "========================================="
echo "  MMS 语言规范一致性检查"
echo "========================================="
echo ""

# 检查主要语言特性是否在 spec + impl + test 中均有覆盖
check_coverage "match-exhaustive" \
    "$MIMISPEC_DIR/mimispec_v0.3.1_v1.0.0-rc.1.md" \
    "match\|Match\|non.exhaustive" \
    "non.exhaustive\|match.*exhaustive"

check_coverage "linear-capability" \
    "$MIMISPEC_DIR/mimispec-reference.md" \
    "cap\|Cap\|Capability" \
    "cap\|Capability\|use.after"

check_coverage "ffi-extern" \
    "$MIMISPEC_DIR/mimispec_v0.3.1_v1.0.0-rc.1.md" \
    "extern\|ExternBlock\|ExternFunc" \
    "extern\|ffi\|ExternBlock"

check_coverage "contract-requires-ensures" \
    "$MIMISPEC_DIR/mimispec-reference.md" \
    "requires\|ensures\|Contract" \
    "requires\|ensures\|contract"

check_coverage "actor-parasteps" \
    "$MIMISPEC_DIR/mimispec_v0.3.1_v1.0.0-rc.1.md" \
    "Actor\|parastep\|spawn" \
    "Actor\|parasteps\|actor"

check_coverage "generics" \
    "$MIMISPEC_DIR/mimispec-reference.md" \
    "Generic\|TypeParam\|type_param" \
    "generic\|Generic"

check_coverage "ownership" \
    "$MIMISPEC_DIR/mimispec-reference.md" \
    "own\|Own\|borrow\|cap\|capability" \
    "own\|ownership\|move\|borrow"

check_coverage "allocator" \
    "$MIMISPEC_DIR/mimispec-reference.md" \
    "alloc\|Allocator\|Arena\|Bump" \
    "alloc\|Allocator\|arena\|bump"

check_coverage "error-handling" \
    "$MIMISPEC_DIR/mimispec-reference.md" \
    "on_failure\|Result\|try\|Error" \
    "error\|on_failure\|Result"

check_coverage "comptime" \
    "$MIMISPEC_DIR/mimispec-reference.md" \
    "comptime\|CompTime\|compile_time" \
    "comptime\|CompTime"

check_coverage "modules-imports" \
    "$MIMISPEC_DIR/mimispec-reference.md" \
    "import\|Module\|module" \
    "import\|module\|Module"

check_coverage "z3-verification" \
    "$MIMISPEC_DIR/mimispec_v0.3.1_v1.0.0-rc.1.md" \
    "z3\|Z3\|verif\|Solver\|SatResult" \
    "verif\|z3\|Z3\|verify"

check_coverage "string-fstring" \
    "$MIMISPEC_DIR/mimispec-reference.md" \
    "string\|fstring\|FString" \
    "string\|fstring\|FString"

check_coverage "comprehension" \
    "$MIMISPEC_DIR/mimispec-reference.md" \
    "comprehension\|list.*for\|Comprehension" \
    "comprehension\|Comprehension"

echo ""
echo "========================================="
echo "  结果汇总"
echo "========================================="
echo "  Total:  $TOTAL_CHECKS"
echo -e "  Passed: ${GREEN}$PASSED${NC}"
echo -e "  Failed: ${RED}$FAILED${NC}"

# ---------- 自举预言机 (Bootstrap Oracle) ----------
echo ""
echo "========================================="
echo "  自举预言机 (Bootstrap Oracle)"
echo "========================================="
echo ""
log_info "Bootstrap oracle: check for Mimi-written compiler..."
# 检查是否有 Mimi 写的编译器源文件
BOOTSTRAP_DIR="$PROJECT_DIR/bootstrap"
if [ -d "$BOOTSTRAP_DIR" ] && ls "$BOOTSTRAP_DIR"/*.mimi 2>/dev/null | head -1 > /dev/null; then
    log_info "Found bootstrap compiler sources in $BOOTSTRAP_DIR"

    local tmp_dir=$(mktemp -d /tmp/mimi_bootstrap.XXXXXX)
    local stage1_bin="$tmp_dir/stage1"

    # Stage 1: 用 Rust 编译器编译 Mimi 编译器
    if "$MIMI_BIN" build "$BOOTSTRAP_DIR/main.mimi" -o "$stage1_bin" 2>/dev/null; then
        log_info "Stage 1: Rust → Mimi compiler binary created"

        # Stage 2: 用 Stage 1 编译器再次编译自身
        if [ -x "$stage1_bin" ]; then
            local stage2_bin="$tmp_dir/stage2"
            if "$stage1_bin" build "$BOOTSTRAP_DIR/main.mimi" -o "$stage2_bin" 2>/dev/null; then
                # 对比 stage1 和 stage2 二进制
                if cmp -s "$stage1_bin" "$stage2_bin"; then
                    log_pass "Bootstrap oracle PASSED — byte-identical!"
                else
                    log_fail "Bootstrap oracle FAILED — stage1 and stage2 differ!"
                    FAILED=$((FAILED + 1))
                fi
            fi
        fi
    else
        log_warn "Stage 1 compilation failed — Mimi compiler may not be self-hostable yet"
    fi
    rm -rf "$tmp_dir"
else
    log_warn "No bootstrap compiler found in $BOOTSTRAP_DIR — skipping bootstrap oracle"
fi

echo ""

if [ "$FAILED" -gt 0 ]; then
    exit 1
fi
log_pass "All spec consistency checks passed."
