#!/bin/bash
# Safe test runner: monitors memory and kills tests before OOM
# Usage: ./scripts/safe-test.sh [--test-threads=N] [extra cargo test args...]
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

# Ensure LLVM wrapper exists
if [ ! -f /tmp/llvm-wrapper/bin/llvm-config ]; then
    echo "Setting up LLVM wrapper..."
    bash "$SCRIPT_DIR/setup-llvm-wrapper.sh"
fi

export LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper

# Memory limit: 80% of available RAM (leave headroom for OS)
TOTAL_RAM_KB=$(grep MemTotal /proc/meminfo | awk '{print $2}')
LIMIT_KB=$((TOTAL_RAM_KB * 80 / 100))
LIMIT_MB=$((LIMIT_KB / 1024))
echo "Memory limit: ${LIMIT_MB}MB (80% of $((TOTAL_RAM_KB/1024))MB total)"

# Parse args: first positional arg is threads, rest passed to cargo test
THREADS="${1:-4}"
shift 2>/dev/null || true

# Memory monitor function
monitor_memory() {
    local pid=$1
    local limit_kb=$2
    while kill -0 "$pid" 2>/dev/null; do
        # Get RSS of the process tree
        local rss_kb=$(ps -eo rss= --ppid="$pid" 2>/dev/null | awk '{s+=$1} END {print s+0}')
        local self_kb=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ')
        local total_kb=$((rss_kb + ${self_kb:-0}))
        local total_mb=$((total_kb / 1024))

        if [ "$total_kb" -gt "$limit_kb" ]; then
            echo ""
            echo "⚠ Memory limit exceeded: ${total_mb}MB > ${LIMIT_MB}MB"
            echo "Killing test process tree (PID=$pid)..."
            kill -TERM -- -"$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null
            sleep 2
            kill -9 -- -"$pid" 2>/dev/null || kill -9 "$pid" 2>/dev/null
            exit 1
        fi

        # Progress indicator every 10s
        echo -ne "\r  Memory: ${total_mb}MB / ${LIMIT_MB}MB limit  "
        sleep 10
    done
}

echo "Running: cargo test -- --test-threads=$THREADS $@"
echo ""

# Start test in background
cargo test -- --test-threads="$THREADS" "$@" &
TEST_PID=$!

# Start memory monitor
monitor_memory "$TEST_PID" "$LIMIT_KB" &
MONITOR_PID=$!

# Wait for test to finish
wait "$TEST_PID" 2>/dev/null
TEST_EXIT=$?

# Kill monitor
kill "$MONITOR_PID" 2>/dev/null
wait "$MONITOR_PID" 2>/dev/null

echo ""
exit $TEST_EXIT
