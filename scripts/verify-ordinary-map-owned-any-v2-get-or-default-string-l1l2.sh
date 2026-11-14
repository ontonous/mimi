#!/usr/bin/env bash
set -Eeuo pipefail
set -o noclobber

ROOT_DIR="$(cd -- "$(dirname -- "$0")/.." && pwd -P)"
PROFILE=ordinary-map-owned-any-v2-get-or-default-string-v1
TEST_TARGET=canonical_mir_ordinary_map_owned_any_v2_get_or_default_string
ARTIFACT_ENV=MIMI_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_L1L2_ARTIFACT_DIR
TARGET_ENV=MIMI_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_L1L2_TARGET_DIR
TMPDIR_ENV=MIMI_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_L1L2_TMPDIR
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"

if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper
fi

if [[ -v "$ARTIFACT_ENV" && -n "${!ARTIFACT_ENV}" ]]; then
    requested="${!ARTIFACT_ENV}"
else
    requested="${TMPDIR:-/tmp}/mimi-map-owned-any-v2-get-or-default-string-l1l2-$run_id"
fi
case "$requested" in /*) ;; *) requested="$ROOT_DIR/$requested" ;; esac
if [[ -e "$requested" || -L "$requested" ]]; then
    printf 'refusing to overwrite String GetOrDefault L1/L2 artifact path: %s\n' "$requested" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    "$ROOT_DIR/"*)
        printf 'String GetOrDefault L1/L2 artifacts must be outside the repository: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname -- "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"

failed=0
error_count=0
snapshot_before_ok=false
snapshot_after_ok=false
source_tree_identical=false
manifest_digests_verified=false
isolation_configured=false
isolation_unchanged=false
test_inventory_ok=false
fused_failure_test_inventory_ok=false
test_discovered_count=0
test_pass_count=0
test_ignored_count=0
fused_failure_test_discovered_count=0
fused_failure_test_pass_count=0
: > "$artifact_dir/validation-errors.txt"

record_error() {
    failed=1
    error_count=$((error_count + 1))
    printf 'reason=%s\n' "$1" >> "$artifact_dir/validation-errors.txt"
    printf 'validation_error=%s\n' "$1" >&2
}

build_root="${MIMI_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_L1L2_BUILD_ROOT:-${TMPDIR:-/tmp}}"
if [[ -v "$TARGET_ENV" && -n "${!TARGET_ENV}" ]]; then
    target_requested="${!TARGET_ENV}"
else
    target_requested="$build_root/mimi-map-owned-any-v2-get-or-default-string-l1l2-target-$run_id"
fi
if [[ -v "$TMPDIR_ENV" && -n "${!TMPDIR_ENV}" ]]; then
    temporary_requested="${!TMPDIR_ENV}"
else
    temporary_requested="$build_root/mimi-map-owned-any-v2-get-or-default-string-l1l2-tmp-$run_id"
fi
case "$target_requested" in /*) ;; *) target_requested="$ROOT_DIR/$target_requested" ;; esac
case "$temporary_requested" in /*) ;; *) temporary_requested="$ROOT_DIR/$temporary_requested" ;; esac
target_dir="$(realpath -m -- "$target_requested")"
temporary_dir="$(realpath -m -- "$temporary_requested")"
gate_log="$artifact_dir/gate.log"

for path_label in target_dir temporary_dir; do
    case "${!path_label}/" in
        "$ROOT_DIR/"*) record_error "$path_label must be outside the repository: ${!path_label}" ;;
        "$artifact_dir/"*) record_error "$path_label must be outside the evidence directory: ${!path_label}" ;;
    esac
    case "$artifact_dir/" in
        "${!path_label}/"*) record_error "evidence directory must be outside $path_label: $artifact_dir" ;;
    esac
done
if [[ "$target_dir" == "$temporary_dir" ]]; then
    record_error 'CARGO_TARGET_DIR and TMPDIR must be separate sibling directories'
fi
if [[ "$(dirname -- "$target_dir")" != "$(dirname -- "$temporary_dir")" ]]; then
    record_error 'CARGO_TARGET_DIR and TMPDIR must be separate siblings with the same external parent'
fi
for path in "$target_dir" "$temporary_dir"; do
    if [[ -e "$path" || -L "$path" ]]; then
        record_error "refusing to reuse an existing external build or scratch path: $path"
    fi
done
if [[ -e "$gate_log" || -L "$gate_log" ]]; then
    record_error "refusing to overwrite gate log: $gate_log"
fi
{
    printf 'CARGO_TARGET_DIR=%s\nTMPDIR=%s\ngate_log=%s\n' \
        "$target_dir" "$temporary_dir" "$gate_log"
    printf 'target_tmp_parent=%s\nexternal_paths_required=true\n' "$(dirname -- "$target_dir")"
} > "$artifact_dir/build-environment.txt"

snapshot_sources() {
    local label="$1"
    local paths="$artifact_dir/source-snapshot.$label.paths.nul"
    local text_paths="$artifact_dir/source-snapshot.$label.paths.txt"
    local manifest="$artifact_dir/source-snapshot.$label.manifest.sha256"
    local path required status=0 ignore_status
    if ! (
        cd "$ROOT_DIR"
        for directory in src tests std; do
            [[ -d "$directory" ]] || exit 1
            find "$directory" -type f -print0
        done
        printf '%s\0' Cargo.toml \
            .gitignore \
            scripts/verify-ordinary-map-owned-any-v2-get-or-default-string-l1l2.sh \
            scripts/verify-ordinary-map-owned-any-v2-get-or-default-string-l3.sh
        for optional in Cargo.lock build.rs rust-toolchain rust-toolchain.toml .cargo/config .cargo/config.toml; do
            [[ ! -f "$optional" ]] || printf '%s\0' "$optional"
        done
    ) | LC_ALL=C sort -zu > "$paths"; then
        record_error "could not enumerate source snapshot $label"
        return 1
    fi
    if ! tr '\0' '\n' < "$paths" > "$text_paths"; then
        record_error "could not render source path list $label"
        return 1
    fi
    local -a required_sources=(
        .gitignore
        Cargo.toml
        std/maps.mimi
        tests/support/memcheck_summary.rs
        tests/canonical_mir_ordinary_map_owned_any_v2_get_or_default_string.rs
        scripts/verify-ordinary-map-owned-any-v2-get-or-default-string-l1l2.sh
        scripts/verify-ordinary-map-owned-any-v2-get-or-default-string-l3.sh
    )
    local -a profile_fixtures=()
    mapfile -t profile_fixtures < <(
        find "$ROOT_DIR/tests/fixtures" -maxdepth 1 -type f \
            -name 'mir_ordinary_map_owned_any_v2_get_or_default_string_*.mimi' -print \
            | LC_ALL=C sort
    )
    if [[ "${#profile_fixtures[@]}" -ne 25 ]]; then
        printf 'expected 25 GetOrDefault<String> fixtures, found %s\n' \
            "${#profile_fixtures[@]}" >> "$artifact_dir/source-snapshot.$label.errors.txt"
        status=1
    fi
    for path in "${profile_fixtures[@]}"; do
        required_sources+=("${path#"$ROOT_DIR"/}")
    done
    for required in "${required_sources[@]}"; do
        if [[ ! -f "$ROOT_DIR/$required" ]]; then
            printf 'required source input is missing: %s\n' "$required" \
                >> "$artifact_dir/source-snapshot.$label.errors.txt"
            status=1
        fi
        if git -C "$ROOT_DIR" check-ignore -q -- "$required"; then
            printf 'required source input is ignored: %s\n' "$required" \
                >> "$artifact_dir/source-snapshot.$label.errors.txt"
            status=1
        else
            ignore_status=$?
            if [[ "$ignore_status" -ne 1 ]]; then
                printf 'could not check ignore status for required source input: %s (exit %s)\n' \
                    "$required" "$ignore_status" \
                    >> "$artifact_dir/source-snapshot.$label.errors.txt"
                status=1
            fi
        fi
        if ! grep -F -x -q -- "$required" "$text_paths"; then
            printf 'required source input is absent from the source snapshot: %s\n' "$required" \
                >> "$artifact_dir/source-snapshot.$label.errors.txt"
            status=1
        fi
    done
    : > "$manifest"
    while IFS= read -r -d '' path; do
        if [[ ! -f "$ROOT_DIR/$path" ]]; then
            printf 'source_disappeared=%s\n' "$path" >> "$artifact_dir/source-snapshot.$label.errors.txt"
            status=1
        elif ! sha256sum -- "$ROOT_DIR/$path" >> "$manifest"; then
            printf 'source_hash_failed=%s\n' "$path" >> "$artifact_dir/source-snapshot.$label.errors.txt"
            status=1
        fi
    done < "$paths"
    return "$status"
}

validate_filtered_test_inventory() {
    local directory="$1"
    local stdout="$directory/stdout"
    local ignored_test=get_or_default_string_native_default_and_explicit_mir_are_memcheck_clean
    local running_count summary_count failed_count expected_count=0 issues=0 test_name count
    local -a expected_passing_tests=(
        get_or_default_string_hit_empty_hit_and_miss_match_reference_bytecode
        get_or_default_string_typed_mir_receipt_rejects_checker_binding_forgery
        get_or_default_string_cli_default_and_explicit_mir_routes_match_for_all_consumers
        get_or_default_string_near_miss_fixtures_stay_outside_the_profile
        get_or_default_string_actor_method_caller_is_rejected_before_profile_admission
        same_named_user_get_or_default_string_helper_does_not_receive_stdlib_profile
        get_or_default_string_unreachable_caller_is_rejected_before_profile_admission
        get_or_default_string_actor_tagged_shared_helper_is_rejected_without_legacy_fallback
        get_or_default_string_cold_nominal_set_is_rejected_globally
    )

    if [[ ! -s "$stdout" ]]; then
        record_error 'Cargo test stdout is missing; filtered test inventory is unavailable'
        : >| "$directory/test-rows.txt"
    else
        grep -E '^test [[:alnum:]_]+ \.\.\. (ok|FAILED|ignored)(,.*)?$' "$stdout" \
            >| "$directory/test-rows.txt" || true
    fi
    test_discovered_count="$(wc -l < "$directory/test-rows.txt")"
    test_pass_count="$(grep -Ec '^test [[:alnum:]_]+ \.\.\. ok$' "$directory/test-rows.txt" || true)"
    test_ignored_count="$(grep -Ec '^test [[:alnum:]_]+ \.\.\. ignored(,.*)?$' "$directory/test-rows.txt" || true)"
    failed_count="$(grep -Ec '^test [[:alnum:]_]+ \.\.\. FAILED$' "$directory/test-rows.txt" || true)"
    if [[ -s "$stdout" ]]; then
        running_count="$(grep -Ec '^running 10 tests$' "$stdout" || true)"
        summary_count="$(grep -Ec '^test result: ok\. 9 passed; 0 failed; 1 ignored; 0 measured; [0-9]+ filtered out(; finished in .*)?$' "$stdout" || true)"
    else
        running_count=0
        summary_count=0
    fi
    expected_count="${#expected_passing_tests[@]}"

    {
        printf 'expected_discovered_tests=10\nobserved_discovered_tests=%s\n' "$test_discovered_count"
        printf 'expected_passing_tests=%s\nobserved_passing_tests=%s\n' "$expected_count" "$test_pass_count"
        printf 'expected_ignored_tests=1\nobserved_ignored_tests=%s\n' "$test_ignored_count"
        printf 'expected_l3_ignored_test=%s\n' "$ignored_test"
        printf 'running_10_tests_rows=%s\nexpected_result_summary_rows=1\nobserved_result_summary_rows=%s\n' \
            "$running_count" "$summary_count"
        printf 'failed_test_rows=%s\n' "$failed_count"
        printf '%s\n' 'expected_passing_test_names:'
        printf '%s\n' "${expected_passing_tests[@]}"
        printf '%s\n' 'observed_test_rows:'
        cat "$directory/test-rows.txt"
        printf '%s\n' 'observed_result_summary:'
        grep -E '^test result:' "$stdout" 2>/dev/null || true
    } >| "$directory/test-inventory.txt"

    if [[ "$test_discovered_count" -ne 10 || "$running_count" -ne 1 ]]; then
        record_error "expected exactly 10 filtered tests, observed test rows=$test_discovered_count running rows=$running_count"
        issues=1
    fi
    if [[ "$test_pass_count" -ne 9 || "$test_ignored_count" -ne 1 || "$failed_count" -ne 0 ]]; then
        record_error "expected 9 passing tests and one ignored L3 test, observed pass=$test_pass_count ignored=$test_ignored_count failed=$failed_count"
        issues=1
    fi
    if [[ "$summary_count" -ne 1 ]]; then
        record_error "expected one Rust test summary with 9 passed, 0 failed, and 1 ignored; observed $summary_count"
        issues=1
    fi
    for test_name in "${expected_passing_tests[@]}"; do
        count="$(grep -F -x -c "test $test_name ... ok" "$directory/test-rows.txt" || true)"
        if [[ "$count" -ne 1 ]]; then
            record_error "expected exactly one passing filtered test row for $test_name, observed $count"
            issues=1
        fi
    done
    count="$(grep -Ec "^test ${ignored_test} \.\.\. ignored(,.*)?$" "$directory/test-rows.txt" || true)"
    if [[ "$count" -ne 1 ]]; then
        record_error "expected exactly one ignored L3 row for $ignored_test, observed $count"
        issues=1
    fi
    if [[ "$issues" -eq 0 ]]; then test_inventory_ok=true; fi
}

validate_fused_failure_test_inventory() {
    local directory="$1"
    local stdout="$directory/stdout"
    local running_count summary_count failed_count test_name count issues=0
    local -a expected_passing_tests=(
        core::mir::reference::tests::ordinary_map_owned_any_v2_get_or_default_string_fused_failure_publication_and_unpack
        core::mir::reference::tests::ordinary_map_owned_any_v2_get_or_default_string_fused_failure_bytecode_string_copy
        codegen::mir::ordinary_map_owned_any_v2::tests::ordinary_map_owned_any_v2_get_or_default_string_fused_failure_native_abort_cleanup
    )

    if [[ ! -s "$stdout" ]]; then
        record_error 'Cargo test stdout is missing; fused failure-test inventory is unavailable'
        : >| "$directory/test-rows.txt"
    else
        grep -E '^test [[:alnum:]_:]+ \.\.\. (ok|FAILED|ignored)(,.*)?$' "$stdout" \
            >| "$directory/test-rows.txt" || true
    fi
    fused_failure_test_discovered_count="$(wc -l < "$directory/test-rows.txt")"
    fused_failure_test_pass_count="$(grep -Ec '^test [[:alnum:]_:]+ \.\.\. ok$' "$directory/test-rows.txt" || true)"
    failed_count="$(grep -Ec '^test [[:alnum:]_:]+ \.\.\. FAILED$' "$directory/test-rows.txt" || true)"
    running_count="$(grep -Ec '^running 3 tests$' "$stdout" 2>/dev/null || true)"
    summary_count="$(grep -Ec '^test result: ok\. 3 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out(; finished in .*)?$' "$stdout" 2>/dev/null || true)"

    {
        printf 'expected_discovered_tests=3\nobserved_discovered_tests=%s\n' "$fused_failure_test_discovered_count"
        printf 'expected_passing_tests=3\nobserved_passing_tests=%s\n' "$fused_failure_test_pass_count"
        printf 'expected_ignored_tests=0\nobserved_ignored_tests=0\n'
        printf 'running_3_tests_rows=%s\nexpected_result_summary_rows=1\nobserved_result_summary_rows=%s\n' \
            "$running_count" "$summary_count"
        printf 'failed_test_rows=%s\n' "$failed_count"
        printf '%s\n' 'expected_passing_test_names:'
        printf '%s\n' "${expected_passing_tests[@]}"
        printf '%s\n' 'observed_test_rows:'
        cat "$directory/test-rows.txt"
        printf '%s\n' 'observed_result_summary:'
        grep -E '^test result:' "$stdout" 2>/dev/null || true
    } >| "$directory/test-inventory.txt"

    if [[ "$fused_failure_test_discovered_count" -ne 3 || "$running_count" -ne 1 || \
        "$fused_failure_test_pass_count" -ne 3 || "$failed_count" -ne 0 || "$summary_count" -ne 1 ]]; then
        record_error "expected exactly three passing fused failure tests, observed rows=$fused_failure_test_discovered_count running=$running_count pass=$fused_failure_test_pass_count failed=$failed_count summary=$summary_count"
        issues=1
    fi
    for test_name in "${expected_passing_tests[@]}"; do
        count="$(grep -F -x -c "test $test_name ... ok" "$directory/test-rows.txt" || true)"
        if [[ "$count" -ne 1 ]]; then
            record_error "expected exactly one passing fused failure test row for $test_name, observed $count"
            issues=1
        fi
    done
    if [[ "$issues" -eq 0 ]]; then fused_failure_test_inventory_ok=true; fi
}

run_stage() {
    local name="$1" expected="$2" status=0
    shift 2
    local directory="$artifact_dir/$name"
    mkdir -p -- "$directory"
    {
        printf 'expected_exit_code=%s\n' "$expected"
        printf 'cwd=%q\n' "$ROOT_DIR"
        printf '%q ' "$@"
        printf '\n'
    } > "$directory/command.txt"
    printf '%s\0' "$@" > "$directory/command.argv.nul"
    if (cd "$ROOT_DIR" && "$@") > "$directory/stdout" 2> "$directory/stderr"; then
        status=0
    else
        status=$?
    fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$directory/process.status"
    {
        printf 'stage=%s exit_code=%s expected_exit_code=%s\n' "$name" "$status" "$expected"
        printf '%s\n' '--- stdout ---'
        cat "$directory/stdout"
        printf '%s\n' '--- stderr ---'
        cat "$directory/stderr"
    } >> "$gate_log"
    if [[ "$status" -ne "$expected" ]]; then
        record_error "$name returned $status; expected $expected"
    fi
    if [[ "$name" == string-profile-regressions ]]; then
        validate_filtered_test_inventory "$directory"
    fi
    if [[ "$name" == fused-failure-model-tests ]]; then
        validate_fused_failure_test_inventory "$directory"
    fi
    return 0
}

write_report() {
    local validation=FAIL
    if [[ "$failed" -eq 0 && "$manifest_digests_verified" == true && "$source_tree_identical" == true && "$isolation_unchanged" == true && "$test_inventory_ok" == true && "$fused_failure_test_inventory_ok" == true ]]; then
        validation=PASS
    fi
    {
        printf 'validation=%s\nprofile=%s\nlevel=L1/L2\n' "$validation" "$PROFILE"
        printf 'test_target=%s\nsource_tree_identical=%s\n' "$TEST_TARGET" "$source_tree_identical"
        printf 'source_snapshot_before_ok=%s\nsource_snapshot_after_ok=%s\n' "$snapshot_before_ok" "$snapshot_after_ok"
        printf 'CARGO_TARGET_DIR=%s\nTMPDIR=%s\ngate_log=%s\nisolation_configured=%s\nisolation_unchanged=%s\n' \
            "$target_dir" "$temporary_dir" "$gate_log" "$isolation_configured" "$isolation_unchanged"
        printf 'filtered_test_inventory_ok=%s\nfiltered_test_rows=%s\nfiltered_passing_rows=%s\nfiltered_ignored_rows=%s\n' \
            "$test_inventory_ok" "$test_discovered_count" "$test_pass_count" "$test_ignored_count"
        printf 'fused_failure_test_inventory_ok=%s\nfused_failure_test_rows=%s\nfused_failure_test_passes=%s\n' \
            "$fused_failure_test_inventory_ok" "$fused_failure_test_discovered_count" "$fused_failure_test_pass_count"
        printf 'native_fused_abort_cleanup=exercised; exact-hit route injects GetAny publication, typed-unpack, String-copy allocation, and String-copy publication failures; pre-drain Any/Map/String owner invariants and post-drain zero counts are asserted\n'
        printf 'global_map_any_v2_production_ready_expected=false\n'
        printf 'l1_l2_test_filter=get_or_default_string\ncli_coverage=default_and_explicit_mir_run_build_verify_and_near_miss_checks_in_rust_regression\n'
        printf 'failure_count=%s\n' "$error_count"
        cat "$artifact_dir/validation-errors.txt"
    } >| "$artifact_dir/artifact-validation.txt"
}

finalize() {
    local exit_status=$?
    trap - EXIT
    if [[ -n "${artifact_dir:-}" && -d "$artifact_dir" ]]; then
        if [[ "$isolation_configured" == true ]]; then
            printf 'CARGO_TARGET_DIR=%s\nTMPDIR=%s\n' \
                "${CARGO_TARGET_DIR:-<unset>}" "${TMPDIR:-<unset>}" > "$artifact_dir/build-environment-after.txt"
            if [[ "${CARGO_TARGET_DIR:-}" == "$target_dir" && "${TMPDIR:-}" == "$temporary_dir" ]]; then
                isolation_unchanged=true
            else
                record_error 'CARGO_TARGET_DIR or TMPDIR changed during the L1/L2 gate'
            fi
        fi
        if snapshot_sources after; then snapshot_after_ok=true; else snapshot_after_ok=false; fi
        if [[ "$snapshot_before_ok" == true && "$snapshot_after_ok" == true ]] && \
            cmp -s "$artifact_dir/source-snapshot.before.paths.nul" "$artifact_dir/source-snapshot.after.paths.nul" && \
            cmp -s "$artifact_dir/source-snapshot.before.manifest.sha256" "$artifact_dir/source-snapshot.after.manifest.sha256"; then
            source_tree_identical=true
        else
            record_error 'source tree paths or hashes changed during the L1/L2 capture'
        fi
        if [[ "$exit_status" -ne 0 && "$failed" -eq 0 ]]; then
            record_error "script exited before completing all L1/L2 checks (exit=$exit_status)"
        fi
        : > "$artifact_dir/manifest-validation.exit"
        if (
            cd "$artifact_dir"
            find . -type f \
                ! -name artifacts.sha256 \
                ! -name artifacts.sha256.sha256 \
                ! -name manifest-validation.log \
                ! -name manifest-validation.exit \
                ! -name artifact-validation.txt \
                -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum > artifacts.sha256 &&
            sha256sum --check artifacts.sha256 &&
            sha256sum artifacts.sha256 > artifacts.sha256.sha256 &&
            sha256sum --check artifacts.sha256.sha256
        ) > "$artifact_dir/manifest-validation.log" 2>&1; then
            manifest_digests_verified=true
            printf '0\n' >| "$artifact_dir/manifest-validation.exit"
        else
            status=$?
            printf '%s\n' "$status" >| "$artifact_dir/manifest-validation.exit"
            record_error "artifact checksum contract failed (exit=$status)"
        fi
        write_report
        sha256sum "$artifact_dir/artifact-validation.txt" > "$artifact_dir/artifact-validation.txt.sha256"
        if ! (cd "$artifact_dir" && sha256sum --check artifact-validation.txt.sha256) >> "$artifact_dir/manifest-validation.log" 2>&1; then
            record_error 'final validation report checksum failed'
            write_report
        fi
        printf 'artifact_dir=%s\nacceptance_test_exit=%s\n' "$artifact_dir" "$failed"
        cat "$artifact_dir/artifact-validation.txt"
        if [[ "$failed" -ne 0 || "$exit_status" -ne 0 ]]; then exit 1; fi
    fi
    exit "$exit_status"
}
trap finalize EXIT

cd "$ROOT_DIR"
if snapshot_sources before; then snapshot_before_ok=true; else snapshot_before_ok=false; fi
printf '%s\n' "$PROFILE" > "$artifact_dir/profile.txt"
printf 'head=%s\nbranch=%s\n' "$(git rev-parse HEAD 2>/dev/null || printf unavailable)" \
    "$(git branch --show-current 2>/dev/null || printf unavailable)" > "$artifact_dir/repository.txt"

for tool in bash cargo rustc sha256sum realpath find sort cmp git grep tr date dirname wc; do
    if ! command -v "$tool" >/dev/null 2>&1; then record_error "required tool is missing: $tool"; fi
done
if [[ ! -d "$LLVM_SYS_181_PREFIX" ]]; then record_error "LLVM wrapper directory is unavailable: $LLVM_SYS_181_PREFIX"; fi
if [[ "$failed" -ne 0 ]]; then exit 1; fi
LLVM_SYS_181_PREFIX="$(realpath -- "$LLVM_SYS_181_PREFIX")"
export LLVM_SYS_181_PREFIX
export CARGO_BUILD_JOBS=1
export CARGO_TERM_COLOR=never
if [[ -e "$gate_log" || -L "$gate_log" ]]; then
    record_error "refusing to overwrite gate log: $gate_log"
    exit 1
fi
: > "$gate_log"
mkdir -p -- "$target_dir" "$temporary_dir"
export CARGO_TARGET_DIR="$target_dir"
export TMPDIR="$temporary_dir"
isolation_configured=true
{
    printf 'CARGO_TARGET_DIR=%s\nTMPDIR=%s\ngate_log=%s\n' \
        "$CARGO_TARGET_DIR" "$TMPDIR" "$gate_log"
    printf 'target_tmp_parent=%s\nexternal_paths_required=true\n' "$(dirname -- "$CARGO_TARGET_DIR")"
} >| "$artifact_dir/build-environment.txt"

run_stage format-check 0 cargo fmt --all -- --check
run_stage string-profile-regressions 0 cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --test "$TEST_TARGET" get_or_default_string -- --test-threads=1 --show-output
run_stage fused-failure-model-tests 0 cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --lib ordinary_map_owned_any_v2_get_or_default_string_fused_failure -- --test-threads=1 --show-output

printf 'L1/L2 regression assets checked for profile=%s\n' "$PROFILE"
