#!/usr/bin/env bash
# Source-bound multi-Set Result L3 gate. The current repository lacks an exact
# Checker/MIR/native route for these fixtures, so its integration tests record
# the route boundary and fail explicitly until that route is implemented.
set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
test_target=ordinary_map_multiset_result_l3
runtime_feature=ordinary-map-set-result-l3

: "${CARGO_TARGET_DIR:?Set CARGO_TARGET_DIR to an explicit external target directory}"
: "${MIMI_ORDINARY_MAP_MULTISET_RESULT_L3_EVIDENCE_DIR:?Set MIMI_ORDINARY_MAP_MULTISET_RESULT_L3_EVIDENCE_DIR to a new external evidence directory}"

requested_target_dir="$CARGO_TARGET_DIR"
requested_evidence_dir="$MIMI_ORDINARY_MAP_MULTISET_RESULT_L3_EVIDENCE_DIR"
[[ "$requested_target_dir" == /* ]] || {
    printf 'CARGO_TARGET_DIR must be absolute: %s\n' "$requested_target_dir" >&2
    exit 2
}
[[ "$requested_evidence_dir" == /* ]] || {
    printf 'L3 evidence directory must be absolute: %s\n' "$requested_evidence_dir" >&2
    exit 2
}
[[ ! -e "$requested_evidence_dir" && ! -L "$requested_evidence_dir" ]] || {
    printf 'Refusing to overwrite existing L3 evidence: %s\n' "$requested_evidence_dir" >&2
    exit 2
}

target_dir="$(realpath -m -- "$requested_target_dir")"
evidence_dir="$(realpath -m -- "$requested_evidence_dir")"
case "$target_dir/" in
    "$repo_root/"*)
        printf 'CARGO_TARGET_DIR must stay outside the repository: %s\n' "$target_dir" >&2
        exit 2
        ;;
esac
case "$evidence_dir/" in
    "$repo_root/"*)
        printf 'L3 evidence must stay outside the repository: %s\n' "$evidence_dir" >&2
        exit 2
        ;;
esac

llvm_prefix="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
[[ -d "$llvm_prefix" ]] || {
    printf 'LLVM_SYS_181_PREFIX is unavailable: %s\n' "$llvm_prefix" >&2
    exit 2
}
llvm_prefix="$(realpath -- "$llvm_prefix")"
for tool in cargo rustc valgrind sha256sum realpath find sort cmp diff xargs awk git; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'Strict multi-Set Result L3 requires %s\n' "$tool" >&2
        exit 2
    }
done

case "$evidence_dir/" in
    "$target_dir/"*)
        printf 'L3 evidence directory must stay outside CARGO_TARGET_DIR: %s\n' "$evidence_dir" >&2
        exit 2
        ;;
esac
case "$target_dir/" in
    "$evidence_dir/"*)
        printf 'CARGO_TARGET_DIR must stay outside the L3 evidence directory: %s\n' "$target_dir" >&2
        exit 2
        ;;
esac

mkdir -p -- "$target_dir" "$(dirname -- "$evidence_dir")"
mkdir -- "$evidence_dir"
target_dir="$(realpath -- "$target_dir")"
evidence_dir="$(realpath -- "$evidence_dir")"
export CARGO_TARGET_DIR="$target_dir"
export LLVM_SYS_181_PREFIX="$llvm_prefix"
export MIMI_ORDINARY_MAP_MULTISET_RESULT_L3_EVIDENCE_DIR="$evidence_dir"

fixture_paths=(
    tests/fixtures/mir_ordinary_map_any_owner_v2_record.mimi
    tests/fixtures/mir_ordinary_map_any_owner_v2_record_unit.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_clone.mimi
)
fixture_hashes=(
    7befca311daf9129a1495f06461309b9b62cc7a6b0d826d02ef9041870b79cc5
    85f7c9ee2a386b2e5caf1ae8f51753b9c96832625a4729f92bf34a837cce8ca5
    91bdc8f2471dd0eaa7910d5cab43763b3ff311f18cfdbc8980aa4c97538f4d90
)
for index in "${!fixture_paths[@]}"; do
    fixture="$repo_root/${fixture_paths[$index]}"
    [[ -f "$fixture" ]] || {
        printf 'Pinned fixture is missing: %s\n' "$fixture" >&2
        exit 2
    }
    actual_hash="$(sha256sum -- "$fixture" | awk '{print $1}')"
    [[ "$actual_hash" == "${fixture_hashes[$index]}" ]] || {
        printf 'Pinned fixture hash mismatch: %s expected=%s actual=%s\n' \
            "$fixture" "${fixture_hashes[$index]}" "$actual_hash" >&2
        exit 2
    }
done

source_snapshot() {
    (
        cd -- "$repo_root"
        sha256sum -- \
            Cargo.toml Cargo.lock \
            src/core/mir/route.rs \
            src/codegen/mir/ordinary_map_owned_any_v2.rs \
            src/runtime/map_any_snapshot.rs \
            tests/ordinary_map_multiset_result_l3.rs \
            scripts/verify-ordinary-map-multiset-result-l3.sh \
            "${fixture_paths[@]}"
    )
}

source_snapshot > "$evidence_dir/source.before.sha256"
sha256sum -- \
    "$repo_root/tests/ordinary_map_multiset_result_l3.rs" \
    "$script_dir/$(basename -- "${BASH_SOURCE[0]}")" \
    "$repo_root/${fixture_paths[0]}" \
    "$repo_root/${fixture_paths[1]}" \
    "$repo_root/${fixture_paths[2]}" \
    > "$evidence_dir/gate-source-identities.sha256"
{
    printf 'repo_root=%s\nCARGO_TARGET_DIR=%s\nLLVM_SYS_181_PREFIX=%s\n' \
        "$repo_root" "$target_dir" "$llvm_prefix"
    printf 'feature=%s\nstrict_memcheck_required=true\n' "$runtime_feature"
    printf 'required_fixtures=record,record_unit,clone\n'
    printf 'required_native_cases=success+NewErr+everySetErr\n'
    printf 'required_counter_assertions=all_final_owner_domains_zero\n'
    printf 'required_memcheck=zero_errors_and_zero_definite_indirect_possible_leaks\n'
    printf 'current_route_gate=must_reject_if_exact_multi_set_result_route_is_absent\n'
} > "$evidence_dir/runner.identity.txt"

status=0
test_status=not_run
source_stable=false
finalize() {
    local shell_status=$?
    trap - EXIT
    [[ "$shell_status" == 0 ]] || status="$shell_status"

    source_snapshot > "$evidence_dir/source.after.sha256" || status=98
    if cmp -s "$evidence_dir/source.before.sha256" "$evidence_dir/source.after.sha256"; then
        source_stable=true
    else
        status=98
        diff -u "$evidence_dir/source.before.sha256" "$evidence_dir/source.after.sha256" \
            > "$evidence_dir/source.diff" || true
    fi

    local passed=false
    if [[ "$status" == 0 && "$test_status" == 0 && "$source_stable" == true ]]; then
        passed=true
    fi
    {
        printf 'test_status=%s\nrunner_exit_code=%s\nsource_tree_unchanged=%s\n' \
            "$test_status" "$status" "$source_stable"
        printf 'native_execution_count=0\nstrict_memcheck_run_count=0\n'
        printf 'route_api_gap=%s\n' \
            "$([[ "$test_status" == not_run ]] && printf 'not-probed' || printf 'inspect-fixture-route-probe-logs')"
        printf 'l3_accepted=%s\n' "$passed"
    } > "$evidence_dir/gate.status"
    (
        cd -- "$evidence_dir"
        find . -type f ! -name SHA256SUMS -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum
    ) > "$evidence_dir/SHA256SUMS"
    if [[ "$passed" == true ]]; then
        printf 'Multi-Set Result native/Memcheck L3 passed; evidence: %s\n' "$evidence_dir"
    else
        printf 'Multi-Set Result L3 did not pass (runner exit %s, test %s); evidence: %s\n' \
            "$status" "$test_status" "$evidence_dir" >&2
    fi
    exit "$status"
}
trap finalize EXIT

test_arguments=(
    test
    --manifest-path "$repo_root/Cargo.toml"
    --features "$runtime_feature"
    --test "$test_target"
    --
    --ignored
    --nocapture
    --test-threads=1
)
{
    printf 'LLVM_SYS_181_PREFIX=%q CARGO_TARGET_DIR=%q MIMI_ORDINARY_MAP_MULTISET_RESULT_L3_EVIDENCE_DIR=%q ' \
        "$llvm_prefix" "$target_dir" "$evidence_dir"
    printf '%q ' cargo "${test_arguments[@]}"
    printf '\n'
} > "$evidence_dir/command.txt"
printf '%s\0' cargo "${test_arguments[@]}" > "$evidence_dir/command.argv.nul"

set +e
cargo "${test_arguments[@]}" \
    > "$evidence_dir/cargo.stdout.log" \
    2> "$evidence_dir/cargo.stderr.log"
test_status=$?
set -e
printf 'exit_code=%s\nsuccess=%s\n' \
    "$test_status" "$([[ "$test_status" == 0 ]] && printf true || printf false)" \
    > "$evidence_dir/cargo.status"
if [[ "$test_status" != 0 ]]; then
    status="$test_status"
fi
exit "$status"
