#!/usr/bin/env bash
# Full local release-candidate gate replay (delegates shared Tier 1/2 checks to
# `scripts/run-gates.sh` so lint/audit rules never diverge from CI).
#
# Usage: scripts/check_all_gates.sh
set -euo pipefail

ROOT_DIR="$(cd -P "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
# shellcheck source=/dev/null
source "$ROOT_DIR/scripts/dev-env.sh"

echo "== lint & static gates =="
bash scripts/run-gates.sh --lint --fail-fast

echo "== cargo test --lib =="
cargo test --locked --lib -- --test-threads="$(nproc 2>/dev/null || echo 4)"

echo "== dispatch zero =="
python3 scripts/dispatch_stat.py check --zero

echo "== stress smoke =="
make test-stress

echo "== stress heavy =="
make test-stress-heavy

echo "== dogfood =="
make test-dogfood

echo "== bin CLI tests =="
cargo test --locked --bin mimi -- --test-threads=1

echo "== real-world CLI =="
cargo test --locked --test real_world_cli -- --test-threads=1

echo "ALL GATES PASSED"
