#!/usr/bin/env bash
# Unified local & CI gate runner for Mimi.
#
# Replaces divergent ad-hoc checklists with one deterministic, fail-honest
# entrypoint ordered fastest-first (static/docs -> audits/fmt -> clippy -> tests).
#
# Usage:
#   scripts/run-gates.sh [--fast | --lint | --all | --python-core] [--ci] [--fail-fast]

set -uo pipefail

ROOT_DIR="$(cd -P "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# shellcheck source=/dev/null
source "$ROOT_DIR/scripts/dev-env.sh"

MODE="lint"
FAIL_FAST=0
CI_MODE=0
PLAN_ARGS=(--ready)

while [ "$#" -gt 0 ]; do
    case "$1" in
        --fast) MODE="fast" ;;
        --lint) MODE="lint" ;;
        --python-core) MODE="python-core" ;;
        --ci) CI_MODE=1 ;;
        --all) MODE="all" ;;
        --fail-fast) FAIL_FAST=1 ;;
        --keep-going) FAIL_FAST=0 ;;
        -h|--help)
            sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \?//'
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 2
            ;;
    esac
    shift
done

if [ "$CI_MODE" -eq 1 ]; then
    PLAN_ARGS=(--ready --allow-missing-external)
fi

RESULTS=()
PASS_COUNT=0
FAIL_COUNT=0

run_gate() {
    local name="$1"
    shift
    printf '\n=== [GATE] %s ===\n' "$name"
    local exit_code=0
    if "$@"; then
        RESULTS+=("PASS|$name|0")
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        exit_code=$?
        RESULTS+=("FAIL|$name|$exit_code")
        FAIL_COUNT=$((FAIL_COUNT + 1))
        if [ "$FAIL_FAST" -eq 1 ]; then
            print_summary
            exit "$exit_code"
        fi
    fi
}

print_summary() {
    printf '\n=========================================\n'
    printf '  Mimi Gate Dashboard (%s)\n' "$MODE"
    printf '=========================================\n'
    local row status label code
    for row in "${RESULTS[@]}"; do
        IFS='|' read -r status label code <<< "$row"
        if [ "$status" = "PASS" ]; then
            printf '  [PASS] %s\n' "$label"
        else
            printf '  [FAIL] %s (exit=%s)\n' "$label" "$code"
        fi
    done
    printf -- '-----------------------------------------\n'
    printf '  Passed: %d   Failed: %d\n' "$PASS_COUNT" "$FAIL_COUNT"
}

run_core_python_tests() {
    python3 -m unittest \
        scripts/test_collect_evidence.py \
        scripts/test_dispatch_stat.py \
        scripts/test_merge_actor_census_unresolved_adjudication_r3.py \
        scripts/test_reconcile_actor_census_orthogonal_ledger.py \
        scripts/test_release_evidence.py \
        scripts/test_v02_evidence.py \
        tests/scripts/test_actor_census_build_receipt.py \
        tests/scripts/test_args_string_index_l3_runner_contract.py \
        tests/scripts/test_audit_mir_native_production.py \
        tests/scripts/test_gen_stdlib_matrix_maps.py \
        tests/scripts/test_generate_route_census_authority.py \
        tests/scripts/test_ordinary_map_any_v2_l3_runner_contract.py \
        tests/scripts/test_ordinary_map_set_get_any_i32_l3_runner_contract.py \
        tests/scripts/test_render_special_route_authorities.py \
        tests/scripts/test_v02_app_eval.py
}

if [ "$MODE" = "python-core" ]; then
    run_core_python_tests
    exit $?
fi

# Tier 1: Fast static, docs & script contracts (~5s)
run_gate "language-docs" python3 scripts/check_language_docs.py
run_gate "v031-roadmap" python3 scripts/check_v031_roadmap.py
run_gate "edge-isolation" python3 scripts/check_edge_isolation.py
run_gate "mms-removal-guard" bash scripts/mms-consistency.sh
run_gate "v02-plan-structure" python3 devdocs/V0.2/tools/check_plan.py "${PLAN_ARGS[@]}"
run_gate "native-audit-self-test" bash scripts/audit-mir-native-production.sh --self-test
run_gate "python-core-tests" run_core_python_tests

if [ "$MODE" = "fast" ]; then
    print_summary
    [ "$FAIL_COUNT" -eq 0 ]
    exit $?
fi

# Tier 2: Code hygiene & MIR boundary gates
run_gate "unsafe-safety" python3 scripts/check_unsafe_safety.py
run_gate "native-mir-production" bash scripts/audit-mir-native-production.sh
run_gate "legacy-owners-static" bash scripts/audit-mir-legacy-owners-static.sh
run_gate "cargo-fmt" cargo fmt -- --check
run_gate "cargo-clippy" cargo clippy --locked --all-targets -- -D warnings

if [ "$MODE" = "lint" ]; then
    print_summary
    [ "$FAIL_COUNT" -eq 0 ]
    exit $?
fi

# Tier 3: Full compiler & invariant test suite
THREADS="${MIMI_TEST_THREADS:-$(nproc 2>/dev/null || echo 4)}"
if [ "$THREADS" -gt 16 ]; then
    THREADS=16
fi

run_gate "l1-dual-backend" cargo test --locked dual_ -- --test-threads=2
run_gate "l2-typecheck" cargo test --locked typecheck:: -- --test-threads=2
run_gate "ffi-contracts" cargo test --locked ffi_ -- --test-threads=2
run_gate "z3-verification" cargo test --locked v1_2_verification -- --test-threads=1
run_gate "full-test-suite" bash -c "ulimit -v 20000000 && cargo test --locked -- --test-threads=${THREADS}"

print_summary
[ "$FAIL_COUNT" -eq 0 ]
