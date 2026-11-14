#!/usr/bin/env bash
set -Eeuo pipefail
set -o noclobber

usage() {
    cat <<'USAGE'
Usage: scripts/verify-ordinary-map-owner-observer-v1.sh [RUN_ID]

Runs exact unit, CLI, and strict Valgrind L3 selectors for the V1 ordinary
Map lifecycle observer slice. Writes unique, read-only evidence under
evidence/0.1.11-dev/gates/ordinary-map-owner-observer-v1-RUN_ID.

RUN_ID uses lowercase letters, digits, and hyphens. Existing evidence is
never overwritten. With no RUN_ID, a unique UTC-based identifier is used.
USAGE
}
if [[ $# -eq 1 && ( "$1" == --help || "$1" == -h ) ]]; then usage; exit 0; fi
if [[ $# -gt 1 ]]; then usage >&2; exit 2; fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT_DIR="$(cd -- "$script_dir/.." && pwd -P)"
GATE_ROOT="$ROOT_DIR/evidence/0.1.11-dev/gates"
if [[ $# -eq 1 ]]; then
    run_id="$1"
else
    run_id="current-$(date -u +%Y%m%dT%H%M%SZ)-$$-$RANDOM"
fi
if [[ ! "$run_id" =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]]; then
    printf 'invalid RUN_ID: %s\n' "$run_id" >&2
    usage >&2
    exit 2
fi
artifact_dir="$GATE_ROOT/ordinary-map-owner-observer-v1-$run_id"
if [[ -e "$artifact_dir" || -L "$artifact_dir" ]]; then
    printf 'refusing to overwrite existing evidence directory: %s\n' "$artifact_dir" >&2
    exit 2
fi
if ! command -v sha256sum >/dev/null 2>&1; then
    printf 'sha256sum is required for evidence checksums\n' >&2
    exit 127
fi
mkdir -p -- "$GATE_ROOT"
mkdir -- "$artifact_dir"
artifact_dir="$(cd -- "$artifact_dir" && pwd -P)"

if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper; fi
if [[ ! -v CARGO_BUILD_JOBS || -z "$CARGO_BUILD_JOBS" ]]; then CARGO_BUILD_JOBS=1; fi
export LLVM_SYS_181_PREFIX CARGO_BUILD_JOBS

snapshot_sources() {
    local destination="$1"
    (
        cd -- "$ROOT_DIR"
        {
            find src tests std scripts -type f -print0
            if [[ -d .cargo ]]; then find .cargo -type f -print0; fi
            for file in Cargo.toml Cargo.lock build.rs rust-toolchain rust-toolchain.toml; do
                [[ ! -f "$file" ]] || printf '%s\0' "$file"
            done
        } | LC_ALL=C sort -zu | while IFS= read -r -d '' file; do sha256sum -- "$file"; done
    ) > "$destination"
}
hash_file() { sha256sum -- "$1" | awk '{print $1}'; }

finish() {
    local status="$?"
    local snapshot_status=0
    trap - EXIT
    set +e
    if [[ -d "$artifact_dir" ]]; then
        if [[ -f "$artifact_dir/source.sha256.before" ]]; then
            snapshot_sources "$artifact_dir/source.sha256.after"
            snapshot_status=$?
            if [[ "$snapshot_status" -eq 0 ]]; then
                hash_file "$artifact_dir/source.sha256.after" > "$artifact_dir/source.sha256.after.digest"
                cmp -s -- "$artifact_dir/source.sha256.before" "$artifact_dir/source.sha256.after" || snapshot_status=98
            fi
        fi
        local unchanged=false
        if [[ "$snapshot_status" -eq 0 && -f "$artifact_dir/source.sha256.after" ]] \
            && cmp -s -- "$artifact_dir/source.sha256.before" "$artifact_dir/source.sha256.after"; then
            unchanged=true
        fi
        if [[ "$status" -eq 0 && "$snapshot_status" -ne 0 ]]; then status=98; fi
        {
            printf 'exit_code=%s\nsource_tree_unchanged=%s\n' "$status" "$unchanged"
            if [[ "$snapshot_status" -eq 98 ]]; then
                printf 'source_snapshot_error=source files changed during the run\n'
            elif [[ "$snapshot_status" -ne 0 ]]; then
                printf 'source_snapshot_error=after snapshot failed with status %s\n' "$snapshot_status"
            fi
        } > "$artifact_dir/run.status.txt"
        if [[ ! -e "$artifact_dir/SHA256SUMS" ]]; then
            (
                cd -- "$artifact_dir"
                find . -type f ! -name SHA256SUMS ! -name .SHA256SUMS.tmp -print0 \
                    | LC_ALL=C sort -z \
                    | while IFS= read -r -d '' file; do sha256sum -- "$file"; done
            ) > "$artifact_dir/.SHA256SUMS.tmp"
            if [[ $? -eq 0 ]]; then
                mv -- "$artifact_dir/.SHA256SUMS.tmp" "$artifact_dir/SHA256SUMS"
            else
                rm -f -- "$artifact_dir/.SHA256SUMS.tmp"
                printf 'checksum_manifest_error=true\n' >> "$artifact_dir/run.status.txt"
                [[ "$status" -ne 0 ]] || status=99
            fi
        fi
        printf 'Evidence directory: %s\n' "$artifact_dir"
        [[ ! -f "$artifact_dir/SHA256SUMS" ]] || chmod -R a-w -- "$artifact_dir"
    fi
    exit "$status"
}
trap finish EXIT

cd -- "$ROOT_DIR"
printf 'run_id=%s\nstarted_utc=%s\nworking_directory=%q\n' \
    "$run_id" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$ROOT_DIR" > "$artifact_dir/run.info.txt"
printf '%q\0' "$0" "$@" > "$artifact_dir/runner.argv.nul"
printf '%q ' "$0" "$@" > "$artifact_dir/runner.argv.txt"
printf '\n' >> "$artifact_dir/runner.argv.txt"
snapshot_sources "$artifact_dir/source.sha256.before"
hash_file "$artifact_dir/source.sha256.before" > "$artifact_dir/source.sha256.before.digest"

valgrind_unsets=(-u VALGRIND_OPTS)
valgrind_names=(VALGRIND_OPTS)
while IFS= read -r name; do
    case "$name" in
        VALGRIND_OPTS) ;;
        VALGRIND_*) valgrind_unsets+=(-u "$name"); valgrind_names+=("$name");;
    esac
done < <(compgen -e | LC_ALL=C sort -u)
test_only_unsets=(-u MIMI_TEST_FORCE_NO_Z3 -u MIMI_OBSERVER_V1_EVIDENCE_DIR)

record_environment() {
    local destination="$1"
    {
        printf 'working_directory=%q\nLLVM_SYS_181_PREFIX=%q\nCARGO_BUILD_JOBS=%q\nCARGO_TERM_COLOR=never\n' \
            "$ROOT_DIR" "$LLVM_SYS_181_PREFIX" "$CARGO_BUILD_JOBS"
        printf 'VALGRIND_variables_unset='
        if [[ ${#valgrind_names[@]} -eq 0 ]]; then printf '(none)\n'; else printf '%q ' "${valgrind_names[@]}"; printf '\n'; fi
        printf 'test_process_variables_unset=MIMI_TEST_FORCE_NO_Z3 (test override disabled so verifier tests cannot silently take the no-Z3 return path)\n'
        printf 'inherited_environment=other variables retained; credential-like values redacted by name\n'
        env -0 | while IFS= read -r -d '' variable; do
            local key="${variable%%=*}" value="${variable#*=}"
            case "$key" in VALGRIND_*) continue;; esac
            if [[ "$key" =~ (TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|AUTH|COOKIE|API_KEY|PRIVATE_KEY|PROXY|URL|URI|DSN) ]]; then
                printf '%s=<redacted>\n' "$key"
            else
                printf '%s=%q\n' "$key" "$value"
            fi
        done | LC_ALL=C sort
        printf 'Cargo test resolves Cargo-built mimi through CARGO_BIN_EXE_mimi when available, else beside its test executable; runner supplies no manual mimi path.\n'
    } > "$destination"
}
record_environment "$artifact_dir/runner.environment.txt"

record_tool() {
    local label="$1" command_name="$2"; shift 2
    local directory="$artifact_dir/toolchain/$label" executable status
    mkdir -p -- "$directory"
    executable="$(command -v -- "$command_name" 2>/dev/null || true)"
    if [[ -z "$executable" ]]; then
        executable="$command_name"; status=127
        printf '%s was not found on PATH\n' "$command_name" > "$directory/stderr.bin"
        : > "$directory/stdout.bin"
    elif env "${valgrind_unsets[@]}" "$executable" "$@" > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    printf '%q ' "$executable" "$@" > "$directory/command.txt"
    printf '\n' >> "$directory/command.txt"
    printf '%s\0' "$executable" "$@" > "$directory/command.argv.nul"
    cp -- "$artifact_dir/runner.environment.txt" "$directory/environment.txt"
    if [[ -f "$executable" ]]; then sha256sum -- "$executable" > "$directory/executable.sha256" 2>/dev/null || true; fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$directory/process.status"
    return "$status"
}

mkdir -- "$artifact_dir/toolchain"
record_tool bash bash --version || bash_version_status=$?
record_tool cargo cargo --version --verbose || cargo_version_status=$?
record_tool rustc rustc --version --verbose || rustc_version_status=$?
record_tool valgrind valgrind --version || valgrind_version_status=$?
record_tool sha256sum sha256sum --version || checksum_version_status=$?
llvm_config="$LLVM_SYS_181_PREFIX/bin/llvm-config"
if [[ ! -x "$llvm_config" && -x "$LLVM_SYS_181_PREFIX/llvm-config" ]]; then llvm_config="$LLVM_SYS_181_PREFIX/llvm-config"; fi
if [[ -x "$llvm_config" ]]; then
    record_tool llvm-config "$llvm_config" --version || llvm_version_status=$?
else
    mkdir -- "$artifact_dir/toolchain/llvm-config"
    printf '%q --version\n' "$llvm_config" > "$artifact_dir/toolchain/llvm-config/command.txt"
    printf '%s\0' "$llvm_config" --version > "$artifact_dir/toolchain/llvm-config/command.argv.nul"
    cp -- "$artifact_dir/runner.environment.txt" "$artifact_dir/toolchain/llvm-config/environment.txt"
    printf 'llvm-config missing under LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX" > "$artifact_dir/toolchain/llvm-config/stderr.bin"
    : > "$artifact_dir/toolchain/llvm-config/stdout.bin"
    printf 'exit_code=127\nsuccess=false\n' > "$artifact_dir/toolchain/llvm-config/process.status"
    llvm_version_status=127
fi
{
    printf 'LLVM_SYS_181_PREFIX=%s\nCARGO_BUILD_JOBS=%s\n' "$LLVM_SYS_181_PREFIX" "$CARGO_BUILD_JOBS"
    printf 'cargo_version_status=%s\nrustc_version_status=%s\nvalgrind_version_status=%s\nllvm_config_version_status=%s\n' \
        "${cargo_version_status:-0}" "${rustc_version_status:-0}" "${valgrind_version_status:-0}" "${llvm_version_status:-0}"
} > "$artifact_dir/toolchain.txt"

cargo_bin="$(command -v -- cargo 2>/dev/null || true)"
rustc_bin="$(command -v -- rustc 2>/dev/null || true)"
if [[ -z "$cargo_bin" ]]; then printf 'cargo unavailable; exact selectors cannot run\n' >&2; exit 127; fi
if [[ -z "$rustc_bin" ]]; then printf 'rustc unavailable for Cargo tests\n' >&2; exit 127; fi

expected_test_count=20
validated_test_count=0

run_exact_test() {
    local label="$1" layer="$2" target_kind="$3" selector="$4" ignored="$5" target_name="${6:-}"
    local directory="$artifact_dir/tests/$label" expected_line summary running_count ok_count status raw_evidence
    local -a args scoped_env=()
    mkdir -- "$directory"
    args=(test --manifest-path "$ROOT_DIR/Cargo.toml")
    case "$target_kind" in
        --lib) args+=(--lib);;
        --bin) args+=(--bin "$target_name");;
        --test) args+=(--test "$target_name");;
        *) printf 'unsupported target kind: %s\n' "$target_kind" >&2; return 2;;
    esac
    args+=("$selector" -- --exact --test-threads=1)
    [[ "$ignored" != true ]] || args+=(--ignored)
    printf '%q ' "$cargo_bin" "${args[@]}" > "$directory/command.txt"
    printf '\n' >> "$directory/command.txt"
    printf '%s\0' "$cargo_bin" "${args[@]}" > "$directory/command.argv.nul"
    cp -- "$artifact_dir/runner.environment.txt" "$directory/environment.txt"
    printf 'layer=%s\ntarget_kind=%s\ntarget_name=%s\nselector=%s\nignored_selector=%s\nexpected_selected=1\nexpected_passed=1\nexpected_failed=0\nexpected_ignored=0\n' \
        "$layer" "$target_kind" "$target_name" "$selector" "$ignored" > "$directory/selection.txt"
    if [[ "$layer" == verifier-* ]]; then
        printf 'requires_z3=true\nreject_output_marker=skipped (Z3 not available)\n' \
            >> "$directory/selection.txt"
    fi
    printf '%q\n' "$ROOT_DIR" > "$directory/working-directory.txt"

    if [[ "$label" == strict-valgrind-l3-native-binaries ]]; then
        raw_evidence="$directory/raw-memcheck"
        [[ ! -e "$raw_evidence" && ! -L "$raw_evidence" ]] || {
            printf 'refusing to reuse raw Memcheck evidence path: %s\n' "$raw_evidence" >&2
            return 2
        }
        scoped_env+=("MIMI_OBSERVER_V1_EVIDENCE_DIR=$raw_evidence")
        printf 'MIMI_OBSERVER_V1_EVIDENCE_DIR=%q\n' "$raw_evidence" >> "$directory/environment.txt"
    fi

    status=0
    if env "${valgrind_unsets[@]}" "${test_only_unsets[@]}" LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" CARGO_TERM_COLOR=never "${scoped_env[@]}" \
        "$cargo_bin" "${args[@]}" > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$directory/process.status"
    { printf '===== stdout =====\n'; cat -- "$directory/stdout.bin"; printf '\n===== stderr =====\n'; cat -- "$directory/stderr.bin"; } \
        > "$directory/combined.log"
    cat -- "$directory/stdout.bin"
    cat -- "$directory/stderr.bin" >&2
    if [[ "$status" -ne 0 ]]; then
        printf 'validation=failed\nreason=Cargo exit %s; ignored tests are never accepted as a skip\n' "$status" \
            > "$directory/validation.txt"
        return "$status"
    fi

    if [[ "$layer" == verifier-* ]] \
        && grep -Fq 'skipped (Z3 not available)' "$directory/combined.log"; then
        printf 'validation=failed\nreason=Z3 unavailable; verifier test returned early instead of exercising its assertions\n' \
            > "$directory/validation.txt"
        printf 'verifier selector did not exercise assertions because Z3 is unavailable: %s\n' \
            "$selector" >&2
        return 1
    fi

    expected_line="test $selector ... ok"
    running_count="$(grep -Ec '^running [0-9]+ tests?$' "$directory/combined.log" || true)"
    ok_count="$(grep -Fxc -- "$expected_line" "$directory/combined.log" || true)"
    summary="$(grep -E '^test result: ' "$directory/combined.log" | tail -n 1 || true)"
    local summary_ok=0
    if grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;' <<< "$summary"; then
        summary_ok=1
    fi
    if [[ "$running_count" -ne 1 || "$ok_count" -ne 1 || "$summary_ok" -ne 1 ]]; then
        printf 'validation=failed\nexpected_selected=1\nobserved_running_lines=%s\nexpected_passed=1\nobserved_exact_ok_lines=%s\nexpected_failed=0\nexpected_ignored=0\nobserved_summary=%s\n' \
            "$running_count" "$ok_count" "$summary" > "$directory/validation.txt"
        printf 'exact selector validation failed: %s\n' "$selector" >&2
        return 1
    fi
    if [[ "$label" == strict-valgrind-l3-native-binaries ]]; then
        local required_file
        for required_file in \
            control.memcheck.log control.still-reachable.txt control-memcheck-process.status.txt \
            fixture-default.memcheck.log fixture-default.still-reachable.txt fixture-default-memcheck-process.status.txt \
            fixture-explicit-mir.memcheck.log fixture-explicit-mir.still-reachable.txt fixture-explicit-mir-memcheck-process.status.txt \
            product-default.memcheck.log product-default.still-reachable.txt product-default-memcheck-process.status.txt \
            product-explicit-mir.memcheck.log product-explicit-mir.still-reachable.txt product-explicit-mir-memcheck-process.status.txt \
            still-reachable-comparison.txt; do
            if [[ ! -s "$raw_evidence/$required_file" ]]; then
                printf 'validation=failed\nreason=missing durable raw L3 evidence: %s\n' "$required_file" \
                    > "$directory/validation.txt"
                printf 'strict L3 did not retain required raw evidence: %s\n' "$required_file" >&2
                return 1
            fi
        done
        local report_count
        report_count="$(find "$raw_evidence" -maxdepth 1 -type f -name '*.memcheck.log' | wc -l)"
        if [[ "$report_count" -ne 5 ]]; then
            printf 'validation=failed\nreason=expected exactly five raw Memcheck reports, found %s\n' "$report_count" \
                > "$directory/validation.txt"
            printf 'strict L3 retained %s Valgrind reports, expected 5\n' "$report_count" >&2
            return 1
        fi
    fi
    validated_test_count=$((validated_test_count + 1))
    {
        printf 'validation=passed\nselector=%s\nselected_tests=1\npassed_tests=1\nfailed_tests=0\nignored_tests=0\ncargo_exit_code=0\nsummary=%s\n' \
            "$selector" "$summary"
        if [[ "$label" == strict-valgrind-l3-native-binaries ]]; then
            printf 'raw_memcheck_evidence=%s\nraw_memcheck_reports=%s\n' "$raw_evidence" "$report_count"
        fi
    } > "$directory/validation.txt"
}

run_exact_group() {
    local label="$1" layer="$2" filter="$3"; shift 3
    local directory="$artifact_dir/tests/$label" expected_count running_count ok_count bad_count summary status
    local -a args=(test --manifest-path "$ROOT_DIR/Cargo.toml" --lib "$filter" -- --test-threads=1)
    local -a selectors=("$@")
    expected_count="${#selectors[@]}"
    mkdir -- "$directory"
    printf '%q ' "$cargo_bin" "${args[@]}" > "$directory/command.txt"
    printf '\n' >> "$directory/command.txt"
    printf '%s\0' "$cargo_bin" "${args[@]}" > "$directory/command.argv.nul"
    cp -- "$artifact_dir/runner.environment.txt" "$directory/environment.txt"
    {
        printf 'layer=%s\ntarget_kind=--lib\nfilter=%s\nselection_mode=exact expected-name set validated from libtest output\n' \
            "$layer" "$filter"
        printf 'expected_selected=%s\nexpected_passed=%s\nexpected_failed=0\nexpected_ignored=0\n' \
            "$expected_count" "$expected_count"
        printf '%s\n' "${selectors[@]}"
    } > "$directory/selection.txt"
    printf '%q\n' "$ROOT_DIR" > "$directory/working-directory.txt"
    printf '%s\n' "${selectors[@]}" | LC_ALL=C sort > "$directory/expected-test-names.txt"

    status=0
    if env "${valgrind_unsets[@]}" "${test_only_unsets[@]}" LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" CARGO_TERM_COLOR=never \
        "$cargo_bin" "${args[@]}" > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$directory/process.status"
    { printf '===== stdout =====\n'; cat -- "$directory/stdout.bin"; printf '\n===== stderr =====\n'; cat -- "$directory/stderr.bin"; } \
        > "$directory/combined.log"
    cat -- "$directory/stdout.bin"
    cat -- "$directory/stderr.bin" >&2
    if [[ "$status" -ne 0 ]]; then
        printf 'validation=failed\nreason=Cargo exit %s\n' "$status" > "$directory/validation.txt"
        return "$status"
    fi

    sed -nE 's/^test ([^ ]+) \.\.\. (ok|FAILED|ignored)$/\1/p' "$directory/combined.log" \
        | LC_ALL=C sort > "$directory/observed-test-names.txt"
    running_count="$(grep -Ec '^running [0-9]+ tests?$' "$directory/combined.log" || true)"
    ok_count="$(grep -Ec '^test [^ ]+ \.\.\. ok$' "$directory/combined.log" || true)"
    bad_count="$(grep -Ec '^test [^ ]+ \.\.\. (FAILED|ignored)$' "$directory/combined.log" || true)"
    summary="$(grep -E '^test result: ' "$directory/combined.log" | tail -n 1 || true)"
    local summary_ok=0 names_match=0
    grep -Eq "^test result: ok\. ${expected_count} passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;" \
        <<< "$summary" && summary_ok=1
    cmp -s -- "$directory/expected-test-names.txt" "$directory/observed-test-names.txt" && names_match=1
    if [[ "$running_count" -ne 1 || "$ok_count" -ne "$expected_count" || "$bad_count" -ne 0 \
        || "$summary_ok" -ne 1 || "$names_match" -ne 1 ]]; then
        printf 'validation=failed\nexpected_selected=%s\nobserved_running_lines=%s\nexpected_passed=%s\nobserved_ok_lines=%s\nobserved_failed_or_ignored_lines=%s\nexpected_names_match=%s\nobserved_summary=%s\n' \
            "$expected_count" "$running_count" "$expected_count" "$ok_count" "$bad_count" \
            "$names_match" "$summary" > "$directory/validation.txt"
        printf 'exact selector group validation failed: %s\n' "$label" >&2
        return 1
    fi
    validated_test_count=$((validated_test_count + expected_count))
    printf 'validation=passed\nselected_names_exact=true\nselected_tests=%s\npassed_tests=%s\nfailed_tests=0\nignored_tests=0\ncargo_exit_code=0\nsummary=%s\n' \
        "$expected_count" "$expected_count" "$summary" > "$directory/validation.txt"
}

mkdir -- "$artifact_dir/tests"
cat > "$artifact_dir/gate-plan.txt" <<'PLAN'
The 15 library selectors sharing one test binary are run in one Cargo process
to avoid recompiling it per test. The runner compares the complete observed
libtest name set against the exact expected set and requires all 15 to pass.
The remaining five selectors use exact individual invocations. The last is
explicitly run with --ignored. Missing Valgrind is a hard L3 failure. Raw L3
Valgrind/build logs remain in the evidence directory. Cargo supplies the fresh
mimi executable to the integration test via CARGO_BIN_EXE_mimi.

--lib prefix ordinary_map_owner_observer_v1_: exactly 15 listed below
core::cfg::resource_lower::tests::ordinary_map_owner_observer_v1_receipts_are_exactly_once_and_effect_typed
core::cfg::resource_lower::tests::ordinary_map_owner_observer_v1_records_bound_unit_observer_once
core::cfg::resource_lower::tests::ordinary_map_owner_observer_v1_rejects_extra_map_uses_and_gets
core::mir::route::tests::ordinary_map_owner_observer_v1_materializes_one_exact_borrowed_route_receipt
core::mir::route::tests::ordinary_map_owner_observer_v1_rejects_an_uncovered_map_operation
interp::bytecode::mir::tests::ordinary_map_owner_observer_v1_json_is_canonical_and_borrows_live_root
interp::bytecode::mir::tests::ordinary_map_owner_observer_v1_allocation_failures_are_e0800_and_atomic
interp::bytecode::mir::tests::ordinary_map_owner_observer_v1_tampering_fails_before_first_output
interp::bytecode::mir::tests::ordinary_map_owner_observer_v1_rejects_wrong_or_foreign_route_receipts
interp::bytecode::vm::shared_string_publication_tests::ordinary_map_owner_observer_v1_continuing_denial_returns_trap_and_discards_frames
interp::bytecode::vm::shared_string_publication_tests::ordinary_map_owner_observer_v1_real_shared_allocation_failure_is_atomic
verifier::tests::ordinary_map_owner_observer_v1_reports_reachable_capacity_after_exact_json_projection
verifier::tests::ordinary_map_owner_observer_v1_rejects_unknown_keys_and_values_without_proof
verifier::tests::ordinary_map_owner_observer_v1_rejects_mismatched_or_tampered_route_receipts
verifier::tests::ordinary_map_owner_observer_v1_rejects_tampered_checker_mir_projection

--bin mimi canonical_dispatch::tests::ordinary_map_owner_observer_v1_uses_its_exact_receipt_for_default_and_explicit_routes
--lib codegen::tests::ordinary_map_owner_lifecycle_observer_v1_native_uses_borrowed_json_and_owned_string_drop
--test canonical_mir_ordinary_map_owner_observer_v1_cli observer_v1_run_build_verify_use_the_same_default_and_explicit_mir_receipt
--test canonical_mir_ordinary_map_owner_observer_v1_cli product_observer_v1_run_build_verify_use_same_default_and_explicit_mir_receipt
--test canonical_mir_ordinary_map_owner_observer_v1_cli --ignored observer_v1_default_and_explicit_native_bins_pass_strict_memcheck_l3
PLAN

run_exact_group observer-v1-library-coverage checker-route-bytecode-verifier ordinary_map_owner_observer_v1_ \
    core::cfg::resource_lower::tests::ordinary_map_owner_observer_v1_receipts_are_exactly_once_and_effect_typed \
    core::cfg::resource_lower::tests::ordinary_map_owner_observer_v1_records_bound_unit_observer_once \
    core::cfg::resource_lower::tests::ordinary_map_owner_observer_v1_rejects_extra_map_uses_and_gets \
    core::mir::route::tests::ordinary_map_owner_observer_v1_materializes_one_exact_borrowed_route_receipt \
    core::mir::route::tests::ordinary_map_owner_observer_v1_rejects_an_uncovered_map_operation \
    interp::bytecode::mir::tests::ordinary_map_owner_observer_v1_json_is_canonical_and_borrows_live_root \
    interp::bytecode::mir::tests::ordinary_map_owner_observer_v1_allocation_failures_are_e0800_and_atomic \
    interp::bytecode::mir::tests::ordinary_map_owner_observer_v1_tampering_fails_before_first_output \
    interp::bytecode::mir::tests::ordinary_map_owner_observer_v1_rejects_wrong_or_foreign_route_receipts \
    interp::bytecode::vm::shared_string_publication_tests::ordinary_map_owner_observer_v1_continuing_denial_returns_trap_and_discards_frames \
    interp::bytecode::vm::shared_string_publication_tests::ordinary_map_owner_observer_v1_real_shared_allocation_failure_is_atomic \
    verifier::tests::ordinary_map_owner_observer_v1_reports_reachable_capacity_after_exact_json_projection \
    verifier::tests::ordinary_map_owner_observer_v1_rejects_unknown_keys_and_values_without_proof \
    verifier::tests::ordinary_map_owner_observer_v1_rejects_mismatched_or_tampered_route_receipts \
    verifier::tests::ordinary_map_owner_observer_v1_rejects_tampered_checker_mir_projection
run_exact_test mir-default-explicit-selector mir-selector --bin \
    canonical_dispatch::tests::ordinary_map_owner_observer_v1_uses_its_exact_receipt_for_default_and_explicit_routes false mimi
run_exact_test native-codegen-borrow-drop native-codegen --lib \
    codegen::tests::ordinary_map_owner_lifecycle_observer_v1_native_uses_borrowed_json_and_owned_string_drop false
run_exact_test cli-default-explicit-runtime cli-run-build-verify --test \
    observer_v1_run_build_verify_use_the_same_default_and_explicit_mir_receipt false \
    canonical_mir_ordinary_map_owner_observer_v1_cli
run_exact_test cli-product-default-explicit-runtime cli-run-build-verify-product --test \
    product_observer_v1_run_build_verify_use_same_default_and_explicit_mir_receipt false \
    canonical_mir_ordinary_map_owner_observer_v1_cli
run_exact_test strict-valgrind-l3-native-binaries strict-valgrind-l3 --test \
    observer_v1_default_and_explicit_native_bins_pass_strict_memcheck_l3 true \
    canonical_mir_ordinary_map_owner_observer_v1_cli

if [[ "$validated_test_count" -ne "$expected_test_count" ]]; then
    printf 'validation=failed\nexpected_test_count=%s\nvalidated_test_count=%s\n' \
        "$expected_test_count" "$validated_test_count" > "$artifact_dir/selection-summary.txt"
    printf 'test count mismatch: expected %s, validated %s\n' \
        "$expected_test_count" "$validated_test_count" >&2
    exit 1
fi
printf 'expected_test_count=%s\nvalidated_test_count=%s\nexpected_total_selected=%s\nexpected_total_passed=%s\nexpected_total_failed=0\nexpected_total_ignored=0\n' \
    "$expected_test_count" "$validated_test_count" "$expected_test_count" \
    "$expected_test_count" \
    > "$artifact_dir/selection-summary.txt"
printf 'all_expected_tests_passed=true\nvalidated_evidence_layers=checker,mir-route,mir-selector,bytecode-lowering-vm,bytecode-allocation,bytecode-tampering,bytecode-route-receipt,verifier-capacity-projection,verifier-negative,verifier-route-tampering,verifier-checker-mir-projection,native-codegen,cli-run-build-verify,strict-valgrind-l3\n' \
    > "$artifact_dir/gate-result.txt"
