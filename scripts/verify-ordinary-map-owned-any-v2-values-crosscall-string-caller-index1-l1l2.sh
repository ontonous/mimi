#!/usr/bin/env bash
# Serial, source-bound L1/L2 evidence for the Values cross-call String caller Index1 profile.
set -euo pipefail
set -o noclobber

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
RUNNER_REL=scripts/verify-ordinary-map-owned-any-v2-values-crosscall-string-caller-index1-l1l2.sh
FIXTURE_REL=tests/fixtures/mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1.mimi
REFERENCE_TARGET=canonical_mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_reference
REFERENCE_TEST=values_crosscall_string_caller_index1_executes_same_checker_mir_receipt_in_reference_and_bytecode
CLI_TARGET=canonical_mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_cli
CLI_POSITIVE_TEST=values_crosscall_string_caller_index1_uses_its_exact_cli_route
CLI_NEAR_MISS_TEST=values_crosscall_string_caller_index1_near_misses_fail_closed
ARTIFACT_ENV=MIMI_VALUES_CROSSCALL_INDEX1_L1L2_ARTIFACT_DIR
TARGET_OVERRIDE_ENV=MIMI_VALUES_CROSSCALL_INDEX1_TARGET_DIR

for tool in bash cargo rustc sha256sum realpath git find sort cmp grep awk env; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'Values cross-call Index1 L1/L2 capture requires %s\n' "$tool" >&2
        exit 1
    }
done

LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
[[ -d "$LLVM_SYS_181_PREFIX" ]] || {
    printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
}
LLVM_SYS_181_PREFIX="$(realpath -- "$LLVM_SYS_181_PREFIX")"
if [[ -x "$LLVM_SYS_181_PREFIX/bin/llvm-config" ]]; then
    LLVM_CONFIG_BIN="$LLVM_SYS_181_PREFIX/bin/llvm-config"
elif [[ -x "$LLVM_SYS_181_PREFIX/llvm-config" ]]; then
    LLVM_CONFIG_BIN="$LLVM_SYS_181_PREFIX/llvm-config"
elif command -v llvm-config >/dev/null 2>&1; then
    LLVM_CONFIG_BIN="$(command -v llvm-config)"
else
    printf 'llvm-config is unavailable under LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi

CARGO_BIN="$(command -v cargo)"
RUSTC_BIN="$(command -v rustc)"
BASH_BIN="$(command -v bash)"
ENV_BIN="$(command -v env)"

# CARGO_TARGET_DIR and the named override are explicit external-cache choices.
# With neither set, resolve the repository target symlink so Cargo writes to the
# configured external cache rather than creating artifacts in this checkout.
if [[ -n "${MIMI_VALUES_CROSSCALL_INDEX1_TARGET_DIR:-}" ]]; then
    TARGET_REQUESTED="$MIMI_VALUES_CROSSCALL_INDEX1_TARGET_DIR"
    TARGET_SOURCE="$TARGET_OVERRIDE_ENV"
elif [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    TARGET_REQUESTED="$CARGO_TARGET_DIR"
    TARGET_SOURCE=CARGO_TARGET_DIR
else
    TARGET_REQUESTED="$(realpath -- "$ROOT_DIR/target")"
    TARGET_SOURCE=resolved-repository-target-symlink
fi
case "$TARGET_REQUESTED" in
    /*) ;;
    *) TARGET_REQUESTED="$ROOT_DIR/$TARGET_REQUESTED" ;;
esac
TARGET_DIR="$(realpath -m -- "$TARGET_REQUESTED")"
case "$TARGET_DIR/" in
    "$ROOT_DIR/"*)
        printf 'Cargo target cache must resolve outside the repository: %s\n' "$TARGET_DIR" >&2
        exit 2
        ;;
esac

if [[ -n "${MIMI_VALUES_CROSSCALL_INDEX1_L1L2_ARTIFACT_DIR:-}" ]]; then
    ARTIFACT_REQUESTED="$MIMI_VALUES_CROSSCALL_INDEX1_L1L2_ARTIFACT_DIR"
else
    ARTIFACT_REQUESTED="$ROOT_DIR/evidence/0.1.11-dev/gates/values-crosscall-string-caller-index1-l1l2-$(date -u +%Y%m%dT%H%M%SZ)-$$"
fi
case "$ARTIFACT_REQUESTED" in
    /*) ;;
    *) ARTIFACT_REQUESTED="$ROOT_DIR/$ARTIFACT_REQUESTED" ;;
esac
ARTIFACT_DIR="$(realpath -m -- "$ARTIFACT_REQUESTED")"
case "$ARTIFACT_DIR/" in
    "$ROOT_DIR/evidence/0.1.11-dev/gates/"*) ;;
    *)
        printf 'L1/L2 artifacts must remain under evidence/0.1.11-dev/gates: %s\n' "$ARTIFACT_DIR" >&2
        exit 2
        ;;
esac
if [[ -e "$ARTIFACT_DIR" || -L "$ARTIFACT_DIR" ]]; then
    printf 'refusing to overwrite or reuse existing Index1 L1/L2 evidence: %s\n' "$ARTIFACT_DIR" >&2
    exit 2
fi
mkdir -p -- "$(dirname -- "$ARTIFACT_DIR")"
mkdir -- "$ARTIFACT_DIR"
ARTIFACT_DIR="$(realpath -- "$ARTIFACT_DIR")"
unset "$ARTIFACT_ENV" "$TARGET_OVERRIDE_ENV"

cd -- "$ROOT_DIR"

source_input_paths() {
    local source_tree
    for source_tree in src std stdlib tests scripts .cargo; do
        if [[ -d "$source_tree" ]]; then
            find "$source_tree" -type f -print0
        fi
    done
    find . -maxdepth 1 -type f \
        \( -name 'Cargo.toml' -o -name 'Cargo.lock' -o -name 'build.rs' \
        -o -name '.gitignore' -o -name 'rust-toolchain' \
        -o -name 'rust-toolchain.toml' -o -name '.rustfmt.toml' \
        -o -name 'rustfmt.toml' \) -print0
    printf '%s\0' "$FIXTURE_REL" "$RUNNER_REL" \
        tests/canonical_mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_reference.rs \
        tests/canonical_mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_cli.rs
}

write_source_manifest() {
    local destination="$1"
    (
        cd -- "$ROOT_DIR"
        source_input_paths | LC_ALL=C sort -zu | while IFS= read -r -d '' input; do
            if [[ -f "$input" ]]; then
                sha256sum -- "$input"
            else
                printf 'MISSING  %s\n' "$input"
            fi
        done
    ) > "$destination"
}

write_fixture_snapshot() {
    local destination="$1"
    (
        cd -- "$ROOT_DIR"
        if [[ -f "$FIXTURE_REL" ]]; then
            sha256sum -- "$FIXTURE_REL"
        else
            printf 'MISSING  %s\n' "$FIXTURE_REL"
        fi
    ) > "$destination"
}

write_vcs_snapshot() {
    local destination="$1"
    (
        cd -- "$ROOT_DIR"
        printf 'repository_root=%s\n' "$(git rev-parse --show-toplevel)"
        printf 'worktree_path=%s\n' "$(pwd -P)"
        printf 'branch='; git branch --show-current
        printf 'head='; git rev-parse HEAD
        printf 'worktrees_begin\n'
        git worktree list --porcelain
        printf 'worktrees_end\n'
    ) > "$destination" 2> "${destination%.txt}.stderr"
}

write_git_status() {
    local destination="$1"
    (cd -- "$ROOT_DIR" && git status --short --branch --untracked-files=all) \
        > "$destination" 2> "${destination%.txt}.stderr"
}

record_tool() {
    local label="$1" executable="$2" status=0 directory="$ARTIFACT_DIR/toolchain/$1"
    shift 2
    mkdir -- "$directory"
    {
        printf '%q' "$executable"
        printf ' %q' "$@"
        printf '\n'
    } > "$directory/command.txt"
    printf '%s\0' "$executable" "$@" > "$directory/command.argv.nul"
    if "$executable" "$@" > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
        status=0
    else
        status=$?
        TOOLCHAIN_STATUS=1
    fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$directory/process.status"
}

RELEVANT_ENVIRONMENT=(
    PATH HOME TMPDIR
    CARGO_HOME RUSTUP_HOME CARGO_TARGET_DIR CARGO_BUILD_JOBS CARGO_INCREMENTAL
    CARGO_TERM_COLOR CARGO_ENCODED_RUSTFLAGS RUSTFLAGS RUSTDOCFLAGS
    RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER RUSTUP_TOOLCHAIN
    LLVM_SYS_181_PREFIX LLVM_CONFIG_PATH LIBCLANG_PATH
    CC CXX AR CFLAGS CXXFLAGS LDFLAGS LD_LIBRARY_PATH LIBRARY_PATH CPATH PKG_CONFIG_PATH
    LC_ALL LANG TZ
)

write_environment() {
    local destination="$1" variable
    {
        printf 'runner=%s\nrepository_root=%s\nworking_directory=%s\n' \
            "$RUNNER_REL" "$ROOT_DIR" "$ROOT_DIR"
        printf 'artifact_directory=%s\nartifact_environment_variable=%s\n' \
            "$ARTIFACT_DIR" "$ARTIFACT_ENV"
        printf 'LLVM_SYS_181_PREFIX=%q\nCARGO_TARGET_DIR=%q\nCARGO_TARGET_SOURCE=%s\nCARGO_BUILD_JOBS=1\ntest_threads=1\n' \
            "$LLVM_SYS_181_PREFIX" "$TARGET_DIR" "$TARGET_SOURCE"
        printf 'cargo_path=%q\nrustc_path=%q\nbash_path=%q\nenv_path=%q\nllvm_config_path=%q\n' \
            "$CARGO_BIN" "$RUSTC_BIN" "$BASH_BIN" "$ENV_BIN" "$LLVM_CONFIG_BIN"
        for variable in "${RELEVANT_ENVIRONMENT[@]}"; do
            if [[ "$variable" == CARGO_TARGET_DIR ]]; then
                printf '%s=%q (effective target is also recorded above)\n' "$variable" "$TARGET_REQUESTED"
            elif [[ -v "$variable" ]]; then
                printf '%s=%q\n' "$variable" "${!variable}"
            else
                printf '%s=<unset>\n' "$variable"
            fi
        done
        while IFS='=' read -r variable _; do
            if [[ "$variable" == MIMI_* ]]; then
                printf '%s=%q\n' "$variable" "${!variable}"
            fi
        done < <(env | LC_ALL=C sort)
    } > "$destination"
}

mkdir -- "$ARTIFACT_DIR/toolchain"
TOOLCHAIN_STATUS=0
record_tool bash "$BASH_BIN" --version
record_tool cargo "$CARGO_BIN" --version --verbose
record_tool rustc "$RUSTC_BIN" --version --verbose
record_tool llvm-config "$LLVM_CONFIG_BIN" --version --prefix --libdir --host-target
write_environment "$ARTIFACT_DIR/environment.txt"

write_vcs_snapshot "$ARTIFACT_DIR/vcs.before.txt"
write_git_status "$ARTIFACT_DIR/git-status.before.txt"
write_source_manifest "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256"
write_fixture_snapshot "$ARTIFACT_DIR/fixture.before.sha256"

validation_errors=()
add_validation_error() {
    validation_errors+=("$1")
}

run_cargo_case() {
    local case_name="$1" filter="$2"
    shift 2
    local case_dir="$ARTIFACT_DIR/$case_name" status=0 expected
    local -a expected_tests=("$@")
    local -a cargo_arguments=(
        test --locked --manifest-path "$ROOT_DIR/Cargo.toml" --target-dir "$TARGET_DIR"
        --lib "$filter" -- --test-threads=1
    )
    local -a invocation=(
        "$ENV_BIN" "LLVM_SYS_181_PREFIX=$LLVM_SYS_181_PREFIX"
        "CARGO_TARGET_DIR=$TARGET_DIR" CARGO_BUILD_JOBS=1
        "$CARGO_BIN" "${cargo_arguments[@]}"
    )

    mkdir -- "$case_dir"
    {
        printf 'cd %q &&' "$ROOT_DIR"
        printf ' %q' "${invocation[@]}"
        printf '\n'
    } > "$case_dir/command.txt"
    printf '%s\0' "${invocation[@]}" > "$case_dir/command.argv.nul"
    printf '%s\0' "$CARGO_BIN" "${cargo_arguments[@]}" > "$case_dir/cargo.argv.nul"
    {
        printf 'cwd=%s\n' "$ROOT_DIR"
        printf 'LLVM_SYS_181_PREFIX=%q\nCARGO_TARGET_DIR=%q\nCARGO_BUILD_JOBS=1\n' \
            "$LLVM_SYS_181_PREFIX" "$TARGET_DIR"
        cat "$ARTIFACT_DIR/environment.txt"
    } > "$case_dir/command.environment.txt"
    if (cd -- "$ROOT_DIR" && "${invocation[@]}") \
        > "$case_dir/stdout.bin" 2> "$case_dir/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    cat "$case_dir/stdout.bin" "$case_dir/stderr.bin" > "$case_dir/combined-for-validation.log"
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$case_dir/process.status"
    if [[ "$status" -ne 0 ]]; then
        add_validation_error "$case_name command exited $status"
    elif ! grep -Fq 'test result: ok.' "$case_dir/combined-for-validation.log"; then
        add_validation_error "$case_name produced no green Rust test summary"
    fi
    if [[ "$status" -eq 0 ]]; then
        for expected in "${expected_tests[@]}"; do
            if ! grep -Fq "$expected ... ok" "$case_dir/combined-for-validation.log"; then
                add_validation_error "$case_name did not report expected passing test $expected"
            fi
        done
    fi
    printf '%s: exit=%s expected_tests=%s\n' \
        "$case_name" "$status" "${#expected_tests[@]}"
}

run_exact_integration_case() {
    local case_name="$1" target="$2" test_name="$3"
    local case_dir="$ARTIFACT_DIR/$case_name" status=0
    shift 3
    local -a cargo_arguments=(
        test --locked --manifest-path "$ROOT_DIR/Cargo.toml" --target-dir "$TARGET_DIR"
        --test "$target" "$test_name" -- --exact --test-threads=1
    )
    local -a invocation=(
        "$ENV_BIN" "LLVM_SYS_181_PREFIX=$LLVM_SYS_181_PREFIX"
        "CARGO_TARGET_DIR=$TARGET_DIR" CARGO_BUILD_JOBS=1
        "$CARGO_BIN" "${cargo_arguments[@]}"
    )

    mkdir -- "$case_dir"
    {
        printf 'cd %q &&' "$ROOT_DIR"
        printf ' %q' "${invocation[@]}"
        printf '\n'
    } > "$case_dir/command.txt"
    printf '%s\0' "${invocation[@]}" > "$case_dir/command.argv.nul"
    printf '%s\0' "$CARGO_BIN" "${cargo_arguments[@]}" > "$case_dir/cargo.argv.nul"
    {
        printf 'cwd=%s\ntest_target=%s\ntest_name=%s\n' "$ROOT_DIR" "$target" "$test_name"
        printf 'LLVM_SYS_181_PREFIX=%q\nCARGO_TARGET_DIR=%q\nCARGO_BUILD_JOBS=1\n' \
            "$LLVM_SYS_181_PREFIX" "$TARGET_DIR"
        cat "$ARTIFACT_DIR/environment.txt"
    } > "$case_dir/command.environment.txt"
    if (cd -- "$ROOT_DIR" && "${invocation[@]}") \
        > "$case_dir/stdout.bin" 2> "$case_dir/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    cat "$case_dir/stdout.bin" "$case_dir/stderr.bin" > "$case_dir/combined-for-validation.log"
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$case_dir/process.status"
    if [[ "$status" -ne 0 ]]; then
        add_validation_error "$case_name command exited $status"
    elif ! grep -Fq "test $test_name ..." "$case_dir/combined-for-validation.log" \
        || ! grep -Fq 'test result: ok. 1 passed; 0 failed;' "$case_dir/combined-for-validation.log"; then
        add_validation_error "$case_name did not report its exact passing test $test_name"
    fi
    printf '%s: exit=%s test=%s\n' "$case_name" "$status" "$test_name"
}

run_audit_case() {
    local case_name=production-owner-audit case_dir="$ARTIFACT_DIR/production-owner-audit" status=0
    local -a invocation=("$BASH_BIN" "$ROOT_DIR/scripts/audit-mir-legacy-owners.sh")
    mkdir -- "$case_dir"
    {
        printf 'cd %q &&' "$ROOT_DIR"
        printf ' %q' "${invocation[@]}"
        printf '\n'
    } > "$case_dir/command.txt"
    printf '%s\0' "${invocation[@]}" > "$case_dir/command.argv.nul"
    {
        printf 'cwd=%s\n' "$ROOT_DIR"
        cat "$ARTIFACT_DIR/environment.txt"
    } > "$case_dir/command.environment.txt"
    if (cd -- "$ROOT_DIR" && "${invocation[@]}") \
        > "$case_dir/stdout.bin" 2> "$case_dir/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    cat "$case_dir/stdout.bin" "$case_dir/stderr.bin" > "$case_dir/combined-for-validation.log"
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$case_dir/process.status"
    if [[ "$status" -ne 0 ]]; then
        add_validation_error "production owner audit exited $status"
    elif ! grep -Fq 'values_crosscall_string_caller_route_binding=V1-index0+V2-index1->' \
        "$case_dir/combined-for-validation.log" \
        || ! grep -Fq 'route_boundary_audit_status=passed' "$case_dir/combined-for-validation.log"; then
        add_validation_error 'production owner audit omitted the V1/V2 binding or green audit marker'
    fi
    printf '%s: exit=%s\n' "$case_name" "$status"
}

run_audit_case

# The single focused lib filter spans Checker admission, MIR lowering/receipt,
# bytecode route binding, and the named Index1 verifier admission/tamper tests.
run_cargo_case library-index1-checker-mir-bytecode-and-verifier crosscall \
    values_crosscall_index1_admission_rejects_unbound_set_temporaries \
    values_crosscall_index1_source_rejects_unbound_set_temporaries \
    values_crosscall_index1_binds_exact_set_temporary_drops \
    values_crosscall_index1_profile_scopes_the_erased_list_catalog_alias \
    ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_has_its_own_route \
    managed_any_values_crosscall_string_index1_seals_only_the_sorted_two_entry_profile \
    values_crosscall_string_caller_index1_native_entry_is_exact_and_versioned \
    values_crosscall_string_caller_index1_native_entry_rejects_unselected_set_cleanup_tampering_before_emission \
    v2_checker_route_admits_sorted_entries_index_one_and_three_roots \
    v2_verifier_rejects_v1_profile_wrong_index_and_missing_entry_drop \
    v2_public_verifier_entries_reject_unselected_set_cleanup_tampering_before_proof_output

run_cargo_case library-runtime-values-ownership v2_values \
    v2_values_is_sorted_mixed_descriptor_snapshot_with_independent_clone_and_element_owners \
    v2_values_failures_are_atomic_and_staged_payloads_are_dropped_once
run_cargo_case library-runtime-cloned-string-root-ownership cloned_string_roots_and_get_snapshots_survive_overwrite_remove_and_both_drop_orders \
    cloned_string_roots_and_get_snapshots_survive_overwrite_remove_and_both_drop_orders
run_cargo_case library-runtime-shared-string-payload-ownership owned_string_payload_survives_shallow_clone_until_last_map_owner \
    owned_string_payload_survives_shallow_clone_until_last_map_owner

run_exact_integration_case reference-bytecode-same-checker-mir-receipt \
    "$REFERENCE_TARGET" "$REFERENCE_TEST"
run_exact_integration_case cli-positive-index1-route "$CLI_TARGET" "$CLI_POSITIVE_TEST"
run_exact_integration_case cli-near-misses-fail-closed "$CLI_TARGET" "$CLI_NEAR_MISS_TEST"

write_source_manifest "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256"
write_fixture_snapshot "$ARTIFACT_DIR/fixture.after.sha256"
write_vcs_snapshot "$ARTIFACT_DIR/vcs.after.txt"
write_git_status "$ARTIFACT_DIR/git-status.after.txt"

if ! cmp -s "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256" \
    "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256"; then
    add_validation_error 'source input snapshot changed during evidence capture'
    diff -u "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256" \
        "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256" \
        > "$ARTIFACT_DIR/source-snapshot.diff" || true
fi
if ! cmp -s "$ARTIFACT_DIR/fixture.before.sha256" "$ARTIFACT_DIR/fixture.after.sha256"; then
    add_validation_error 'Index1 fixture snapshot changed during evidence capture'
fi
if ! cmp -s "$ARTIFACT_DIR/vcs.before.txt" "$ARTIFACT_DIR/vcs.after.txt"; then
    add_validation_error 'branch, HEAD, or worktree identity changed during evidence capture'
fi
if [[ "$TOOLCHAIN_STATUS" -ne 0 ]]; then
    add_validation_error 'one or more toolchain version captures failed'
fi

VISIBILITY_STATUS=0
VISIBILITY_LOG="$ARTIFACT_DIR/visibility-check.txt"
: > "$VISIBILITY_LOG"
check_evidence_visible() {
    local path="$1" status=0
    if git check-ignore -v -- "$path" >> "$VISIBILITY_LOG" 2>&1; then
        printf 'ignored=%s\n' "$path" >> "$VISIBILITY_LOG"
        add_validation_error "evidence file is ignored by Git: $path"
        VISIBILITY_STATUS=1
    else
        status=$?
        if [[ "$status" -eq 1 ]]; then
            printf 'visible_to_git=%s\n' "$path" >> "$VISIBILITY_LOG"
        else
            printf 'check_ignore_error=%s exit=%s\n' "$path" "$status" >> "$VISIBILITY_LOG"
            add_validation_error "git check-ignore failed for evidence file $path (exit $status)"
            VISIBILITY_STATUS=1
        fi
    fi
}

mapfile -d '' -t EVIDENCE_FILES < <(
    find "$ARTIFACT_DIR" -type f ! -name visibility-check.txt \
        ! -name generated-program-ignore-check.txt -print0
)
for evidence_file in "${EVIDENCE_FILES[@]}"; do
    [[ "${evidence_file##*/}" == program ]] && continue
    check_evidence_visible "${evidence_file#"$ROOT_DIR"/}"
done
check_evidence_visible "${ARTIFACT_DIR#"$ROOT_DIR"/}/artifact-validation.txt"
check_evidence_visible "${ARTIFACT_DIR#"$ROOT_DIR"/}/artifacts.sha256"
check_evidence_visible "${ARTIFACT_DIR#"$ROOT_DIR"/}/artifacts.sha256.check"
check_evidence_visible "${ARTIFACT_DIR#"$ROOT_DIR"/}/artifacts.sha256.sha256"
visibility_relative="${VISIBILITY_LOG#"$ROOT_DIR"/}"
if git check-ignore --quiet -- "$visibility_relative"; then
    add_validation_error "evidence visibility report is ignored by Git: $visibility_relative"
    VISIBILITY_STATUS=1
else
    status=$?
    if [[ "$status" -ne 1 ]]; then
        add_validation_error "git check-ignore failed for visibility report (exit $status)"
        VISIBILITY_STATUS=1
    else
        printf 'visible_to_git=%s\n' "$visibility_relative" >> "$VISIBILITY_LOG"
    fi
fi

PROGRAM_IGNORE_STATUS=0
PROGRAM_COUNT=0
while IFS= read -r -d '' program; do
    PROGRAM_COUNT=$((PROGRAM_COUNT + 1))
    relative_program="${program#"$ROOT_DIR"/}"
    if git check-ignore -v -- "$relative_program" \
        >> "$ARTIFACT_DIR/generated-program-ignore-check.txt" 2>&1; then
        :
    else
        status=$?
        printf 'program_not_ignored=%s\ncheck_ignore_exit=%s\n' "$relative_program" "$status" \
            >> "$ARTIFACT_DIR/generated-program-ignore-check.txt"
        PROGRAM_IGNORE_STATUS=1
    fi
done < <(find "$ARTIFACT_DIR" -type f -name program -print0)
if [[ "$PROGRAM_COUNT" -eq 0 ]]; then
    printf 'generated_program_count=0\nignored_if_present=true\n' \
        > "$ARTIFACT_DIR/generated-program-ignore-check.txt"
fi
check_evidence_visible "${ARTIFACT_DIR#"$ROOT_DIR"/}/generated-program-ignore-check.txt"
if [[ "$PROGRAM_IGNORE_STATUS" -ne 0 ]]; then
    add_validation_error 'one or more generated evidence/**/program files are not ignored'
fi

if [[ "${#validation_errors[@]}" -eq 0 ]]; then
    validation=PASS
else
    validation=FAIL
fi
{
    printf 'validation=%s\nprofile=ValuesCrosscallStringCallerIndex1\nlevel=L1/L2\n' "$validation"
    printf 'scope=bounded-index1-checker-mir-bytecode-verifier-reference-cli-and-runtime-ownership-evidence\n'
    printf 'global_map_any_v2_production_ready=false\nproduction_readiness_claim=none\n'
    printf 'target_dir=%s\ntarget_dir_external=true\n' "$TARGET_DIR"
    printf 'source_snapshot_stable=%s\nfixture_snapshot_stable=%s\nvcs_identity_stable=%s\n' \
        "$([[ -f "$ARTIFACT_DIR/source-snapshot.diff" ]] && printf false || printf true)" \
        "$(cmp -s "$ARTIFACT_DIR/fixture.before.sha256" "$ARTIFACT_DIR/fixture.after.sha256" && printf true || printf false)" \
        "$(cmp -s "$ARTIFACT_DIR/vcs.before.txt" "$ARTIFACT_DIR/vcs.after.txt" && printf true || printf false)"
    printf 'production_owner_audit=production-owner-audit\n'
    printf 'evidence_files_visible=%s\n' "$([[ "$VISIBILITY_STATUS" -eq 0 ]] && printf true || printf false)"
    printf 'library_filter=crosscall\nreference_test=%s\ncli_positive_test=%s\ncli_near_miss_test=%s\n' \
        "$REFERENCE_TEST" "$CLI_POSITIVE_TEST" "$CLI_NEAR_MISS_TEST"
    printf 'generated_program_count=%s\n' "$PROGRAM_COUNT"
    for error in "${validation_errors[@]}"; do
        printf 'reason=%s\n' "$error"
    done
} > "$ARTIFACT_DIR/artifact-validation.txt"

# Keep the human-readable logs and manifests visible; program binaries emitted
# under evidence are intentionally ignored by the repository's narrow rule.
(
    cd -- "$ARTIFACT_DIR"
    find . -type f ! -name artifacts.sha256 ! -name artifacts.sha256.check \
        ! -name artifacts.sha256.sha256 -print0 \
        | LC_ALL=C sort -z | xargs -0 -r sha256sum > artifacts.sha256
    sha256sum --check artifacts.sha256 > artifacts.sha256.check 2>&1
    sha256sum artifacts.sha256 artifacts.sha256.check > artifacts.sha256.sha256
    sha256sum --check artifacts.sha256.sha256
)

printf 'artifact_dir=%s\n' "$ARTIFACT_DIR"
if [[ "$validation" != PASS ]]; then
    cat "$ARTIFACT_DIR/artifact-validation.txt" >&2
    exit 1
fi
cat "$ARTIFACT_DIR/artifact-validation.txt"
