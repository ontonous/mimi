#!/usr/bin/env bash
set -Eeuo pipefail
set -o noclobber

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd -- "$ROOT_DIR"
RUNNER=scripts/verify-ordinary-map-v2-removed-key-checked-miss.sh
PROFILE=map-any-owner-v2-removed-key-checked-miss-v1
REFERENCE_TARGET=canonical_mir_ordinary_map_any_v2_removed_key_checked_miss_reference
CLI_TARGET=canonical_mir_ordinary_map_any_owner_v2_removed_key_checked_miss_cli
L3_TARGET=canonical_mir_ordinary_map_any_v2_removed_key_checked_miss_memcheck_l3
L3_TEST=ordinary_map_owned_any_v2_removed_key_checked_miss_native_builds_are_strict_memcheck_clean
ARTIFACT_ENV=MIMI_MAP_V2_REMOVED_KEY_CHECKED_MISS_RUN_DIR

for tool in bash cargo rustc sha256sum realpath git find sort cmp wc date env awk; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'RemovedKeyCheckedMiss reproducibility runner requires %s\n' "$tool" >&2
        exit 1
    }
done

if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper
fi
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
    printf 'llvm-config is unavailable for LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi
command -v valgrind >/dev/null 2>&1 || {
    printf 'RemovedKeyCheckedMiss strict L3 requires valgrind\n' >&2
    exit 1
}
VALGRIND_BIN="$(realpath -- "$(command -v valgrind)")"

if [[ -v "$ARTIFACT_ENV" && -n "${!ARTIFACT_ENV}" ]]; then
    requested="${!ARTIFACT_ENV}"
else
    requested="${TMPDIR:-/tmp}/mimi-map-v2-removed-key-checked-miss-$(date -u +%Y%m%dT%H%M%SZ)-$$"
fi
case "$requested" in /*) ;; *) requested="$ROOT_DIR/$requested" ;; esac
if [[ -e "$requested" || -L "$requested" ]]; then
    printf 'refusing to overwrite existing RemovedKeyCheckedMiss artifact path: %s\n' "$requested" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/scripts/"*|"$ROOT_DIR/target/"*|"$ROOT_DIR/.git/"*)
        printf 'RemovedKeyCheckedMiss artifacts must be outside source/build/git trees: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname -- "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"

mkdir -- "$artifact_dir/l1l2" "$artifact_dir/l3" "$artifact_dir/toolchain"
printf 'cwd=%s\nrunner=%s\nprofile=%s\ncommand=LLVM_SYS_181_PREFIX=%q %s=%q bash %q\n' \
    "$ROOT_DIR" "$RUNNER" "$PROFILE" "$LLVM_SYS_181_PREFIX" "$ARTIFACT_ENV" "$artifact_dir" "$RUNNER" \
    > "$artifact_dir/runner-invocation.txt"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/mimi-removed-key-miss-runner.XXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT
failed=0
source_tree_identical=false
manifest_digests_verified=false
steps_completed=0
step_failures=0
validation_finalized=false

source_snapshot_manifest() {
    local destination="$1"
    local paths="$scratch/source.paths.nul" sorted="$scratch/source.sorted.paths.nul"
    local path
    : >| "$paths"
    : >| "$destination"
    for source_dir in src tests std scripts; do
        [[ -d "$source_dir" ]] || {
            printf 'required source directory is missing: %s\n' "$source_dir" >&2
            return 1
        }
        find "$source_dir" -type f -print0 >> "$paths"
    done
    for path in Cargo.toml Cargo.lock build.rs rust-toolchain rust-toolchain.toml \
        rustfmt.toml .gitignore .cargo/config .cargo/config.toml "$RUNNER" AGENTS.md; do
        if [[ -f "$path" ]]; then printf '%s\0' "$path" >> "$paths"; fi
    done
    sort -zu "$paths" -o "$sorted"
    while IFS= read -r -d '' path; do
        [[ -f "$path" ]] || {
            printf 'source disappeared before hashing: %s\n' "$path" >&2
            return 1
        }
        sha256sum -- "$path" >> "$destination"
    done < "$sorted"
}

record_vcs() {
    local destination="$1"
    {
        printf 'branch='; git branch --show-current
        printf 'head='; git rev-parse HEAD
        printf 'worktree_status_begin\n'
        git status --short --untracked-files=normal
        printf 'worktree_status_end\n'
    } > "$destination"
}

record_tool() {
    local label="$1" executable="$2"; shift 2
    local directory="$artifact_dir/toolchain/$label" status=0
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
    fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" > "$directory/process.status"
    [[ "$status" -eq 0 ]]
}

write_command() {
    local directory="$1"; shift
    {
        printf 'cd %q &&' "$ROOT_DIR"
        printf ' %q' "$@"
        printf '\n'
    } > "$directory/command.txt"
    printf '%s\0' "$@" > "$directory/command.argv.nul"
    {
        printf 'cwd=%s\nLLVM_SYS_181_PREFIX=%s\nCARGO_BUILD_JOBS=1\nCARGO_TARGET_DIR=%s/target\n' \
            "$ROOT_DIR" "$LLVM_SYS_181_PREFIX" "$ROOT_DIR"
        for variable in PATH RUSTC RUSTUP_TOOLCHAIN RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_HOME; do
            if [[ -v "$variable" ]]; then
                printf '%s=%q\n' "$variable" "${!variable}"
            else
                printf '%s=<unset>\n' "$variable"
            fi
        done
    } > "$directory/command.environment.txt"
}

run_step() {
    local name="$1"; shift
    local directory="$artifact_dir/$name" status=0
    mkdir -- "$directory"
    write_command "$directory" "$@"
    if (cd "$ROOT_DIR" && "$@") > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    {
        printf '===== stdout =====\n'; cat "$directory/stdout.bin"
        printf '\n===== stderr =====\n'; cat "$directory/stderr.bin"
    } > "$directory/command.log"
    printf '%s\n' "$status" > "$directory/command.exit"
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" > "$directory/process.status"
    steps_completed=$((steps_completed + 1))
    if [[ "$status" -ne 0 ]]; then
        failed=1
        step_failures=$((step_failures + 1))
        printf 'step_failed=%s exit_code=%s\n' "$name" "$status" >> "$artifact_dir/failures.txt"
    fi
    cat "$directory/command.log"
}

write_validation_report() {
    local validation=FAIL
    if [[ "$failed" -eq 0 && "$source_tree_identical" == true && "$manifest_digests_verified" == true ]]; then
        validation=PASS
    fi
    {
        printf 'validation=%s\nprofile=RemovedKeyCheckedMiss\nlevel=L1/L2/L3\n' "$validation"
        printf 'route_profile=%s\nglobal_map_any_v2_production_ready=false\n' "$PROFILE"
        printf 'source_tree_identical=%s\nmanifest_digests_verified=%s\n' \
            "$source_tree_identical" "$manifest_digests_verified"
        printf 'l1_reference_target=%s\nl1_cli_target=%s\nl3_memcheck_target=%s\n' \
            "$REFERENCE_TARGET" "$CLI_TARGET" "$L3_TARGET"
        printf 'l1_receipt_unit_filter=core::mir::receipt::tests::removed_key_\nl1_bytecode_unit_filter=interp::bytecode::mir::tests::ordinary_map_owned_any_v2_removed_key_checked_miss_\n'
        printf 'l3_test=%s\nrunner_output=%s\nsteps_completed=%s\nstep_failures=%s\n' \
            "$L3_TEST" "$artifact_dir" "$steps_completed" "$step_failures"
        printf 'positive_assertions=reference+bytecode+default/explicit-run-build-verify+default/explicit-native+three-root-drops+pair-result-drops\n'
        printf 'negative_assertions=pre-remove-root-hit+pre-remove-root-unpack-provenance+wrong-remove-key+wrong-get-key+formula-equivalence+found-only+payload-only+wrong-zero+hit-nonzero+extra-map-op+root-alias+missing-duplicate-wrong-root-drop+malformed-receipt+wrong-owner-key-type-receipt+cross-profile-receipt\n'
        printf 'failure_manifest=%s\n' "$([[ -f "$artifact_dir/failures.txt" ]] && printf present || printf absent)"
    } > "$artifact_dir/run-validation.txt"
    validation_finalized=true
}

finalize() {
    local command_status=$?
    trap - EXIT
    if [[ "$validation_finalized" != true ]]; then
        failed=1
        printf 'runner_exited_before_validation_finalization=%s\n' "$command_status" \
            >> "$artifact_dir/failures.txt" || true
        write_validation_report || true
        if [[ "$command_status" -eq 0 ]]; then command_status=1; fi
    fi
    rm -rf -- "$scratch"
    exit "$command_status"
}
trap finalize EXIT

record_vcs "$artifact_dir/vcs.before.txt"
source_snapshot_manifest "$artifact_dir/source-snapshot.before.manifest.sha256"
sha256sum -- "$artifact_dir/source-snapshot.before.manifest.sha256" \
    | awk '{print $1}' > "$artifact_dir/source-snapshot.before.sha256"

tool_status=0
record_tool bash "$(command -v bash)" --version || tool_status=1
record_tool rustc "$(command -v rustc)" --version --verbose || tool_status=1
record_tool cargo "$(command -v cargo)" --version --verbose || tool_status=1
record_tool llvm-config "$LLVM_CONFIG_BIN" --version --prefix --libdir --host-target || tool_status=1
record_tool valgrind "$VALGRIND_BIN" --version || tool_status=1
record_tool sha256sum "$(command -v sha256sum)" --version || tool_status=1
printf 'tool_capture_exit_code=%s\n' "$tool_status" > "$artifact_dir/toolchain/tool-capture.status"
if [[ "$tool_status" -ne 0 ]]; then failed=1; fi

export LLVM_SYS_181_PREFIX
export MIMI_STDLIB="$ROOT_DIR/std"
export CARGO_BUILD_JOBS=1
export CARGO_TARGET_DIR="$ROOT_DIR/target"
export CARGO_TERM_COLOR=never
run_step l1l2/reference env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
    MIMI_STDLIB="$MIMI_STDLIB" CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    CARGO_TERM_COLOR=never cargo test --locked --manifest-path "$ROOT_DIR/Cargo.toml" \
    --test "$REFERENCE_TARGET" -- --nocapture --test-threads=1
run_step l1l2/cli env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
    MIMI_STDLIB="$MIMI_STDLIB" CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    CARGO_TERM_COLOR=never cargo test --locked --manifest-path "$ROOT_DIR/Cargo.toml" \
    --test "$CLI_TARGET" -- --nocapture --test-threads=1
run_step l1l2/receipt-unit env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
    MIMI_STDLIB="$MIMI_STDLIB" CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    CARGO_TERM_COLOR=never cargo test --locked --manifest-path "$ROOT_DIR/Cargo.toml" \
    --lib core::mir::receipt::tests::removed_key_ -- --nocapture --test-threads=1
run_step l1l2/bytecode-unit env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
    MIMI_STDLIB="$MIMI_STDLIB" CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    CARGO_TERM_COLOR=never cargo test --locked --manifest-path "$ROOT_DIR/Cargo.toml" \
    --lib interp::bytecode::mir::tests::ordinary_map_owned_any_v2_removed_key_checked_miss_ \
    -- --nocapture --test-threads=1
run_step l3/native-memcheck env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
    MIMI_STDLIB="$MIMI_STDLIB" CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    CARGO_TERM_COLOR=never MIMI_L3_VALGRIND="$VALGRIND_BIN" \
    MIMI_MAP_V2_REMOVED_KEY_CHECKED_MISS_L3_ARTIFACT_DIR="$artifact_dir/l3/native-evidence" \
    cargo test --locked --manifest-path "$ROOT_DIR/Cargo.toml" --test "$L3_TARGET" \
    -- --ignored "$L3_TEST" --nocapture --test-threads=1

record_vcs "$artifact_dir/vcs.after.txt"
if source_snapshot_manifest "$artifact_dir/source-snapshot.after.manifest.sha256"; then
    sha256sum -- "$artifact_dir/source-snapshot.after.manifest.sha256" \
        | awk '{print $1}' > "$artifact_dir/source-snapshot.after.sha256"
    if cmp -s "$artifact_dir/source-snapshot.before.manifest.sha256" \
        "$artifact_dir/source-snapshot.after.manifest.sha256"; then
        source_tree_identical=true
        manifest_digests_verified=true
    else
        failed=1
        printf 'source_snapshot_changed_during_run=true\n' >> "$artifact_dir/failures.txt"
    fi
else
    failed=1
    printf 'source_snapshot_after_failed=true\n' >> "$artifact_dir/failures.txt"
fi

write_validation_report
if [[ "$failed" -ne 0 ]]; then exit 1; fi
