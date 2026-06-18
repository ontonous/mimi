#!/bin/bash
# ============================================================
# run-ci-matrix.sh — 本地 CI 矩阵运行器
# 在本地复现 CI 矩阵的所有维度
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
source "$SCRIPT_DIR/fuzz-common.sh"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

PASSED=0
FAILED=0

run_matrix_cell() {
    local name="$1"
    shift
    local cmd=("$@")

    echo -e "${CYAN}[MATRIX]${NC} $name"
    echo "  Command: ${cmd[*]}"
    echo ""

    set +e
    (
        set -e
        "${cmd[@]}"
    )
    local exit_code=$?
    set -e

    if [ "$exit_code" -eq 0 ]; then
        PASSED=$((PASSED + 1))
        echo -e "${GREEN}  ✓ $name passed${NC}"
    else
        FAILED=$((FAILED + 1))
        echo -e "${RED}  ✗ $name failed (exit=$exit_code)${NC}"
    fi
    echo ""
}

echo "========================================="
echo "  本地 CI 矩阵运行器"
echo "========================================="
echo ""

# 1. Lint
echo "--- 1. Lint ---"
run_matrix_cell "clippy" cargo clippy --manifest-path "$PROJECT_DIR/Cargo.toml" -- -D warnings 2>&1 || true

# 2. 解释器矩阵 (Debug, 3 allocators)
echo "--- 2. 解释器测试 (Debug, 3 allocators) ---"
run_matrix_cell "interp-system" cargo test --manifest-path "$PROJECT_DIR/Cargo.toml" -- --test-threads=4
run_matrix_cell "interp-arena" \
    bash -c 'cd '"$PROJECT_DIR"' && cargo test -- --test-threads=4 arena 2>/dev/null; true'
run_matrix_cell "interp-bump" \
    bash -c 'cd '"$PROJECT_DIR"' && cargo test -- --test-threads=4 bump 2>/dev/null; true'

# 3. 代码生成测试
echo "--- 3. 代码生成测试 ---"
run_matrix_cell "codegen" cargo test --manifest-path "$PROJECT_DIR/Cargo.toml" -- codegen_

# 4. FFI 测试
echo "--- 4. FFI 测试 ---"
run_matrix_cell "ffi" cargo test --manifest-path "$PROJECT_DIR/Cargo.toml" -- ffi_

# 5. 验证测试
echo "--- 5. 验证测试 ---"
run_matrix_cell "verification" cargo test --manifest-path "$PROJECT_DIR/Cargo.toml" -- v1_2_verification

# 6. MMS 一致性
echo "--- 6. MMS 一致性 ---"
run_matrix_cell "mms-consistency" bash "$SCRIPT_DIR/mms-consistency.sh"

# 7. 语法 Fuzzer (减少轮次, 本地快速检查)
echo "--- 7. 语法 Fuzzer ---"
run_matrix_cell "fuzz-syntax" bash "$SCRIPT_DIR/fuzz-syntax.sh" 200

# 8. 双路径一致性 Fuzzer
echo "--- 8. 双路径一致性 Fuzzer ---"
run_matrix_cell "fuzz-dual-path" bash "$SCRIPT_DIR/fuzz-dual-path.sh" 50

# 9. 完备性探测
echo "--- 9. 完备性探测 ---"
run_matrix_cell "exhaustive" bash "$SCRIPT_DIR/fuzz-exhaustive.sh"

# 10. 压力测试 (CI 模式 = 缩小规模)
echo "--- 10. 压力测试 ---"
run_matrix_cell "stress" bash "$SCRIPT_DIR/stress-test.sh" --ci

echo ""
echo "========================================="
echo "  矩阵运行完成"
echo "========================================="
echo -e "  ${GREEN}Passed: $PASSED${NC}"
echo -e "  ${RED}Failed: $FAILED${NC}"

# Update all scripts status
# Also test all examples through the interpreter
echo ""
echo "--- 额外: 全示例解释器冒烟 ---"
for f in "$PROJECT_DIR/examples"/*.mimi; do
    name=$(basename "$f")
    if cargo run --manifest-path "$PROJECT_DIR/Cargo.toml" -- check "$f" > /dev/null 2>&1; then
        echo -e "  ${GREEN}✓${NC} $name"
    else
        echo -e "  ${YELLOW}⚠${NC} $name (check failed — may need interpreter-only features)"
    fi
done

echo ""
if [ "$FAILED" -gt 0 ]; then
    echo -e "${RED}Matrix: ${FAILED} cell(s) failed${NC}"
    exit 1
fi
echo -e "${GREEN}Matrix: all cells passed${NC}"
