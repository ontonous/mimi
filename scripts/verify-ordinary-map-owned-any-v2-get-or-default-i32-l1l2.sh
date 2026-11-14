#!/usr/bin/env bash
# Source-bound L1/L2 evidence gate for the narrow get_or_default<i32> Map route.
set -uo pipefail

usage() {
    printf 'Usage: %s [--help]\n' "$0"
}

if [[ ${1:-} == --help ]]; then
    usage
    exit 0
fi
if [[ $# -ne 0 ]]; then
    usage >&2
    exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)" || exit 2
run_pid="$$"
run_id="${timestamp}-${run_pid}-${RANDOM}${RANDOM}"
artifact_dir="$repo_root/evidence/0.1.11-dev/gates/map-owned-any-v2-get-or-default-i32-l1l2-$run_id"
cargo_target_dir="/dev/shm/mimi-get-or-default-i32-target-$run_id"
cargo_tmpdir="$cargo_target_dir/tmp"

if [[ -e "$artifact_dir" || -L "$artifact_dir" ]]; then
    printf 'Refusing to overwrite existing evidence directory: %s\n' "$artifact_dir" >&2
    exit 2
fi
mkdir -p -- "$(dirname -- "$artifact_dir")" || exit 2
if ! mkdir -- "$artifact_dir"; then
    printf 'Could not create unique evidence directory: %s\n' "$artifact_dir" >&2
    exit 2
fi

cd -- "$repo_root" || exit 2

test_file="tests/canonical_mir_ordinary_map_owned_any_v2_get_or_default_i32.rs"
hit_fixture="tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_hit.mimi"
miss_fixture="tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_miss.mimi"
zero_default_miss_fixture="tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_zero_default_miss.mimi"
route_profile="ordinary-map-owned-any-v2-get-or-default-i32-v1"
test_target="canonical_mir_ordinary_map_owned_any_v2_get_or_default_i32"
llvm_prefix="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
source_required_paths=(
    "$test_file"
    tests/support/memcheck_summary.rs
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_hit.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_miss.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_zero_default_miss.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_reachable_helper.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_wrong_string.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_wrong_bool.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_dynamic_key.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_non_i32_default.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_non_i32_instance.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_multiple_callsites.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_missing_pair_drop.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_duplicate_pair_drop.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_early_pair_drop.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_missing_root_drop.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_duplicate_root_drop.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_extra_mutation.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_alias.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_actor_caller.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_actor_shared_helper.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_unreachable_caller.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_user_helper.mimi
    src/core/mir/route.rs
    src/core/mir/mod.rs
    src/core/mir/receipt.rs
    src/core/mir/reference.rs
    src/core/mir/islands.rs
    src/core/mir/lower.rs
    src/core/mir/managed_any.rs
    src/core/mir/option_island.rs
    src/interp/bytecode/mir.rs
    src/interp/bytecode/mod.rs
    src/interp/bytecode/instr.rs
    src/interp/bytecode/disasm.rs
    src/interp/bytecode/vm.rs
    src/codegen/mir/eligibility.rs
    src/codegen/mir/mod.rs
    src/codegen/mir/ordinary_map_owned_any_v2.rs
    src/codegen/compile.rs
    src/core/cfg/resource_lower.rs
    src/core/ownership.rs
    src/core/resolved/mod.rs
    src/main/canonical_dispatch.rs
    src/main/build.rs
    src/main/run.rs
    src/main/verify.rs
    src/verifier/mir_capability.rs
    src/verifier/mir.rs
    src/verifier/mod.rs
    src/lib.rs
    src/core/mod.rs
    std/maps.mimi
    .gitignore
    scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l1l2.sh
    scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l3.sh
    Cargo.toml
    Cargo.lock
    build.rs
)
for optional_source in rust-toolchain rust-toolchain.toml .cargo/config .cargo/config.toml; do
    if [[ -e "$optional_source" || -L "$optional_source" ]]; then
        source_required_paths+=("$optional_source")
    fi
done

overall_state=INCOMPLETE
overall_exit=2
source_before_ok=false
source_after_ok=false
source_unchanged=false
git_identity_unchanged=false
preflight_missing=0
preflight_failures=0
test_count=0
test_failure_count=0
toolchain_ok=true
evidence_visible=false
finalized=false

source_snapshot() {
    local phase="$1"
    local paths_file="$artifact_dir/source-files.$phase.nul"
    local text_paths_file="$artifact_dir/source-files.$phase.txt"
    local hashes_file="$artifact_dir/source-hashes.$phase.tsv"
    local source_path source_hash source_kind required_path ignore_status
    local status=0

    if ! (
        cd -- "$repo_root"
        git ls-files --cached --others --exclude-standard -z -- \
            src \
            "$test_file" \
            tests/support/memcheck_summary.rs \
            tests/fixtures \
            std/maps.mimi .gitignore scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l1l2.sh \
            scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l3.sh \
            Cargo.toml Cargo.lock build.rs rust-toolchain rust-toolchain.toml \
            .cargo/config .cargo/config.toml \
            "${source_required_paths[@]}" \
            | LC_ALL=C sort -zu
    ) > "$paths_file"; then
        printf 'source path enumeration failed for %s\n' "$phase" >&2
        return 1
    fi

    tr '\0' '\n' < "$paths_file" > "$text_paths_file" || return 1
    for required_path in "${source_required_paths[@]}"; do
        if [[ ! -f "$required_path" && ! -L "$required_path" ]]; then
            printf 'required source input is missing: %s\n' "$required_path" >&2
            status=1
        fi
        if git check-ignore -q -- "$required_path"; then
            printf 'required source input is ignored: %s\n' "$required_path" >&2
            status=1
        else
            ignore_status=$?
            if [[ "$ignore_status" != 1 ]]; then
                printf 'could not check ignore status for required source input: %s (exit %s)\n' \
                    "$required_path" "$ignore_status" >&2
                status=1
            fi
        fi
        if ! rg -F -x -q -- "$required_path" "$text_paths_file"; then
            printf 'required source input is absent from the source snapshot: %s\n' \
                "$required_path" >&2
            status=1
        fi
    done
    : > "$hashes_file" || return 1
    while IFS= read -r -d '' source_path; do
        if git ls-files --error-unmatch -- "$source_path" >/dev/null 2>&1; then
            source_kind=tracked
        else
            source_kind=untracked
        fi
        if [[ -f "$source_path" || -L "$source_path" ]]; then
            source_hash="$(sha256sum -- "$source_path" | awk '{print $1}')"
            if [[ -z "$source_hash" ]]; then
                printf 'could not hash source path: %s\n' "$source_path" >&2
                status=1
                source_hash=HASH_ERROR
            fi
        else
            source_hash=MISSING
            if [[ "$source_kind" != tracked ]]; then
                printf 'untracked source path disappeared during snapshot: %s\n' "$source_path" >&2
                status=1
            fi
        fi
        printf '%s\t%s\t%s\n' "$source_kind" "$source_hash" "$source_path" >> "$hashes_file" || return 1
    done < "$paths_file"
    return "$status"
}

capture_git_state() {
    local phase="$1"
    {
        printf 'captured_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf 'branch='
        git branch --show-current
        printf 'head='
        git rev-parse HEAD
        printf 'status_begin\n'
        git status --short --branch --untracked-files=all -- \
            src \
            "$test_file" \
            tests/support/memcheck_summary.rs \
            tests/fixtures \
            std/maps.mimi .gitignore scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l1l2.sh \
            scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l3.sh \
            Cargo.toml Cargo.lock build.rs rust-toolchain rust-toolchain.toml
        printf 'status_end\n'
    } > "$artifact_dir/git-state.$phase.txt" 2>&1
}

record_check() {
    local category="$1"
    local name="$2"
    local result="$3"
    local detail="$4"
    printf '%s\t%s\t%s\t%s\n' "$category" "$name" "$result" "$detail" \
        >> "$artifact_dir/checks.tsv"
    if [[ "$result" == INCOMPLETE ]]; then
        preflight_missing=$((preflight_missing + 1))
    elif [[ "$result" == FAIL ]]; then
        preflight_failures=$((preflight_failures + 1))
    fi
}

check_file_marker() {
    local label="$1"
    local path="$2"
    local marker="$3"
    if [[ -f "$path" ]] && rg -Fq -- "$marker" "$path"; then
        record_check source-preflight "$label" PASS "$path contains required marker"
    else
        record_check source-preflight "$label" INCOMPLETE "$path missing marker: $marker"
    fi
}

record_literal_count() {
    local label="$1"
    local path="$2"
    local literal="$3"
    local expected_count="$4"
    local matches count
    matches="$(rg -o -F -- "$literal" "$path" 2>/dev/null || true)"
    if [[ -n "$matches" ]]; then
        count="$(printf '%s\n' "$matches" | wc -l | tr -d '[:space:]')"
    else
        count=0
    fi
    if [[ "$count" == "$expected_count" ]]; then
        record_check fixture-contract "$label" PASS "count=$count"
    else
        record_check fixture-contract "$label" FAIL "expected_count=$expected_count observed_count=$count"
    fi
}

check_fixture_contract() {
    local label="$1"
    local path="$2"
    local stored_key="$3"
    local stored_value="$4"
    local queried_key="$5"
    local default_value="$6"
    local expected_stdout="$7"
    local call_line empty_drop_line root_drop_line

    if [[ ! -f "$path" ]]; then
        record_check fixture-contract "$label" FAIL "fixture is missing: $path"
        return
    fi
    record_file_line() {
        local field="$1"
        local literal="$2"
        local found_line
        found_line="$(rg -n -F -- "$literal" "$path" | head -n 1 | cut -d: -f1)"
        printf -v "$field" '%s' "${found_line:-0}"
    }
    record_literal_count "$label map_new" "$path" 'map_new()' 1
    record_literal_count "$label std::maps import" "$path" 'use std::maps;' 1
    record_literal_count "$label map_set" "$path" 'map_set(' 1
    record_literal_count "$label get_or_default" "$path" 'get_or_default::<i32>(' 1
    record_literal_count "$label drop(empty)" "$path" 'drop(empty)' 1
    record_literal_count "$label drop(root)" "$path" 'drop(root)' 1
    record_literal_count "$label stored key and value" "$path" "map_set(empty, \"$stored_key\", $stored_value)" 1
    record_literal_count "$label queried key and per-fixture default" "$path" \
        "get_or_default::<i32>(root, \"$queried_key\", $default_value)" 1
    record_literal_count "$label observed output" "$path" 'println(observed)' 1
    if (( stored_value != default_value )); then
        record_check fixture-contract "$label stored value differs from default" PASS \
            "stored_value=$stored_value default_value=$default_value expected_stdout=$expected_stdout"
    else
        record_check fixture-contract "$label stored value differs from default" FAIL \
            "stored_value=$stored_value default_value=$default_value would not distinguish selected values"
    fi
    record_file_line call_line "let observed = get_or_default::<i32>(root, \"$queried_key\", $default_value)"
    record_file_line empty_drop_line 'drop(empty)'
    record_file_line root_drop_line 'drop(root)'
    if (( call_line > 0 && empty_drop_line > call_line && root_drop_line > call_line )); then
        record_check fixture-contract "$label drop ordering" PASS \
            "call_line=$call_line empty_drop_line=$empty_drop_line root_drop_line=$root_drop_line"
    else
        record_check fixture-contract "$label drop ordering" FAIL \
            "call_line=$call_line empty_drop_line=$empty_drop_line root_drop_line=$root_drop_line"
    fi
}

capture_tool() {
    local label="$1"
    shift
    local status
    printf '%q ' "$@" > "$artifact_dir/toolchain-$label.command.txt"
    printf '\n' >> "$artifact_dir/toolchain-$label.command.txt"
    "$@" > "$artifact_dir/toolchain-$label.stdout" 2> "$artifact_dir/toolchain-$label.stderr"
    status=$?
    printf 'exit_code=%s\n' "$status" > "$artifact_dir/toolchain-$label.exit-status"
    if [[ "$status" != 0 ]]; then
        toolchain_ok=false
    fi
}

write_status() {
    {
        printf 'state=%s\nexit_code=%s\n' "$overall_state" "$overall_exit"
        printf 'profile=%s\nlevel=L1/L2\nrun_id=%s\n' "$route_profile" "$run_id"
        printf 'branch_before=%s\nhead_before=%s\n' "${branch_before:-unavailable}" "${head_before:-unavailable}"
        printf 'branch_after=%s\nhead_after=%s\n' "${branch_after:-unavailable}" "${head_after:-unavailable}"
        printf 'git_identity_unchanged=%s\n' "$git_identity_unchanged"
        printf 'preflight_missing_count=%s\npreflight_failure_count=%s\nfocused_test_count=%s\nfocused_test_failure_count=%s\n' \
            "$preflight_missing" "$preflight_failures" "$test_count" "$test_failure_count"
        printf 'source_before_captured=%s\nsource_after_captured=%s\nsource_unchanged=%s\n' \
            "$source_before_ok" "$source_after_ok" "$source_unchanged"
        printf 'toolchain_capture_ok=%s\nevidence_visible=%s\n' "$toolchain_ok" "$evidence_visible"
        printf 'cargo_test_invocations_executed_by_gate=%s\n' "$test_count"
        printf 'global_ordinary_map_owned_any_v2_production_ready=false\n'
    } > "$artifact_dir/gate.status"
}

finalize() {
    local shell_status=$?
    trap - EXIT
    if [[ "$shell_status" != 0 && "$overall_state" == PASS ]]; then
        overall_state=FAIL
        overall_exit=1
    fi

    if source_snapshot after > /dev/null 2> "$artifact_dir/source-snapshot.after.stderr"; then
        source_after_ok=true
    else
        source_after_ok=false
        if [[ "$overall_state" != FAIL ]]; then
            overall_state=FAIL
            overall_exit=1
        fi
    fi
    capture_git_state after || true
    branch_after="$(git branch --show-current 2>/dev/null || printf unavailable)"
    head_after="$(git rev-parse HEAD 2>/dev/null || printf unavailable)"
    if [[ "$branch_before" != "$branch_after" || "$head_before" != "$head_after" ]]; then
        git_identity_unchanged=false
        if [[ "$overall_state" != FAIL ]]; then
            overall_state=FAIL
            overall_exit=1
        fi
        printf 'branch_before=%s\nhead_before=%s\nbranch_after=%s\nhead_after=%s\n' \
            "$branch_before" "$head_before" "$branch_after" "$head_after" \
            > "$artifact_dir/git-identity-change.txt"
    else
        git_identity_unchanged=true
    fi
    if [[ "$source_before_ok" == true && "$source_after_ok" == true ]] \
        && cmp -s "$artifact_dir/source-files.before.nul" "$artifact_dir/source-files.after.nul" \
        && cmp -s "$artifact_dir/source-hashes.before.tsv" "$artifact_dir/source-hashes.after.tsv"; then
        source_unchanged=true
    else
        source_unchanged=false
        if [[ "$overall_state" != FAIL ]]; then
            overall_state=FAIL
            overall_exit=1
        fi
        if [[ -f "$artifact_dir/source-hashes.before.tsv" && -f "$artifact_dir/source-hashes.after.tsv" ]]; then
            diff -u "$artifact_dir/source-hashes.before.tsv" "$artifact_dir/source-hashes.after.tsv" \
                > "$artifact_dir/source-hashes.diff" || true
        fi
        if [[ -f "$artifact_dir/source-files.before.txt" && -f "$artifact_dir/source-files.after.txt" ]]; then
            diff -u "$artifact_dir/source-files.before.txt" "$artifact_dir/source-files.after.txt" \
                > "$artifact_dir/source-files.diff" || true
        fi
    fi

    if git check-ignore -q -- "${artifact_dir#"$repo_root"/}/gate.status"; then
        evidence_visible=false
        if [[ "$overall_state" != FAIL ]]; then
            overall_state=FAIL
            overall_exit=1
        fi
        printf 'evidence_visibility=FAIL\npath=%s\n' "${artifact_dir#"$repo_root"/}/gate.status" \
            > "$artifact_dir/evidence-visibility.txt"
    else
        local ignore_status=$?
        if [[ "$ignore_status" == 1 ]]; then
            evidence_visible=true
            printf 'evidence_visibility=PASS\npath=%s\n' "${artifact_dir#"$repo_root"/}/gate.status" \
                > "$artifact_dir/evidence-visibility.txt"
        else
            evidence_visible=false
            printf 'evidence_visibility=FAIL\ncheck_ignore_exit_code=%s\n' "$ignore_status" \
                > "$artifact_dir/evidence-visibility.txt"
            if [[ "$overall_state" != FAIL ]]; then
                overall_state=FAIL
                overall_exit=1
            fi
        fi
    fi

    write_status
    if (cd -- "$artifact_dir" && find . -type f ! -name SHA256SUMS -print0 \
        | LC_ALL=C sort -z | xargs -0 -r sha256sum) > "$artifact_dir/SHA256SUMS"; then
        :
    else
        overall_state=FAIL
        overall_exit=1
        write_status
        (cd -- "$artifact_dir" && find . -type f ! -name SHA256SUMS -print0 \
            | LC_ALL=C sort -z | xargs -0 -r sha256sum) > "$artifact_dir/SHA256SUMS" || true
    fi
    finalized=true
    if [[ "$overall_state" == PASS ]]; then
        printf 'get_or_default<i32> L1/L2 passed; evidence: %s\n' "$artifact_dir"
    elif [[ "$overall_state" == INCOMPLETE ]]; then
        printf 'get_or_default<i32> L1/L2 incomplete; route/toolchain preflight is still landing; evidence: %s\n' \
            "$artifact_dir" >&2
    else
        printf 'get_or_default<i32> L1/L2 failed (exit %s); evidence: %s\n' \
            "$overall_exit" "$artifact_dir" >&2
    fi
    exit "$overall_exit"
}
trap finalize EXIT

touch "$artifact_dir/checks.tsv"
printf 'category\tname\tresult\tdetail\n' > "$artifact_dir/checks.tsv"

if [[ -d /dev/shm && -w /dev/shm && ! -e "$cargo_target_dir" && ! -L "$cargo_target_dir" ]]; then
    record_check source-preflight 'isolated Cargo target directory' PASS \
        "CARGO_TARGET_DIR=$cargo_target_dir; cargo execution is serialized"
    if mkdir -p -- "$cargo_tmpdir"; then
        record_check source-preflight 'isolated Cargo temporary directory' PASS \
            "TMPDIR=$cargo_tmpdir keeps linker scratch files on /dev/shm"
    else
        record_check source-preflight 'isolated Cargo temporary directory' FAIL \
            "could not create TMPDIR=$cargo_tmpdir"
    fi
else
    record_check source-preflight 'isolated Cargo target directory' INCOMPLETE \
        "need writable /dev/shm and a fresh path at $cargo_target_dir"
fi

branch_before="$(git branch --show-current 2>/dev/null || printf unavailable)"
head_before="$(git rev-parse HEAD 2>/dev/null || printf unavailable)"
capture_git_state before || true
if source_snapshot before > /dev/null 2> "$artifact_dir/source-snapshot.before.stderr"; then
    source_before_ok=true
else
    source_before_ok=false
    record_check source-snapshot before FAIL 'could not enumerate and hash relevant source files'
fi

{
    printf 'repo_root=%s\nrun_id=%s\nrun_timestamp_utc=%s\nrun_pid=%s\n' \
        "$repo_root" "$run_id" "$timestamp" "$run_pid"
    printf 'branch=%s\nhead=%s\n' "$branch_before" "$head_before"
    printf 'test_target=%s\nroute_profile=%s\n' "$test_target" "$route_profile"
    printf 'llvm_sys_181_prefix=%s\n' "$llvm_prefix"
    printf 'cargo_target_dir=%s\ncargo_target_root=dev-shm\ncargo_tmpdir=%s\n' \
        "$cargo_target_dir" "$cargo_tmpdir"
    printf 'cargo_build_jobs=1\ncargo_profile_test_debug=0\ncargo_profile_dev_debug=0\ncargo_incremental=1\ncargo_term_color=never\ntest_threads=1\n'
} > "$artifact_dir/run-metadata.txt"

printf 'expected_hit_default=99\nexpected_hit_stdout=0\\n\n' > "$artifact_dir/expected-results.txt"
printf 'expected_miss_default=73\nexpected_miss_stdout=73\\n\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'expected_zero_default_miss_default=0\nexpected_zero_default_miss_stdout=0\\n\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'expected_route_profile=%s\n' "$route_profile" >> "$artifact_dir/expected-results.txt"
printf 'expected_positive_matrix=hit(default=99,output=0)+miss(default=73,output=73)+zero-default-miss(default=0,output=0)+main-reachable-helper(hit,output=0)\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'expected_near_misses=diagnostic_or_non_profile_route+no_legacy_fallback+no_unearned_native_or_verifier_receipt\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'focused_tests=L1:get_or_default_i32_hit_and_misses_have_one_profile_and_match_reference_bytecode\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'focused_tests=L1:get_or_default_i32_typed_mir_receipt_rejects_checker_binding_forgery\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'focused_tests=L1-lib:core::mir::receipt::tests::get_or_default_i32_mir_receipt_rejects_forged_crosscall_bindings\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'focused_tests=L1-lib:core::mir::receipt::tests::get_or_default_i32_mir_receipt_rechecks_main_direct_call_reachability\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'focused_tests=L2:get_or_default_i32_cli_default_and_explicit_mir_routes_match_for_all_consumers\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'focused_tests=L2:get_or_default_i32_near_miss_fixtures_stay_outside_the_profile\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'focused_tests=L2:get_or_default_i32_actor_method_caller_is_rejected_before_profile_admission\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'focused_tests=L2:same_named_user_get_or_default_helper_does_not_receive_stdlib_profile\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'focused_tests=L2:get_or_default_i32_unreachable_caller_is_rejected_before_profile_admission\n' \
    >> "$artifact_dir/expected-results.txt"
printf 'focused_tests=L2:get_or_default_i32_actor_tagged_shared_helper_is_rejected_without_legacy_fallback\n' \
    >> "$artifact_dir/expected-results.txt"

capture_tool cargo cargo --version
capture_tool rustc rustc --version --verbose
capture_tool git git --version
if [[ -x "$llvm_prefix/bin/llvm-config" ]]; then
    llvm_config_bin="$llvm_prefix/bin/llvm-config"
elif [[ -x "$llvm_prefix/llvm-config" ]]; then
    llvm_config_bin="$llvm_prefix/llvm-config"
elif command -v llvm-config >/dev/null 2>&1; then
    llvm_config_bin="$(command -v llvm-config)"
else
    llvm_config_bin=''
fi
if [[ -n "$llvm_config_bin" ]]; then
    capture_tool llvm-config "$llvm_config_bin" --version
else
    toolchain_ok=false
    printf 'llvm-config unavailable for LLVM_SYS_181_PREFIX=%s\n' "$llvm_prefix" \
        > "$artifact_dir/toolchain-llvm-config.stderr"
    printf 'exit_code=127\n' > "$artifact_dir/toolchain-llvm-config.exit-status"
fi

check_file_marker 'route profile enum' src/core/mir/route.rs 'OrdinaryMapOwnedAnyV2GetOrDefaultI32'
check_file_marker 'Checker admission variant' src/core/mir/route.rs 'GetOrDefaultI32'
check_file_marker 'dedicated MIR fused operation' src/core/mir/mod.rs \
    'OrdinaryMapOwnedAnyV2GetOrDefaultI32'
check_file_marker 'typed MIR receipt API implementation' src/core/mir/receipt.rs 'ordinary_map_owned_any_v2_get_or_default_i32_receipts('
check_file_marker 'typed MIR receipt validator implementation' src/core/mir/receipt.rs 'OrdinaryMapOwnedAnyV2GetOrDefaultI32MirReceipt'
check_file_marker 'typed MIR argument-to-formal binding' src/core/mir/receipt.rs 'call_parameter_order'
check_file_marker 'typed MIR caller-root View assertion' "$test_file" \
    'caller Map argument is passed to the canonical helper'
check_file_marker 'CheckedProgram receipt revalidation method' src/core/mir/receipt.rs \
    'pub(crate) fn validate_against_checked_program('
check_file_marker 'dedicated route validator' src/core/mir/route.rs \
    'pub fn validate_ordinary_map_owned_any_v2_get_or_default_i32_route('
check_file_marker 'dedicated route binds live CheckedProgram receipt' src/core/mir/route.rs \
    'receipt.validate_against_checked_program(checked)?;'
check_file_marker 'typed MIR receipt validator' "$test_file" 'validate_against_program(&route.program)'
check_file_marker 'dedicated CheckedProgram-bound Bytecode API implementation' src/interp/bytecode/mir.rs \
    'pub fn compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_i32_route('
check_file_marker 'dedicated CheckedProgram-bound Bytecode API export' src/interp/bytecode/mod.rs \
    'compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_i32_route'
check_file_marker 'reference and AST-free checked Bytecode API call' "$test_file" \
    'compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_i32_route('
check_file_marker 'Reference interpreter consumer' src/core/mir/reference.rs 'MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultI32'
check_file_marker 'native route consumer' src/codegen/mir/eligibility.rs 'validate_checked_mir_native_get_or_default_i32_route'
check_file_marker 'bytecode route consumer' src/interp/bytecode/mir.rs 'materialize_ordinary_map_owned_any_v2_get_or_default_i32_bindings'
check_file_marker 'MIR verifier route consumer' src/verifier/mir_capability.rs 'validate_mir_capabilities_with_ordinary_map_owned_any_v2_get_or_default_i32_route'
check_file_marker 'CLI route preflight' src/main/canonical_dispatch.rs \
    'preflight_ordinary_map_owned_any_v2_get_or_default_i32('
check_file_marker 'CLI dedicated Bytecode consumer' src/main/canonical_dispatch.rs \
    'compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_i32_route('
check_file_marker 'CLI dedicated native consumer' src/main/canonical_dispatch.rs \
    'compile_checked_mir_native_with_ordinary_map_owned_any_v2_get_or_default_i32_route('
check_file_marker 'CLI dedicated verifier consumer' src/main/canonical_dispatch.rs \
    'verify_mir_with_ordinary_map_owned_any_v2_get_or_default_i32_route('

check_fixture_contract hit "$hit_fixture" hits 0 hits 99 '0\\n'
check_fixture_contract miss "$miss_fixture" other 0 hits 73 '73\\n'
check_fixture_contract zero-default-miss "$zero_default_miss_fixture" other 17 hits 0 '0\\n'
check_file_marker 'extra mutation near-miss fixture' \
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_extra_mutation.mimi \
    'map_set(initial, "extra", 1)'
check_file_marker 'alias near-miss fixture' \
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_alias.mimi \
    'let alias = root'
check_file_marker 'Actor caller negative fixture' \
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_actor_caller.mimi \
    'actor Reader'
check_file_marker 'main-reachable ordinary helper fixture' \
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_reachable_helper.mimi \
    'let observed = lookup()'
check_file_marker 'orphan caller fixture' \
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_unreachable_caller.mimi \
    'func cold() -> i32'
check_file_marker 'Actor-shared helper fixture' \
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_actor_shared_helper.mimi \
    'func lookup_from_actor() -> i32'
check_file_marker 'Actor-shared helper fixture has ordinary main path' \
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_actor_shared_helper.mimi \
    'let observed = lookup()'
record_literal_count 'missing-root-drop fixture has no root Drop' \
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_missing_root_drop.mimi \
    'drop(root)' 0
record_literal_count 'duplicate-root-drop fixture has two root Drops' \
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_duplicate_root_drop.mimi \
    'drop(root)' 2
check_file_marker 'expected hit output' "$test_file" 'const EXPECTED_HIT_STDOUT: &[u8] = b"0\n";'
check_file_marker 'expected miss output' "$test_file" 'const EXPECTED_MISS_STDOUT: &[u8] = b"73\n";'
check_file_marker 'canonical profile identity' "$test_file" "$route_profile"
check_file_marker 'Checker-to-MIR/reference/bytecode L1 test' "$test_file" 'fn get_or_default_i32_hit_and_misses_have_one_profile_and_match_reference_bytecode()'
check_file_marker 'full hit Checker/MIR admission equality' "$test_file" 'hit_admission, hit_route.admission'
check_file_marker 'full miss Checker/MIR admission equality' "$test_file" 'miss_admission, miss_route.admission'
check_file_marker 'full zero-default Checker/MIR admission equality' "$test_file" 'zero_default_miss_admission,'
check_file_marker 'observed output recorder' "$test_file" 'MIMI_GET_OR_DEFAULT_I32_OUTPUT label={label} bytes_hex={bytes_hex}'
check_file_marker 'zero-default miss output' "$test_file" 'const EXPECTED_ZERO_DEFAULT_MISS_STDOUT: &[u8] = b"0\n";'
check_file_marker 'missing root Drop negative fixture' \
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_missing_root_drop.mimi \
    'drop(empty)'
check_file_marker 'duplicate root Drop negative fixture' \
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_duplicate_root_drop.mimi \
    'drop(root)'
check_file_marker 'receipt Checker-binding forgery test' "$test_file" \
    'fn get_or_default_i32_typed_mir_receipt_rejects_checker_binding_forgery()'
check_file_marker 'lib test path root core' src/lib.rs 'pub mod core;'
check_file_marker 'lib test path core::mir' src/core/mod.rs 'pub mod mir;'
check_file_marker 'lib test path core::mir::receipt' src/core/mir/mod.rs 'mod receipt;'
check_file_marker 'lib test path core::mir::receipt::tests' src/core/mir/receipt.rs 'mod tests {'
check_file_marker 'MIR receipt forgery test core::mir::receipt::tests::get_or_default_i32_mir_receipt_rejects_forged_crosscall_bindings' src/core/mir/receipt.rs \
    'fn get_or_default_i32_mir_receipt_rejects_forged_crosscall_bindings()'
check_file_marker 'MIR main-reachability receipt test core::mir::receipt::tests::get_or_default_i32_mir_receipt_rechecks_main_direct_call_reachability' src/core/mir/receipt.rs \
    'fn get_or_default_i32_mir_receipt_rechecks_main_direct_call_reachability()'
check_file_marker 'CLI L2 test' "$test_file" 'fn get_or_default_i32_cli_default_and_explicit_mir_routes_match_for_all_consumers()'
check_file_marker 'near-miss L2 test' "$test_file" 'fn get_or_default_i32_near_miss_fixtures_stay_outside_the_profile()'
check_file_marker 'Actor-owner exclusion L2 test' "$test_file" \
    'fn get_or_default_i32_actor_method_caller_is_rejected_before_profile_admission()'
check_file_marker 'user-helper L2 test' "$test_file" 'fn same_named_user_get_or_default_helper_does_not_receive_stdlib_profile()'
check_file_marker 'orphan-caller L2 test' "$test_file" \
    'fn get_or_default_i32_unreachable_caller_is_rejected_before_profile_admission()'
check_file_marker 'Actor-shared-helper L2 test' "$test_file" \
    'fn get_or_default_i32_actor_tagged_shared_helper_is_rejected_without_legacy_fallback()'
check_file_marker 'strict native L3 test' "$test_file" \
    'fn get_or_default_i32_native_default_and_explicit_mir_are_memcheck_clean()'
check_file_marker 'strict native L3 gate script' \
    scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l3.sh \
    'MIMI_GET_OR_DEFAULT_I32_L3_ARTIFACT_DIR'

if [[ "$toolchain_ok" != true ]]; then
    record_check toolchain availability INCOMPLETE 'one or more toolchain version probes failed'
fi

if (( preflight_failures > 0 )); then
    overall_state=FAIL
    overall_exit=1
    printf 'execution=SKIPPED\nreason=fixture or source contract validation failed; no Cargo tests were started\n' \
        > "$artifact_dir/execution.status"
    printf 'preflight_failure_count=%s\n' "$preflight_failures" >> "$artifact_dir/execution.status"
elif (( preflight_missing > 0 )); then
    overall_state=INCOMPLETE
    overall_exit=2
    printf 'execution=SKIPPED\nreason=preflight incomplete; no Cargo tests were started\n' \
        > "$artifact_dir/execution.status"
    printf 'missing_preflight_count=%s\n' "$preflight_missing" >> "$artifact_dir/execution.status"
else
    overall_state=PASS
    overall_exit=0
    validate_observed_outputs() {
        local test_name="$1"
        local output_log="$2"
        local validation_file="$3"
        local expected_record index
        local -a expected_records=()
        local -a observed_records=()

        case "$test_name" in
            get_or_default_i32_hit_and_misses_have_one_profile_and_match_reference_bytecode)
                expected_records=(
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=hit-reference bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=hit-bytecode bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=miss-reference bytes_hex=37330a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=miss-bytecode bytes_hex=37330a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=zero-default-miss-reference bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=zero-default-miss-bytecode bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=reachable-helper-reference bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=reachable-helper-bytecode bytes_hex=300a'
                )
                ;;
            get_or_default_i32_cli_default_and_explicit_mir_routes_match_for_all_consumers)
                expected_records=(
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=hit-cli-run-default bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=hit-cli-run-explicit-mir bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=hit-native-build-default bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=hit-native-build-explicit-mir bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=miss-cli-run-default bytes_hex=37330a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=miss-cli-run-explicit-mir bytes_hex=37330a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=miss-native-build-default bytes_hex=37330a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=miss-native-build-explicit-mir bytes_hex=37330a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=zero-default-miss-cli-run-default bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=zero-default-miss-cli-run-explicit-mir bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=zero-default-miss-native-build-default bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=zero-default-miss-native-build-explicit-mir bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=reachable-helper-cli-run-default bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=reachable-helper-cli-run-explicit-mir bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=reachable-helper-native-build-default bytes_hex=300a'
                    'MIMI_GET_OR_DEFAULT_I32_OUTPUT label=reachable-helper-native-build-explicit-mir bytes_hex=300a'
                )
                ;;
            same_named_user_get_or_default_helper_does_not_receive_stdlib_profile)
                expected_records=()
                ;;
            get_or_default_i32_near_miss_fixtures_stay_outside_the_profile)
                expected_records=()
                ;;
            get_or_default_i32_actor_method_caller_is_rejected_before_profile_admission)
                expected_records=()
                ;;
            get_or_default_i32_typed_mir_receipt_rejects_checker_binding_forgery)
                expected_records=()
                ;;
            core::mir::receipt::tests::get_or_default_i32_mir_receipt_rejects_forged_crosscall_bindings)
                expected_records=()
                ;;
            core::mir::receipt::tests::get_or_default_i32_mir_receipt_rechecks_main_direct_call_reachability)
                expected_records=()
                ;;
            get_or_default_i32_unreachable_caller_is_rejected_before_profile_admission)
                expected_records=()
                ;;
            get_or_default_i32_actor_tagged_shared_helper_is_rejected_without_legacy_fallback)
                expected_records=()
                ;;
            *)
                printf 'validation=FAIL\nreason=unrecognized focused test %s\n' "$test_name" \
                    > "$validation_file"
                return 1
                ;;
        esac

        mapfile -t observed_records < <(rg '^MIMI_GET_OR_DEFAULT_I32_OUTPUT ' "$output_log" || true)
        if (( ${#observed_records[@]} != ${#expected_records[@]} )); then
            printf 'validation=FAIL\nexpected_record_count=%s\nobserved_record_count=%s\n' \
                "${#expected_records[@]}" "${#observed_records[@]}" > "$validation_file"
            return 1
        fi
        for index in "${!expected_records[@]}"; do
            expected_record="${expected_records[$index]}"
            if [[ "${observed_records[$index]}" != "$expected_record" ]]; then
                printf 'validation=FAIL\nrecord_index=%s\nexpected=%s\nobserved=%s\n' \
                    "$index" "$expected_record" "${observed_records[$index]}" > "$validation_file"
                return 1
            fi
        done
        {
            printf 'validation=PASS\nexpected_record_count=%s\n' "${#expected_records[@]}"
            printf 'observed_begin\n'
            printf '%s\n' "${observed_records[@]}"
            printf 'observed_end\n'
        } > "$validation_file"
        return 0
    }
    run_focused_test() {
        local level="$1"
        local test_name="$2"
        local target_kind="${3:-integration}"
        local name="$level-$test_name"
        local test_dir="$artifact_dir/tests/$name"
        local status=0
        local validation=PASS
        local reason=''
        local -a cargo_target_args=()
        if [[ "$target_kind" == lib ]]; then
            cargo_target_args=(--lib)
        else
            cargo_target_args=(--test "$test_target")
        fi
        local -a command=(
            env
            "CARGO_TARGET_DIR=$cargo_target_dir"
            "TMPDIR=$cargo_tmpdir"
            CARGO_BUILD_JOBS=1
            CARGO_PROFILE_TEST_DEBUG=0
            CARGO_PROFILE_DEV_DEBUG=0
            CARGO_INCREMENTAL=1
            CARGO_TERM_COLOR=never
            "LLVM_SYS_181_PREFIX=$llvm_prefix"
            cargo test --manifest-path "$repo_root/Cargo.toml"
            "${cargo_target_args[@]}" "$test_name"
            -- --exact --show-output --test-threads=1
        )

        mkdir -p -- "$test_dir" || {
            test_failure_count=$((test_failure_count + 1))
            overall_state=FAIL
            overall_exit=1
            return
        }
        test_count=$((test_count + 1))
        printf '%q ' "${command[@]}" > "$test_dir/command.txt"
        printf '\n' >> "$test_dir/command.txt"
        printf 'cwd=%q\n' "$repo_root" > "$test_dir/working-directory.txt"
        "${command[@]}" > "$test_dir/stdout" 2> "$test_dir/stderr"
        status=$?
        printf '%s\n' "$status" > "$test_dir/exit-status"

        if [[ "$status" != 0 ]]; then
            validation=FAIL
            reason="focused cargo test exited $status"
        elif ! rg -Fq -- "test $test_name ..." "$test_dir/stdout"; then
            validation=FAIL
            reason='test harness did not start the exact filtered test'
        elif ! rg -q '^test result: ok\. 1 passed; 0 failed;' "$test_dir/stdout"; then
            validation=FAIL
            reason='test harness summary did not report exactly one passing focused test'
        elif ! validate_observed_outputs "$test_name" "$test_dir/stdout" \
            "$test_dir/observed-outputs.validation.txt"; then
            validation=FAIL
            reason="observed test output records did not match the fixture-specific expectations; see $test_dir/observed-outputs.validation.txt"
        fi
        {
            printf 'level=%s\ntest_name=%s\nexpected_test_count=1\n' "$level" "$test_name"
            printf 'exit_code=%s\nvalidation=%s\n' "$status" "$validation"
            printf 'validation_reason=%s\n' "${reason:-test name and one-test pass summary matched}"
        } > "$test_dir/validation.txt"
        if [[ "$validation" != PASS ]]; then
            test_failure_count=$((test_failure_count + 1))
            overall_state=FAIL
            overall_exit=1
        fi
    }

    run_focused_test L1 get_or_default_i32_hit_and_misses_have_one_profile_and_match_reference_bytecode
    run_focused_test L1 get_or_default_i32_typed_mir_receipt_rejects_checker_binding_forgery
    run_focused_test L1-lib core::mir::receipt::tests::get_or_default_i32_mir_receipt_rejects_forged_crosscall_bindings lib
    run_focused_test L1-lib core::mir::receipt::tests::get_or_default_i32_mir_receipt_rechecks_main_direct_call_reachability lib
    run_focused_test L2 get_or_default_i32_cli_default_and_explicit_mir_routes_match_for_all_consumers
    run_focused_test L2 get_or_default_i32_near_miss_fixtures_stay_outside_the_profile
    run_focused_test L2 get_or_default_i32_actor_method_caller_is_rejected_before_profile_admission
    run_focused_test L2 same_named_user_get_or_default_helper_does_not_receive_stdlib_profile
    run_focused_test L2 get_or_default_i32_unreachable_caller_is_rejected_before_profile_admission
    run_focused_test L2 get_or_default_i32_actor_tagged_shared_helper_is_rejected_without_legacy_fallback
    {
        printf 'execution=%s\nfocused_test_count=%s\nfocused_test_failure_count=%s\n' \
            "$([[ "$test_failure_count" == 0 ]] && printf COMPLETE || printf FAILED)" \
            "$test_count" "$test_failure_count"
        printf 'validation_contract=each test must exit zero, emit its exact test-name ok line, and report exactly one passed test\n'
    } > "$artifact_dir/execution.status"
fi

exit 0
