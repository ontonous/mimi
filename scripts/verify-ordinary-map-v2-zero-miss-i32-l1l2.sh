#!/usr/bin/env bash
set -euo pipefail
set -o noclobber

ROOT_DIR="$(cd -- "$(dirname -- "$0")/.." && pwd -P)"
FIXTURE=tests/fixtures/mir_ordinary_map_any_owner_v2_zero_miss_i32.mimi
PROFILE=map-any-owner-v2-zero-miss-i32-v1

for required_tool in bash cargo rustc sha256sum realpath git find sort wc rg grep awk cmp xargs date printenv sed basename; do
    if ! command -v "$required_tool" >/dev/null 2>&1; then
        printf 'ZeroMissI32 L1/L2 evidence capture requires %s\n' "$required_tool" >&2
        exit 1
    fi
done

if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper
fi
if [[ ! -d "$LLVM_SYS_181_PREFIX" ]]; then
    printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi
LLVM_SYS_181_PREFIX="$(realpath -- "$LLVM_SYS_181_PREFIX")"

if [[ -x "$LLVM_SYS_181_PREFIX/bin/llvm-config" ]]; then
    LLVM_CONFIG_BIN="$LLVM_SYS_181_PREFIX/bin/llvm-config"
elif [[ -x "$LLVM_SYS_181_PREFIX/llvm-config" ]]; then
    LLVM_CONFIG_BIN="$LLVM_SYS_181_PREFIX/llvm-config"
elif command -v llvm-config >/dev/null 2>&1; then
    LLVM_CONFIG_BIN="$(command -v llvm-config)"
else
    printf 'llvm-config is unavailable for LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi

CARGO_BIN="$(command -v cargo)"
RUSTC_BIN="$(command -v rustc)"
BASH_BIN="$(command -v bash)"
if [[ -v MIMI_MAP_V2_ZERO_MISS_I32_L1L2_ARTIFACT_DIR && -n "$MIMI_MAP_V2_ZERO_MISS_I32_L1L2_ARTIFACT_DIR" ]]; then
    requested_artifact_dir="$MIMI_MAP_V2_ZERO_MISS_I32_L1L2_ARTIFACT_DIR"
else
    requested_artifact_dir="/tmp/mimi-map-v2-zero-miss-i32-l1l2-$(date -u +%Y%m%dT%H%M%SZ)-$$"
fi
case "$requested_artifact_dir" in
    /*) ;;
    *) requested_artifact_dir="$ROOT_DIR/$requested_artifact_dir" ;;
esac
if [[ -e "$requested_artifact_dir" || -L "$requested_artifact_dir" ]]; then
    printf 'refusing to overwrite existing ZeroMissI32 L1/L2 artifact path: %s\n' "$requested_artifact_dir" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested_artifact_dir")"
case "$artifact_dir/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/scripts/"*|"$ROOT_DIR/target/"*|"$ROOT_DIR/.git/"*)
        printf 'ZeroMissI32 L1/L2 artifacts must be outside source/build/git trees: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname -- "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
unset MIMI_MAP_V2_ZERO_MISS_I32_L1L2_ARTIFACT_DIR || true

cd "$ROOT_DIR"
export LLVM_SYS_181_PREFIX
export CARGO_BUILD_JOBS=1
export CARGO_TERM_COLOR=never

failed=0
error_count=0
manifest_digests_verified=false
validation_report_finalized=false
source_tree_identical=false
global_readiness_before=unknown
global_readiness_after=unknown
global_readiness_remained_false=false
fixture_sha256=invalid
route_admission_count=0
route_admission_receipt_identity_digest=''
verifier_source_hash_algorithm=blake3
verifier_source_hash_observation_count=0
verifier_source_hash=''
verifier_source_hash_matches_across_modes=true
zero_miss_library_expected_count=0
: > "$artifact_dir/validation-errors.txt"
record_error() {
    local message="$1"
    error_count=$((error_count + 1))
    failed=1
    printf 'reason=%s\n' "$message" >> "$artifact_dir/validation-errors.txt"
    printf 'validation_error=%s\n' "$message" >&2
}

write_validation_report() {
    local validation=FAIL
    if [[ "$failed" -eq 0 && "$manifest_digests_verified" == true ]]; then
        validation=PASS
    fi
    {
        printf 'validation=%s\n' "$validation"
        printf 'profile=ZeroMissI32\nlevel=L1/L2\nsource_tree_identical=%s\n' "$source_tree_identical"
        printf 'global_map_any_v2_production_ready_before=%s\n' "$global_readiness_before"
        printf 'global_map_any_v2_production_ready_after=%s\n' "$global_readiness_after"
        printf 'global_map_any_v2_production_ready_remained_false=%s\n' "$global_readiness_remained_false"
        printf 'fixture_sha256=%s\n' "$fixture_sha256"
        printf 'fixture_sha256_role=source-identification-only; not compared with verifier source_hash\n'
        printf 'verifier_source_hash_algorithm=%s\n' "$verifier_source_hash_algorithm"
        printf 'verifier_source_hash_l1_observations=%s\n' "$verifier_source_hash_observation_count"
        printf 'verifier_source_hash_l1_value=%s\n' "${verifier_source_hash:-unavailable}"
        printf 'verifier_source_hash_matches_across_modes=%s\n' "$verifier_source_hash_matches_across_modes"
        printf 'verifier_source_hash_fixture_binding=L3 replay harness independently checks BLAKE3 of captured fixture bytes\n'
        printf 'route_admission_count=%s\nroute_admission_receipt_identity_digest=%s\n' \
            "$route_admission_count" "${route_admission_receipt_identity_digest:-unavailable}"
        printf 'route_test_filter=%s\n' "${ROUTE_TEST_FILTER:-}"
        printf 'bytecode_test_filter=%s\n' "${BYTECODE_TEST_FILTER:-}"
        printf 'verifier_receipt_test_filter=%s\nverifier_single_dual_test_filter=%s\n' \
            "${VERIFIER_RECEIPT_TEST_FILTER:-}" "${VERIFIER_DUAL_TEST_FILTER:-}"
        printf 'verifier_mir_capability_admit_filter=%s\nverifier_mir_capability_drift_filter=%s\nverifier_mir_capability_extra_ops_filter=%s\n' \
            "${VERIFIER_MIR_CAPABILITY_ADMIT_FILTER:-}" \
            "${VERIFIER_MIR_CAPABILITY_DRIFT_FILTER:-}" \
            "${VERIFIER_MIR_CAPABILITY_EXTRA_OPS_FILTER:-}"
        printf 'default_dispatch_prelude_test_filter=%s\n' "${DISPATCH_TEST_FILTER:-}"
        printf 'l1_cargo_test_filter=zero_miss_i32\nl1_library_test_inventory_count=%s\n' \
            "$zero_miss_library_expected_count"
        printf 'binary_only_zero_miss_test=src/main/canonical_dispatch.rs::zero_miss_i32_uses_its_exact_profile_and_fails_closed_without_materialization_receipt; exact test executed under --bin mimi\n'
        printf 'cli_probes=receipt,default-run,explicit-mir-run,default-build,explicit-mir-build,default-verify,explicit-mir-verify,default-native-run,explicit-mir-native-run\n'
        printf 'manifest_digests_verified=%s\n' "$manifest_digests_verified"
        printf 'manifest_inventory_excludes=artifact-validation.txt,manifest-validation.log,manifest-validation.exit\n'
        printf 'failure_count=%s\n' "$error_count"
        cat "$artifact_dir/validation-errors.txt"
    } > "$artifact_dir/artifact-validation.txt"
    validation_report_finalized=true
}

finalize_failed_exit() {
    local exit_status=$?
    trap - EXIT
    if [[ "$validation_report_finalized" != true && -d "$artifact_dir" ]]; then
        failed=1
        error_count=$((error_count + 1))
        printf 'reason=script exited before all evidence and manifest checks completed (exit=%s)\n' \
            "$exit_status" >> "$artifact_dir/validation-errors.txt" || true
        manifest_digests_verified=false
        write_validation_report || true
        if [[ "$exit_status" -eq 0 ]]; then
            exit_status=1
        fi
    fi
    exit "$exit_status"
}
trap finalize_failed_exit EXIT

source_snapshot_manifest() {
    local phase="$1"
    local paths_file="$artifact_dir/source-snapshot.$phase.paths.nul"
    local manifest_file="$artifact_dir/source-snapshot.$phase.manifest.sha256"
    local status=0
    local source_path

    if ! (
        cd "$ROOT_DIR"
        {
            for source_dir in src tests std; do
                if [[ ! -d "$source_dir" ]]; then
                    printf 'required_source_directory_missing=%s\n' "$source_dir" >&2
                    exit 1
                fi
                find "$source_dir" -type f -print0
            done
            printf '%s\0' Cargo.toml \
                scripts/verify-ordinary-map-v2-zero-miss-i32-l1l2.sh \
                scripts/verify-ordinary-map-v2-zero-miss-i32-l3.sh
            for optional_source in Cargo.lock build.rs rust-toolchain rust-toolchain.toml .cargo/config .cargo/config.toml; do
                if [[ -f "$optional_source" ]]; then
                    printf '%s\0' "$optional_source"
                fi
            done
        } | LC_ALL=C sort -zu
    ) > "$paths_file"; then
        printf 'source_snapshot_path_enumeration_failed=%s\n' "$phase" >&2
        return 1
    fi
    if ! : > "$manifest_file"; then
        printf 'source_snapshot_manifest_initialization_failed=%s\n' "$phase" >&2
        return 1
    fi
    while IFS= read -r -d '' source_path; do
        if [[ ! -f "$source_path" ]]; then
            printf 'source_disappeared_before_hash=%s\n' "$source_path" >&2
            status=1
        elif ! sha256sum -- "$source_path" >> "$manifest_file"; then
            printf 'source_hash_failed=%s\n' "$source_path" >&2
            status=1
        fi
    done < "$paths_file"
    return "$status"
}

check_global_readiness() {
    local phase="$1"
    local result_variable="$2"
    local matches_file="$artifact_dir/global-readiness.$phase.matches.txt"
    local status_file="$artifact_dir/global-readiness.$phase.txt"
    local line value count

    if ! rg -n '^[[:space:]]*pub[[:space:]]+const[[:space:]]+ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY:[[:space:]]*bool[[:space:]]*=' \
        "$ROOT_DIR/src/core/mir/route.rs" > "$matches_file"; then
        : > "$matches_file"
    fi
    count="$(wc -l < "$matches_file")"
    if [[ "$count" -ne 1 ]]; then
        value=unknown
        record_error "$phase source must declare global Map/Any readiness exactly once"
    else
        line="$(cat "$matches_file")"
        value="$(printf '%s\n' "$line" | sed -E 's/.*=[[:space:]]*([^;[:space:]]+);[[:space:]]*$/\1/')"
        if [[ "$value" != false ]]; then
            record_error "$phase source changed ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY from false"
        fi
    fi
    printf -v "$result_variable" '%s' "$value"
    printf 'source=%s\nvalue=%s\nexpected=false\n' \
        "$ROOT_DIR/src/core/mir/route.rs" "$value" > "$status_file"
}

capture_tool() {
    local label="$1"
    local executable="$2"
    shift 2
    local directory="$artifact_dir/toolchain/$label"
    local status=0
    mkdir -- "$directory"
    {
        printf '%q' "$executable"
        printf ' %q' "$@"
        printf '\n'
    } > "$directory/command.txt"
    printf '%s\0' "$executable" "$@" > "$directory/command.argv.nul"
    if "$executable" "$@" > "$directory/stdout" 2> "$directory/stderr"; then
        status=0
    else
        status=$?
        failed=1
        error_count=$((error_count + 1))
        printf 'reason=toolchain metadata command failed: %s\n' "$label" >> "$artifact_dir/validation-errors.txt"
    fi
    printf '%s\n' "$status" > "$directory/exit"
}

run_stage() {
    local name="$1"
    local expected_status="$2"
    shift 2
    local stage_dir="$artifact_dir/$name"
    local status=0

    mkdir -- "$stage_dir"
    {
        printf 'expected_exit_code=%s\n' "$expected_status"
        printf 'cwd=%q\n' "$ROOT_DIR"
        printf '%q ' "$@"
        printf '\n'
    } > "$stage_dir/command.txt"
    printf '%s\0' "$@" > "$stage_dir/command.argv.nul"
    printf 'LLVM_SYS_181_PREFIX=%q\nCARGO_BUILD_JOBS=1\nCARGO_TERM_COLOR=never\n' \
        "$LLVM_SYS_181_PREFIX" > "$stage_dir/command.fixed-environment.txt"
    if "$@" > "$stage_dir/stdout" 2> "$stage_dir/stderr"; then
        status=0
    else
        status=$?
    fi
    printf '%s\n' "$status" > "$stage_dir/exit"
    printf 'exit_code=%s\nexpected_exit_code=%s\nmatched_expected_exit_code=%s\n' \
        "$status" "$expected_status" \
        "$([[ "$status" -eq "$expected_status" ]] && printf true || printf false)" \
        > "$stage_dir/process.status"
    printf '%s exit=%s expected=%s\n' "$name" "$status" "$expected_status"
    if [[ "$status" -ne "$expected_status" ]]; then
        record_error "$name returned exit $status; expected $expected_status"
    fi
}

expect_zero_miss_library_tests_once() {
    local stage="$1"
    local combined="$artifact_dir/$stage/combined.log"
    local inventory="$artifact_dir/test-discovery/library.zero-miss-tests.txt"
    local result_file="$artifact_dir/$stage/test-results.txt"
    local expected_total observed_total summary_count test_name expected_count observed_count passed_count pattern required_filter
    local -a required_filters=("${@:2}")

    cat "$artifact_dir/$stage/stdout" "$artifact_dir/$stage/stderr" > "$combined"
    : > "$result_file"
    if [[ ! -s "$inventory" ]]; then
        record_error "the discovered ZeroMissI32 library-test inventory is empty or unavailable"
        return
    fi
    expected_total="$(wc -l < "$inventory")"
    if [[ "$expected_total" -lt 1 ]]; then
        record_error "the discovered ZeroMissI32 library-test inventory has no tests"
        return
    fi

    while IFS= read -r test_name; do
        expected_count="$(awk -F: -v name="$test_name" '$NF == name { count++ } END { print count + 0 }' "$inventory")"
        pattern="^test (.*::)?${test_name} \.\.\. (ok|FAILED|ignored)$"
        observed_count="$(grep -Ec "$pattern" "$combined" || true)"
        pattern="^test (.*::)?${test_name} \.\.\. ok$"
        passed_count="$(grep -Ec "$pattern" "$combined" || true)"
        printf 'test=%s\nexpected_occurrences=%s\nobserved_occurrences=%s\npassed_occurrences=%s\n' \
            "$test_name" "$expected_count" "$observed_count" "$passed_count" >> "$result_file"
        if [[ "$observed_count" -ne "$expected_count" || "$passed_count" -ne "$expected_count" ]]; then
            record_error "$stage did not run every discovered $test_name exactly once with PASS status"
        fi
    done < <(awk -F: '{ print $NF }' "$inventory" | LC_ALL=C sort -u)

    observed_total="$(grep -Ec '^test .*zero_miss_i32.* \.\.\. (ok|FAILED|ignored)$' "$combined" || true)"
    if [[ "$observed_total" -ne "$expected_total" ]]; then
        record_error "$stage ran $observed_total zero_miss_i32 tests but the source inventory contains $expected_total"
    fi
    summary_count="$(grep -Ec "^test result: ok\\. ${expected_total} passed; 0 failed; 0 ignored;" "$combined" || true)"
    if [[ "$summary_count" -ne 1 ]]; then
        record_error "$stage did not report exactly $expected_total passing, non-ignored ZeroMissI32 library tests"
    fi

    for required_filter in "${required_filters[@]}"; do
        if [[ -z "$required_filter" ]]; then
            record_error "$stage has an undiscovered required exact ZeroMissI32 test filter"
            continue
        fi
        expected_count="$(awk -F: -v name="$required_filter" '$NF == name { count++ } END { print count + 0 }' "$inventory")"
        pattern="^test (.*::)?${required_filter} \.\.\. (ok|FAILED|ignored)$"
        observed_count="$(grep -Ec "$pattern" "$combined" || true)"
        pattern="^test (.*::)?${required_filter} \.\.\. ok$"
        passed_count="$(grep -Ec "$pattern" "$combined" || true)"
        printf 'required_test=%s\ninventory_occurrences=%s\nobserved_occurrences=%s\npassed_occurrences=%s\n' \
            "$required_filter" "$expected_count" "$observed_count" "$passed_count" >> "$result_file"
        if [[ "$expected_count" -ne 1 || "$observed_count" -ne 1 || "$passed_count" -ne 1 ]]; then
            record_error "$stage required exact test $required_filter was missing, duplicated, or not PASS"
        fi
    done

    {
        printf 'inventory_test_count=%s\nobserved_zero_miss_test_count=%s\n' "$expected_total" "$observed_total"
        printf 'cargo_summary_pass_count=%s\n' "$summary_count"
    } >> "$result_file"
}

expect_single_named_test_passed() {
    local stage="$1"
    local test_name="$2"
    local combined="$artifact_dir/$stage/combined.log"
    local test_results="$artifact_dir/$stage/test-results.txt"
    local expected_line="test $test_name ... ok"
    local observed_count summary_count

    cat "$artifact_dir/$stage/stdout" "$artifact_dir/$stage/stderr" > "$combined"
    observed_count="$(grep -Fxc "$expected_line" "$combined" || true)"
    summary_count="$(grep -Ec '^test result: ok\. 1 passed; 0 failed; 0 ignored;' "$combined" || true)"
    {
        printf 'expected_test_line=%s\nobserved_test_line_count=%s\n' \
            "$expected_line" "$observed_count"
        printf 'expected_summary_count=1\nobserved_summary_count=%s\n' "$summary_count"
    } > "$test_results"
    if [[ "$observed_count" -ne 1 || "$summary_count" -ne 1 ]]; then
        record_error "$stage did not run exactly one passing $test_name binary test"
    fi
}

expect_fixed_text() {
    local stage="$1"
    local file="$2"
    local exact_line="$3"
    local count
    count="$(grep -Fxc -- "$exact_line" "$artifact_dir/$stage/$file" || true)"
    if [[ "$count" -ne 1 ]]; then
        record_error "$stage expected exactly one line: $exact_line"
    fi
}

expect_stdout_true() {
    local stage="$1"
    if ! printf 'true\n' | cmp -s - "$artifact_dir/$stage/stdout"; then
        record_error "$stage stdout was not exactly true followed by newline"
    fi
}

expect_no_runtime_failure_diagnostic() {
    local stage="$1"
    local stderr_file="$artifact_dir/$stage/stderr"
    if grep -Eiq 'E0800|runtime[[:space:]_-]*(error|failure)|(^|[^[:alnum:]_])(trap|panic)([^[:alnum:]_]|$)' \
        "$stderr_file"; then
        record_error "$stage emitted a runtime error, trap, panic, or E0800 diagnostic"
    fi
}

expect_route_admission() {
    local stage="$1"
    local consumer="$2"
    local mode="$3"
    local digest="$4"
    local stderr_file="$artifact_dir/$stage/stderr"
    local rows row actual_digest
    rows="$(grep -Ec '^MIMI_ROUTE_CENSUS route-admission ' "$stderr_file" || true)"
    if [[ "$rows" -ne 1 ]]; then
        record_error "$stage did not emit one exact Checker route-admission row"
    else
        row="$(grep -E '^MIMI_ROUTE_CENSUS route-admission ' "$stderr_file")"
        if ! grep -Eq "^MIMI_ROUTE_CENSUS route-admission consumer=$consumer mode=$mode checker_profile=$PROFILE mir_digest=$digest receipt_identity_digest=[0-9a-f]{64}$" \
            <<< "$row"; then
            record_error "$stage did not emit one exact Checker route-admission row"
        else
            actual_digest="${row##* receipt_identity_digest=}"
            route_admission_count=$((route_admission_count + 1))
            if [[ -z "$route_admission_receipt_identity_digest" ]]; then
                route_admission_receipt_identity_digest="$actual_digest"
            elif [[ "$actual_digest" != "$route_admission_receipt_identity_digest" ]]; then
                record_error "$stage receipt_identity_digest differed from an earlier CLI route admission"
            fi
        fi
    fi
    if grep -Fq 'canonical route disposition: legacy' "$stderr_file"; then
        record_error "$stage selected a legacy route"
    fi
}

expect_route_disposition() {
    local stage="$1"
    local kind="$2"
    local digest="$3"
    local exact_line="canonical route disposition: $kind ($PROFILE) mir_digest=$digest"
    expect_fixed_text "$stage" stderr "$exact_line"
    if grep -Fq 'canonical route disposition: legacy' "$artifact_dir/$stage/stderr"; then
        record_error "$stage reported a legacy route"
    fi
}

expect_native_build_receipt() {
    local stage="$1"
    local consumer="$2"
    local digest="$3"
    local stderr_file="$artifact_dir/$stage/stderr"
    local rows row field expected actual route_identity_digest
    local -a fields=(type_desc_digest abi_digest ffi_digest ownership_digest flow_transition_digest root_owners)
    rows="$(grep -Ec '^MIMI_ROUTE_CENSUS canonical-native-build ' "$stderr_file" || true)"
    if [[ "$rows" -ne 1 ]]; then
        record_error "$stage did not emit one ZeroMissI32 native-build receipt"
    else
        row="$(grep -E '^MIMI_ROUTE_CENSUS canonical-native-build ' "$stderr_file")"
        if ! grep -Eq "^MIMI_ROUTE_CENSUS canonical-native-build consumer=$consumer route_id=$PROFILE schema=mimi-mir-route-receipt-v1 profile=$PROFILE mir_digest=$digest " \
            <<< "$row"; then
            record_error "$stage did not emit one ZeroMissI32 native-build receipt"
        else
            for field in "${fields[@]}"; do
                if ! expected="$(awk -F= -v key="$field" '$1 == key { count++; value = substr($0, index($0, "=") + 1) } END { if (count != 1) exit 1; print value }' "$artifact_dir/cli-receipt/stdout")"; then
                    record_error "CLI MIR receipt did not provide exactly one $field value for native receipt comparison"
                    continue
                fi
                if ! actual="$(awk -v key="$field" '{ for (i = 1; i <= NF; i++) if (index($i, key "=") == 1) { count++; value = substr($i, length(key) + 2) } } END { if (count != 1) exit 1; print value }' <<< "$row")"; then
                    record_error "$stage native-build receipt did not provide exactly one $field value"
                    continue
                fi
                if [[ "$actual" != "$expected" ]]; then
                    record_error "$stage native-build receipt $field differs from the CLI MIR receipt"
                fi
            done
            if ! route_identity_digest="$(awk -v key=route_identity_digest '{ for (i = 1; i <= NF; i++) if (index($i, key "=") == 1) { count++; value = substr($i, length(key) + 2) } } END { if (count != 1) exit 1; print value }' <<< "$row")"; then
                record_error "$stage native-build receipt omitted one route_identity_digest"
            elif [[ ! "$route_identity_digest" =~ ^[0-9a-f]{64}$ || "$route_identity_digest" != "$route_admission_receipt_identity_digest" ]]; then
                record_error "$stage native-build route identity differs from route admission"
            fi
        fi
    fi
    if ! grep -Eq "^info: canonical MIR native compiled [1-9][0-9]*/[1-9][0-9]* function\(s\), 0 fell back; mir_digest=$digest$" "$stderr_file"; then
        record_error "$stage did not compile the full MIR function set without fallback"
    fi
}

expect_verifier_provenance() {
    local stage="$1"
    local expected_mir_digest="$2"
    local stderr_file="$artifact_dir/$stage/stderr"
    local rows line field value expected index
    local -a tokens=()
    local -a fields=(profile mir_digest type_desc_digest abi_digest ffi_digest ownership_digest flow_transition_digest source_hash route_identity_digest)

    rows="$(grep -Ec '^canonical MIR verifier provenance: ' "$stderr_file" || true)"
    if [[ "$rows" -ne 1 ]]; then
        record_error "$stage did not emit exactly one canonical MIR verifier provenance row"
        return
    fi
    line="$(grep -F 'canonical MIR verifier provenance: ' "$stderr_file")"
    line="${line#canonical MIR verifier provenance: }"
    read -r -a tokens <<< "$line"
    if [[ "${#tokens[@]}" -ne "${#fields[@]}" ]]; then
        record_error "$stage verifier provenance row has the wrong field count"
        return
    fi
    for index in "${!fields[@]}"; do
        field="${tokens[$index]%%=*}"
        value="${tokens[$index]#*=}"
        if [[ "$field" != "${fields[$index]}" || "$value" == "${tokens[$index]}" ]]; then
            record_error "$stage verifier provenance field order or syntax changed at position $index"
            continue
        fi
        case "$field" in
            profile) expected="$PROFILE" ;;
            mir_digest) expected="$expected_mir_digest" ;;
            source_hash)
                if [[ ! "$value" =~ ^[0-9a-f]{64}$ ]]; then
                    record_error "$stage verifier provenance source_hash is not a lowercase 64-character BLAKE3 digest"
                else
                    verifier_source_hash_observation_count=$((verifier_source_hash_observation_count + 1))
                    if [[ -z "$verifier_source_hash" ]]; then
                        verifier_source_hash="$value"
                    elif [[ "$value" != "$verifier_source_hash" ]]; then
                        verifier_source_hash_matches_across_modes=false
                        record_error "$stage verifier provenance source_hash differs between default and explicit MIR verify"
                    fi
                fi
                continue
                ;;
            route_identity_digest)
                if [[ ! "$value" =~ ^[0-9a-f]{64}$ ]]; then
                    record_error "$stage verifier provenance route_identity_digest is not a lowercase 64-character BLAKE3 digest"
                fi
                expected="$route_admission_receipt_identity_digest"
                ;;
            *)
                expected="$(awk -F= -v key="$field" '$1 == key { print $2 }' "$artifact_dir/cli-receipt/stdout")"
                ;;
        esac
        if [[ "$value" != "$expected" ]]; then
            record_error "$stage verifier provenance changed $field from the captured MIR receipt"
        fi
    done
}

discover_test_filter() {
    local source_file="$1"
    local candidate_name="$2"
    local label="$3"
    local result_variable="$4"
    local matches_file="$artifact_dir/test-discovery/$label.matches.txt"
    local matching pattern
    local -a names=()

    pattern="^[[:space:]]*fn[[:space:]]+$candidate_name[[:space:]]*\\("
    if ! rg -n --with-filename --no-heading "$pattern" "$source_file" > "$matches_file"; then
        record_error "test filter candidate was not found in $source_file"
        return 1
    fi
    matching="$(sed -E 's/^[^:]*:[0-9]+:[[:space:]]*fn[[:space:]]+([A-Za-z0-9_]+).*/\1/' "$matches_file")"
    mapfile -t names < <(printf '%s\n' "$matching" | LC_ALL=C sort -u)
    if [[ "${#names[@]}" -ne 1 ]]; then
        record_error "expected one $label test filter candidate in $source_file"
        return 1
    fi
    printf -v "$result_variable" '%s' "${names[0]}"
    printf 'source=%s\nfilter=%s\n' "$source_file" "${names[0]}" \
        >> "$artifact_dir/test-discovery/$label.selected.txt"
}

discover_zero_miss_test_inventory() {
    local source_file="$1"
    local label="$2"
    local inventory_file="$artifact_dir/test-discovery/$label.zero-miss-tests.txt"

    awk '
        /^[[:space:]]*#\[test\][[:space:]]*$/ { test_attribute = 1; next }
        /^[[:space:]]*#\[/ { next }
        /^[[:space:]]*$/ || /^[[:space:]]*\/\// { next }
        test_attribute && /^[[:space:]]*fn[[:space:]]+[A-Za-z0-9_]+/ {
            name = $0
            sub(/^[[:space:]]*fn[[:space:]]+/, "", name)
            sub(/\(.*/, "", name)
            if (name ~ /zero_miss_i32/) {
                printf "%d:%s\n", FNR, name
            }
            test_attribute = 0
            next
        }
        { test_attribute = 0 }
    ' "$source_file" > "$inventory_file"
}

build_zero_miss_library_inventory() {
    local inventory_file="$artifact_dir/test-discovery/library.zero-miss-tests.txt"
    local source_inventory source_path line test_name index
    local -a sources=(
        src/core/cfg/resource_lower.rs
        src/core/mir/route.rs
        src/core/mir/receipt.rs
        src/interp/bytecode/mir.rs
        src/verifier/mod.rs
        src/verifier/mir_capability.rs
        src/codegen/mir/eligibility.rs
    )
    local -a labels=(resource-lower route receipt bytecode verifier verifier-mir-capability eligibility)

    : > "$inventory_file"
    for index in "${!sources[@]}"; do
        source_path="${sources[$index]}"
        source_inventory="$artifact_dir/test-discovery/${labels[$index]}.zero-miss-tests.txt"
        if [[ ! -f "$source_inventory" ]]; then
            record_error "missing ZeroMissI32 test discovery inventory for $source_path"
            continue
        fi
        while IFS=: read -r line test_name; do
            [[ -n "$test_name" ]] || continue
            printf '%s:%s:%s\n' "$source_path" "$line" "$test_name" >> "$inventory_file"
        done < "$source_inventory"
    done
    zero_miss_library_expected_count="$(wc -l < "$inventory_file")"
    if [[ "$zero_miss_library_expected_count" -eq 0 ]]; then
        record_error "no ZeroMissI32 library test functions were discovered"
    fi
    printf 'l1_cargo_test_filter=zero_miss_i32\nl1_library_test_inventory_count=%s\n' \
        "$zero_miss_library_expected_count" >> "$artifact_dir/test-discovery/summary.txt"
}

mkdir -- "$artifact_dir/test-discovery" "$artifact_dir/toolchain"
: > "$artifact_dir/test-discovery/summary.txt"
source_snapshot_manifest before || record_error "could not create the before source snapshot"
global_readiness_before=unknown
check_global_readiness before global_readiness_before
sha256sum -- "$artifact_dir/source-snapshot.before.manifest.sha256" \
    | awk '{print $1}' > "$artifact_dir/source-snapshot.before.sha256"
if ! fixture_sha256="$(sha256sum -- "$ROOT_DIR/$FIXTURE" | awk '{print $1}')" || \
    [[ ! "$fixture_sha256" =~ ^[0-9a-f]{64}$ ]]; then
    fixture_sha256=invalid
    record_error "could not capture the fixture's source-identification SHA-256"
fi
git rev-parse HEAD > "$artifact_dir/head.before.txt"
git branch --show-current > "$artifact_dir/branch.before.txt"
git status --short --untracked-files=no > "$artifact_dir/git-status.before.txt"

capture_tool bash "$BASH_BIN" --version
capture_tool rustc "$RUSTC_BIN" --version --verbose
capture_tool cargo "$CARGO_BIN" --version --verbose
capture_tool llvm-config "$LLVM_CONFIG_BIN" --version --prefix --libdir --host-target
sha256sum -- "$BASH_BIN" "$RUSTC_BIN" "$CARGO_BIN" "$LLVM_CONFIG_BIN" \
    > "$artifact_dir/toolchain/binaries.sha256"
{
    printf 'root=%q\nfixture=%q\nprofile=%s\n' "$ROOT_DIR" "$FIXTURE" "$PROFILE"
    printf 'LLVM_SYS_181_PREFIX=%q\nCARGO_BUILD_JOBS=1\nCARGO_TERM_COLOR=never\n' "$LLVM_SYS_181_PREFIX"
    printf 'CLI_MIMI_STDLIB=%q\nCLI_MIMI_ROUTE_CENSUS=1\nCLI_MIMI_VERBOSE=1\n' "$ROOT_DIR/std"
    printf 'global_map_any_v2_production_ready=%s\n' "$global_readiness_before"
    printf 'fixture_sha256=%s\n' "$fixture_sha256"
    printf 'fixture_sha256_role=source-identification-only; not compared with verifier source_hash\n'
    printf 'verifier_source_hash_algorithm=%s\n' "$verifier_source_hash_algorithm"
    printf 'verifier_source_hash_fixture_binding=L3 replay harness independently checks BLAKE3 of captured fixture bytes\n'
    for variable in PATH HOME TMPDIR LANG LC_ALL RUSTC RUSTUP_HOME RUSTUP_TOOLCHAIN RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_HOME CARGO_TARGET_DIR CARGO_BUILD_TARGET CARGO_PROFILE_DEV_DEBUG CARGO_INCREMENTAL RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER LIBCLANG_PATH CC CXX CFLAGS LDFLAGS LD_PRELOAD LD_LIBRARY_PATH PKG_CONFIG_PATH MIMI_STDLIB MIMI_ROUTE_CENSUS MIMI_VERBOSE MIMI_TEST_FORCE_NO_Z3; do
        if [[ -v "$variable" ]]; then
            printf 'inherited_%s=%q\n' "$variable" "$(printenv "$variable")"
        else
            printf 'inherited_%s=<unset>\n' "$variable"
        fi
    done
    printf 'cargo_path=%q\nrustc_path=%q\nbash_path=%q\nllvm_config_path=%q\n' \
        "$CARGO_BIN" "$RUSTC_BIN" "$BASH_BIN" "$LLVM_CONFIG_BIN"
} > "$artifact_dir/toolchain/environment.txt"

capture_tool cargo-metadata "$CARGO_BIN" metadata --no-deps --format-version 1 --manifest-path "$ROOT_DIR/Cargo.toml"
target_directory="$(sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' "$artifact_dir/toolchain/cargo-metadata/stdout")"
if [[ -z "$target_directory" || "$target_directory" == *$'\n'* ]]; then
    record_error "Cargo metadata did not provide one usable target directory"
    target_directory="$ROOT_DIR/target"
fi
if [[ -v CARGO_BUILD_TARGET && -n "$CARGO_BUILD_TARGET" ]]; then
    mimi_cli="$(realpath -m -- "$target_directory/$CARGO_BUILD_TARGET/debug/mimi")"
else
    mimi_cli="$(realpath -m -- "$target_directory/debug/mimi")"
fi
printf 'cargo_target_directory=%q\nmimi_cli_binary=%q\n' "$target_directory" "$mimi_cli" \
    >> "$artifact_dir/toolchain/environment.txt"

resource_lower_source=src/core/cfg/resource_lower.rs
route_source=src/core/mir/route.rs
receipt_source=src/core/mir/receipt.rs
bytecode_source=src/interp/bytecode/mir.rs
verifier_source=src/verifier/mod.rs
verifier_mir_capability_source=src/verifier/mir_capability.rs
eligibility_source=src/codegen/mir/eligibility.rs
dispatch_source=src/main/canonical_dispatch.rs
ROUTE_TEST_FILTER=''
BYTECODE_TEST_FILTER=''
VERIFIER_RECEIPT_TEST_FILTER=''
VERIFIER_DUAL_TEST_FILTER=''
VERIFIER_MIR_CAPABILITY_ADMIT_FILTER=''
VERIFIER_MIR_CAPABILITY_DRIFT_FILTER=''
VERIFIER_MIR_CAPABILITY_EXTRA_OPS_FILTER=''
DISPATCH_TEST_FILTER=''
DISPATCH_RECEIPT_TEST_FILTER=''
discover_test_filter "$route_source" \
    ordinary_map_owned_any_v2_zero_miss_i32_has_an_independent_exact_route \
    route-exact-profile ROUTE_TEST_FILTER || true
discover_test_filter "$bytecode_source" \
    ordinary_map_owned_any_v2_zero_miss_i32_requires_its_exact_anchor_before_output \
    bytecode-anchor BYTECODE_TEST_FILTER || true
discover_test_filter "$verifier_source" \
    zero_miss_i32_requires_the_exact_checker_route_receipt \
    verifier-receipt VERIFIER_RECEIPT_TEST_FILTER || true
discover_test_filter "$verifier_source" \
    checked_single_and_dual_verifiers_keep_the_narrow_zero_miss_i32_route \
    verifier-single-dual VERIFIER_DUAL_TEST_FILTER || true
discover_test_filter "$verifier_mir_capability_source" \
    zero_miss_i32_census_admits_only_the_exact_two_owner_flow \
    verifier-mir-capability-admit VERIFIER_MIR_CAPABILITY_ADMIT_FILTER || true
discover_test_filter "$verifier_mir_capability_source" \
    zero_miss_i32_census_rejects_operation_count_and_result_type_drift \
    verifier-mir-capability-drift VERIFIER_MIR_CAPABILITY_DRIFT_FILTER || true
discover_test_filter "$verifier_mir_capability_source" \
    zero_miss_i32_census_rejects_extra_map_operations_and_duplicate_pair_drops \
    verifier-mir-capability-extra-ops VERIFIER_MIR_CAPABILITY_EXTRA_OPS_FILTER || true
discover_test_filter "$dispatch_source" \
    zero_miss_i32_default_route_accepts_only_the_standard_library_prelude \
    default-dispatch-prelude DISPATCH_TEST_FILTER || true
discover_test_filter "$dispatch_source" \
    zero_miss_i32_uses_its_exact_profile_and_fails_closed_without_materialization_receipt \
    missing-receipt-dispatch DISPATCH_RECEIPT_TEST_FILTER || true
if [[ -n "$DISPATCH_TEST_FILTER" ]]; then
    DISPATCH_TEST_FILTER="canonical_dispatch::tests::$DISPATCH_TEST_FILTER"
fi
if [[ -n "$DISPATCH_RECEIPT_TEST_FILTER" ]]; then
    DISPATCH_RECEIPT_TEST_FILTER="canonical_dispatch::tests::$DISPATCH_RECEIPT_TEST_FILTER"
fi
discover_zero_miss_test_inventory "$resource_lower_source" resource-lower
discover_zero_miss_test_inventory "$route_source" route
discover_zero_miss_test_inventory "$receipt_source" receipt
discover_zero_miss_test_inventory "$bytecode_source" bytecode
discover_zero_miss_test_inventory "$verifier_source" verifier
discover_zero_miss_test_inventory "$verifier_mir_capability_source" verifier-mir-capability
discover_zero_miss_test_inventory "$eligibility_source" eligibility
build_zero_miss_library_inventory
if [[ ! -s "$artifact_dir/test-discovery/receipt.zero-miss-tests.txt" ]]; then
    printf 'receipt_tests=none; exact route/verifier tests and real CLI probes cover the Receipt surface\n' \
        >> "$artifact_dir/test-discovery/summary.txt"
fi
if [[ ! -s "$artifact_dir/test-discovery/eligibility.zero-miss-tests.txt" ]]; then
    printf 'eligibility_tests=none; real default and explicit build probes cover native eligibility\n' \
        >> "$artifact_dir/test-discovery/summary.txt"
fi
printf 'verifier_tests_source=%s\n' "$verifier_source" >> "$artifact_dir/test-discovery/summary.txt"
printf 'receipt_tests_source=%s\neligibility_tests_source=%s\n' "$receipt_source" "$eligibility_source" \
    >> "$artifact_dir/test-discovery/summary.txt"
printf 'verifier_mir_capability_tests_source=%s\n' "$verifier_mir_capability_source" \
    >> "$artifact_dir/test-discovery/summary.txt"
printf 'route_test_filter=%s\nbytecode_test_filter=%s\nverifier_receipt_test_filter=%s\nverifier_single_dual_test_filter=%s\n' \
    "$ROUTE_TEST_FILTER" "$BYTECODE_TEST_FILTER" \
    "$VERIFIER_RECEIPT_TEST_FILTER" "$VERIFIER_DUAL_TEST_FILTER" \
    >> "$artifact_dir/test-discovery/summary.txt"
printf 'verifier_mir_capability_admit_filter=%s\nverifier_mir_capability_drift_filter=%s\nverifier_mir_capability_extra_ops_filter=%s\n' \
    "$VERIFIER_MIR_CAPABILITY_ADMIT_FILTER" \
    "$VERIFIER_MIR_CAPABILITY_DRIFT_FILTER" \
    "$VERIFIER_MIR_CAPABILITY_EXTRA_OPS_FILTER" \
    >> "$artifact_dir/test-discovery/summary.txt"
printf 'default_dispatch_prelude_test_filter=%s\n' "$DISPATCH_TEST_FILTER" \
    >> "$artifact_dir/test-discovery/summary.txt"
printf 'missing_receipt_dispatch_test_filter=%s\n' "$DISPATCH_RECEIPT_TEST_FILTER" \
    >> "$artifact_dir/test-discovery/summary.txt"
printf 'binary_only_zero_miss_test=src/main/canonical_dispatch.rs::zero_miss_i32_uses_its_exact_profile_and_fails_closed_without_materialization_receipt; exact test executed under --bin mimi\n' \
    >> "$artifact_dir/test-discovery/summary.txt"

run_stage format 0 "$CARGO_BIN" fmt --all -- --check
run_stage l1-zero-miss-i32-library 0 "$CARGO_BIN" test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --lib zero_miss_i32 -- --test-threads=1
expect_zero_miss_library_tests_once l1-zero-miss-i32-library \
    "$ROUTE_TEST_FILTER" "$BYTECODE_TEST_FILTER" \
    "$VERIFIER_RECEIPT_TEST_FILTER" "$VERIFIER_DUAL_TEST_FILTER" \
    "$VERIFIER_MIR_CAPABILITY_ADMIT_FILTER" "$VERIFIER_MIR_CAPABILITY_DRIFT_FILTER" \
    "$VERIFIER_MIR_CAPABILITY_EXTRA_OPS_FILTER"
if [[ "$failed" -ne 0 ]]; then
    printf 'dependent_cli_probes=skipped_after_library_gate_failure\n' \
        >> "$artifact_dir/test-discovery/summary.txt"
    exit 1
fi

run_stage l1-zero-miss-i32-default-dispatch 0 "$CARGO_BIN" test \
    --manifest-path "$ROOT_DIR/Cargo.toml" --bin mimi "$DISPATCH_TEST_FILTER" \
    -- --exact --test-threads=1
expect_single_named_test_passed l1-zero-miss-i32-default-dispatch "$DISPATCH_TEST_FILTER"
if [[ "$failed" -ne 0 ]]; then
    printf 'dependent_cli_probes=skipped_after_default_dispatch_gate_failure\n' \
        >> "$artifact_dir/test-discovery/summary.txt"
    exit 1
fi
run_stage l1-zero-miss-i32-missing-receipt-dispatch 0 "$CARGO_BIN" test \
    --manifest-path "$ROOT_DIR/Cargo.toml" --bin mimi "$DISPATCH_RECEIPT_TEST_FILTER" \
    -- --exact --test-threads=1
expect_single_named_test_passed l1-zero-miss-i32-missing-receipt-dispatch "$DISPATCH_RECEIPT_TEST_FILTER"
if [[ "$failed" -ne 0 ]]; then
    printf 'dependent_cli_probes=skipped_after_missing_receipt_dispatch_gate_failure\n' \
        >> "$artifact_dir/test-discovery/summary.txt"
    exit 1
fi

run_stage cli-build 0 "$CARGO_BIN" build --manifest-path "$ROOT_DIR/Cargo.toml" --bin mimi
if [[ ! -x "$mimi_cli" ]]; then
    record_error "Cargo build did not produce the expected CLI executable at $mimi_cli"
    exit 1
fi

run_stage cli-receipt 0 env "MIMI_STDLIB=$ROOT_DIR/std" MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
    "$mimi_cli" mir --all --receipt "$ROOT_DIR/$FIXTURE"
receipt_stdout="$artifact_dir/cli-receipt/stdout"
expect_fixed_text cli-receipt stdout mimi-mir-route-manifest-v1
for receipt_field in schema profile mir_digest type_desc_digest abi_digest ffi_digest ownership_digest flow_transition_digest root_owners; do
    count="$(grep -Ec "^$receipt_field=" "$receipt_stdout" || true)"
    if [[ "$count" -ne 1 ]]; then
        record_error "CLI receipt must contain exactly one $receipt_field field"
    fi
done
expect_fixed_text cli-receipt stdout schema=mimi-mir-route-receipt-v1
expect_fixed_text cli-receipt stdout profile=cli-mir-v1
if grep -Eq '^(global_map_any_v2_production_ready|production_ready|execution_route)=' "$receipt_stdout"; then
    record_error "read-only MIR receipt unexpectedly claimed production readiness or an execution route"
fi
mir_digest="$(awk -F= '$1 == "mir_digest" { print $2 }' "$receipt_stdout")"
if [[ ! "$mir_digest" =~ ^[0-9a-f]{64}$ ]]; then
    record_error "CLI MIR receipt did not contain one lowercase SHA-256-shaped MIR digest"
    mir_digest=invalid
fi

run_stage cli-default-run 1 env "MIMI_STDLIB=$ROOT_DIR/std" MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
    "$mimi_cli" run "$ROOT_DIR/$FIXTURE"
expect_stdout_true cli-default-run
expect_no_runtime_failure_diagnostic cli-default-run
expect_route_admission cli-default-run run default "$mir_digest"
expect_route_disposition cli-default-run canonical "$mir_digest"

run_stage cli-explicit-mir-run 1 env "MIMI_STDLIB=$ROOT_DIR/std" MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
    "$mimi_cli" run --mir "$ROOT_DIR/$FIXTURE"
expect_stdout_true cli-explicit-mir-run
expect_no_runtime_failure_diagnostic cli-explicit-mir-run
expect_route_admission cli-explicit-mir-run run explicit-mir "$mir_digest"
expect_route_disposition cli-explicit-mir-run 'explicit canonical' "$mir_digest"

run_stage cli-default-build 0 env "MIMI_STDLIB=$ROOT_DIR/std" MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
    "$mimi_cli" build "$ROOT_DIR/$FIXTURE" --output "$artifact_dir/cli-default-build/program"
default_binary="$artifact_dir/cli-default-build/program"
if [[ ! -x "$default_binary" ]]; then
    record_error "default native build did not produce an executable"
else
    expect_route_admission cli-default-build build default "$mir_digest"
    expect_route_disposition cli-default-build canonical "$mir_digest"
    expect_native_build_receipt cli-default-build default "$mir_digest"
fi
run_stage cli-default-native-run 1 env "$default_binary"
expect_stdout_true cli-default-native-run
expect_no_runtime_failure_diagnostic cli-default-native-run

run_stage cli-explicit-mir-build 0 env "MIMI_STDLIB=$ROOT_DIR/std" MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
    "$mimi_cli" build "$ROOT_DIR/$FIXTURE" --output "$artifact_dir/cli-explicit-mir-build/program" --mir
explicit_binary="$artifact_dir/cli-explicit-mir-build/program"
if [[ ! -x "$explicit_binary" ]]; then
    record_error "explicit MIR native build did not produce an executable"
else
    expect_route_admission cli-explicit-mir-build build explicit-mir "$mir_digest"
    expect_native_build_receipt cli-explicit-mir-build explicit-mir "$mir_digest"
fi
run_stage cli-explicit-mir-native-run 1 env "$explicit_binary"
expect_stdout_true cli-explicit-mir-native-run
expect_no_runtime_failure_diagnostic cli-explicit-mir-native-run

run_stage cli-default-verify 0 env "MIMI_STDLIB=$ROOT_DIR/std" MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
    "$mimi_cli" verify "$ROOT_DIR/$FIXTURE"
if ! grep -Fq 'No contracts to verify' "$artifact_dir/cli-default-verify/stdout"; then
    record_error "default verify CLI probe did not preserve the fixture no-contract result"
fi
expect_route_admission cli-default-verify verify default "$mir_digest"
expect_route_disposition cli-default-verify canonical "$mir_digest"
expect_verifier_provenance cli-default-verify "$mir_digest"

run_stage cli-explicit-mir-verify 0 env "MIMI_STDLIB=$ROOT_DIR/std" MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
    "$mimi_cli" verify "$ROOT_DIR/$FIXTURE" --mir
if ! grep -Fq 'No contracts to verify' "$artifact_dir/cli-explicit-mir-verify/stdout"; then
    record_error "explicit MIR verify CLI probe did not preserve the fixture no-contract result"
fi
expect_route_admission cli-explicit-mir-verify verify explicit-mir "$mir_digest"
expect_verifier_provenance cli-explicit-mir-verify "$mir_digest"

source_snapshot_manifest after || record_error "could not create the after source snapshot"
global_readiness_after=unknown
check_global_readiness after global_readiness_after
sha256sum -- "$artifact_dir/source-snapshot.after.manifest.sha256" \
    | awk '{print $1}' > "$artifact_dir/source-snapshot.after.sha256"
git rev-parse HEAD > "$artifact_dir/head.after.txt"
git branch --show-current > "$artifact_dir/branch.after.txt"
git status --short --untracked-files=no > "$artifact_dir/git-status.after.txt"
source_tree_identical=false
if cmp -s "$artifact_dir/source-snapshot.before.manifest.sha256" \
    "$artifact_dir/source-snapshot.after.manifest.sha256" && \
    cmp -s "$artifact_dir/source-snapshot.before.paths.nul" \
    "$artifact_dir/source-snapshot.after.paths.nul"; then
    source_tree_identical=true
else
    record_error "source tree paths or hashes changed during ZeroMissI32 L1/L2 capture"
fi
global_readiness_remained_false=false
if [[ "$global_readiness_before" == false && "$global_readiness_after" == false ]]; then
    global_readiness_remained_false=true
fi
if [[ "$global_readiness_remained_false" != true ]]; then
    record_error "global Map/Any production readiness did not remain false"
fi
if [[ "$route_admission_count" -ne 6 ]]; then
    record_error "expected six CLI route-admission receipt_identity_digest observations, found $route_admission_count"
fi
if [[ ! "$route_admission_receipt_identity_digest" =~ ^[0-9a-f]{64}$ ]]; then
    record_error "CLI route-admission receipt_identity_digest was not a lowercase SHA-256 value"
fi
if [[ "$verifier_source_hash_observation_count" -ne 2 ]]; then
    record_error "expected source_hash observations from both verifier modes, found $verifier_source_hash_observation_count"
fi
if [[ ! "$verifier_source_hash" =~ ^[0-9a-f]{64}$ || \
    "$verifier_source_hash_matches_across_modes" != true ]]; then
    record_error "default and explicit MIR verifier source_hash values were not identical valid BLAKE3 digests"
fi
if ! cmp -s "$artifact_dir/head.before.txt" "$artifact_dir/head.after.txt" || \
    ! cmp -s "$artifact_dir/branch.before.txt" "$artifact_dir/branch.after.txt"; then
    record_error "branch or HEAD changed during ZeroMissI32 L1/L2 capture"
fi

manifest_validation_log="$artifact_dir/manifest-validation.log"
: > "$manifest_validation_log"
: > "$artifact_dir/manifest-validation.exit"
if (
    cd "$artifact_dir"
    find . -type f \
        ! -name artifacts.sha256 \
        ! -name artifacts.sha256.sha256 \
        ! -name artifact-validation.txt \
        ! -name manifest-validation.log \
        ! -name manifest-validation.exit \
        -print0 | sort -z | xargs -0 -r sha256sum > artifacts.sha256 &&
        sha256sum --check artifacts.sha256 &&
        sha256sum artifacts.sha256 > artifacts.sha256.sha256 &&
        sha256sum --check artifacts.sha256.sha256
) >| "$manifest_validation_log" 2>&1; then
    manifest_digests_verified=true
    printf '0\n' >| "$artifact_dir/manifest-validation.exit"
else
    manifest_status=$?
    printf '%s\n' "$manifest_status" >| "$artifact_dir/manifest-validation.exit"
    record_error "artifact evidence manifest generation or digest verification failed (exit=$manifest_status)"
fi

write_validation_report

printf 'artifact_dir=%s\nacceptance_test_exit=%s\n' "$artifact_dir" "$failed"
if [[ "$failed" -ne 0 ]]; then
    cat "$artifact_dir/artifact-validation.txt" >&2
    exit 1
fi
cat "$artifact_dir/artifact-validation.txt"
