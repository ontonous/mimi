#!/bin/bash
# ============================================================
# fuzz-common.sh — 共享 fuzzer 工具函数
# 被 fuzz-*.sh 脚本 source 使用
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
MIMI_BIN="${MIMI_BIN:-"$PROJECT_DIR/target/release/mimi"}"
MIMI_DEBUG_BIN="${MIMI_DEBUG_BIN:-"$PROJECT_DIR/target/debug/mimi"}"
MIMI_RUN="${MIMI_RUN:-"cargo run --release --"}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

log_info()  { echo -e "${CYAN}[INFO]${NC} $*"; }
log_pass()  { echo -e "${GREEN}[PASS]${NC} $*"; }
log_fail()  { echo -e "${RED}[FAIL]${NC} $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }

# 确认 mimi 二进制可用
ensure_mimi() {
    if [ ! -f "$MIMI_BIN" ] && [ ! -f "$MIMI_DEBUG_BIN" ]; then
        log_info "Building mimi (release)..."
        (cd "$PROJECT_DIR" && cargo build --release)
    fi
    if [ -f "$MIMI_BIN" ]; then
        echo "$MIMI_BIN"
    else
        echo "$MIMI_DEBUG_BIN"
    fi
}

# 运行 mimi check: 返回 0=成功 1=失败
mimi_check() {
    local mimi="$1"; shift
    local src_file="$1"; shift
    "$mimi" check "$src_file" 2>/dev/null
}

# 运行 mimi run: 返回 stdout
mimi_run() {
    local mimi="$1"; shift
    local src_file="$1"; shift
    "$mimi" run "$src_file" 2>/dev/null
}

# 编译 + 运行二进制
mimi_compile_and_run() {
    local mimi="$1"; shift
    local src_file="$1"; shift
    local tmp_bin
    tmp_bin=$(mktemp /tmp/mimi_fuzz_bin.XXXXXX)
    "$mimi" build "$src_file" -o "$tmp_bin" 2>/dev/null && {
        if [ -x "$tmp_bin" ]; then
            "$tmp_bin" 2>/dev/null || true
        fi
    } || true
    rm -f "$tmp_bin"
}
