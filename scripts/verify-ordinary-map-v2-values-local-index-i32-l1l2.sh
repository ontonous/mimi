#!/usr/bin/env bash
set -euo pipefail
set -o pipefail
set -o noclobber

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
FIXTURE=tests/fixtures/mir_ordinary_map_any_owner_v2_values_local_index_i32_result_branch.mimi
STALE_FIXTURE=tests/fixtures/mir_ordinary_map_any_owner_v2_values_local_index_i32.mimi
CLI_TEST_SOURCE=tests/canonical_mir_ordinary_map_any_v2_values_local_index_i32_cli.rs
CLI_TEST_TARGET=canonical_mir_ordinary_map_any_v2_values_local_index_i32_cli
CLI_POSITIVE_TEST=values_local_index_i32_result_branch_keeps_its_own_default_and_explicit_route
CLI_NEGATIVE_TEST=values_local_index_i32_near_misses_reject_all_default_and_explicit_consumers
CLI_STALE_TEST=stale_straightline_values_fixture_is_a_source_diagnostic_on_both_routes
ARTIFACT_ENV=MIMI_VALUES_LOCAL_INDEX_I32_L1L2_ARTIFACT_DIR

if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper
fi
RUST_MIN_STACK=67108864

for tool in cargo rustc sha256sum realpath git mktemp tee; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'ValuesLocalIndexI32 L1/L2 capture requires %s\n' "$tool" >&2
        exit 1
    }
done
[ -d "$LLVM_SYS_181_PREFIX" ] || {
    printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
}

CARGO_BIN="$(command -v cargo)"
RUSTC_BIN="$(command -v rustc)"
if [ -x "$LLVM_SYS_181_PREFIX/bin/llvm-config" ]; then
    LLVM_CONFIG_BIN="$LLVM_SYS_181_PREFIX/bin/llvm-config"
elif [ -x "$LLVM_SYS_181_PREFIX/llvm-config" ]; then
    LLVM_CONFIG_BIN="$LLVM_SYS_181_PREFIX/llvm-config"
elif command -v llvm-config >/dev/null 2>&1; then
    LLVM_CONFIG_BIN="$(command -v llvm-config)"
else
    printf 'llvm-config is unavailable under LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi

if [[ -v "$ARTIFACT_ENV" && -n "${!ARTIFACT_ENV}" ]]; then
    requested="${!ARTIFACT_ENV}"
else
    requested="${TMPDIR:-/tmp}/mimi-values-local-index-i32-l1l2-$(date -u +%Y%m%dT%H%M%SZ)-$$"
fi
case "$requested" in
    /*) ;;
    *) requested="$ROOT_DIR/$requested" ;;
esac
if [ -e "$requested" ] || [ -L "$requested" ]; then
    printf 'refusing to overwrite existing ValuesLocalIndexI32 L1/L2 artifact path: %s\n' "$requested" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/scripts/"*|"$ROOT_DIR/target/"*)
        printf 'ValuesLocalIndexI32 L1/L2 artifacts must be outside source/build trees: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
unset "$ARTIFACT_ENV"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/mimi-values-local-index-i32-l1l2.XXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT

source_input_paths() {
    for source_dir in src std; do
        if [ -d "$source_dir" ]; then
            find "$source_dir" -type f -print0 || printf '%s\0' __SOURCE_ENUMERATION_FAILED__
        else
            printf '%s\0' "__MISSING_SOURCE_DIRECTORY__/$source_dir"
        fi
    done
    printf '%s\0' "$CLI_TEST_SOURCE" "$FIXTURE" "$STALE_FIXTURE" \
        scripts/verify-ordinary-map-v2-values-local-index-i32-l1l2.sh
    for input in Cargo.toml Cargo.lock build.rs rust-toolchain rust-toolchain.toml .cargo/config .cargo/config.toml; do
        [ ! -f "$input" ] || printf '%s\0' "$input"
    done
}

write_source_manifest() {
    local destination="$1"
    (
        cd "$ROOT_DIR"
        source_input_paths | sort -zu | while IFS= read -r -d '' input; do
            if [ ! -f "$input" ]; then
                printf 'MISSING\t%s\n' "$input"
                continue
            fi
            sha256sum -- "$input" || printf 'UNREADABLE\t%s\n' "$input"
        done
    ) > "$destination"
}

write_vcs_identity() {
    local destination="$1"
    (
        cd "$ROOT_DIR"
        printf 'branch='
        git branch --show-current
        printf 'head='
        git rev-parse HEAD
    ) > "$destination"
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
    if "$executable" "$@" > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
        status=0
    else
        status=$?
        toolchain_status=1
    fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$directory/process.status"
}

cd "$ROOT_DIR"
write_vcs_identity "$artifact_dir/vcs.before.txt"
git status --short --untracked-files=no > "$artifact_dir/git-status.before.txt"
write_source_manifest "$artifact_dir/compilation-inputs.before.manifest.sha256"
sha256sum "$artifact_dir/compilation-inputs.before.manifest.sha256" \
    | awk '{print $1}' > "$artifact_dir/compilation-inputs.before.sha256"
sha256sum -- "$FIXTURE" > "$artifact_dir/fixture.before.sha256"
sha256sum -- "$STALE_FIXTURE" > "$artifact_dir/stale-fixture.before.sha256"

mkdir -- "$artifact_dir/toolchain"
toolchain_status=0
capture_tool rustc "$RUSTC_BIN" --version --verbose
capture_tool cargo "$CARGO_BIN" --version --verbose
capture_tool llvm-config "$LLVM_CONFIG_BIN" --version --prefix --libdir --host-target
{
    printf 'LLVM_SYS_181_PREFIX=%q\n' "$LLVM_SYS_181_PREFIX"
    printf 'CARGO_BUILD_JOBS=1\n'
    printf 'rustc_path=%q\n' "$RUSTC_BIN"
    printf 'cargo_path=%q\n' "$CARGO_BIN"
    printf 'llvm_config_path=%q\n' "$LLVM_CONFIG_BIN"
    for variable in PATH RUSTC RUSTUP_TOOLCHAIN RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_HOME CARGO_TARGET_DIR LIBCLANG_PATH CC CXX CFLAGS LDFLAGS MIMI_ROUTE_CENSUS MIMI_VERBOSE RUST_MIN_STACK; do
        if [[ -v "$variable" ]]; then
            printf '%s=%q\n' "$variable" "${!variable}"
        else
            printf '%s=<unset>\n' "$variable"
        fi
    done
} > "$artifact_dir/toolchain/environment.txt"

run_cargo_case() {
    local case_name="$1"
    shift
    local case_dir="$artifact_dir/$case_name"
    local status=0
    local -a cargo_arguments=("$@")
    mkdir -- "$case_dir"
    {
        printf 'cd %q && env LLVM_SYS_181_PREFIX=%q RUST_MIN_STACK=%q CARGO_BUILD_JOBS=1 %q' \
            "$ROOT_DIR" "$LLVM_SYS_181_PREFIX" "$RUST_MIN_STACK" "$CARGO_BIN"
        printf ' %q' "${cargo_arguments[@]}"
        printf '\n'
    } > "$case_dir/command.txt"
    printf '%s\0' "$CARGO_BIN" "${cargo_arguments[@]}" > "$case_dir/command.argv.nul"
    {
        printf 'cwd=%s\nLLVM_SYS_181_PREFIX=%s\nRUST_MIN_STACK=%s\nCARGO_BUILD_JOBS=1\n' \
            "$ROOT_DIR" "$LLVM_SYS_181_PREFIX" "$RUST_MIN_STACK"
        cat "$artifact_dir/toolchain/environment.txt"
    } > "$case_dir/command.environment.txt"
    if (
        cd "$ROOT_DIR"
        env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" RUST_MIN_STACK="$RUST_MIN_STACK" CARGO_BUILD_JOBS=1 \
            "$CARGO_BIN" "${cargo_arguments[@]}"
    ) 2>&1 | tee "$case_dir/command.log"; then
        status=0
    else
        status=$?
    fi
    printf '%s\n' "$status" > "$case_dir/command.exit"
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$case_dir/process.status"
}

run_cargo_case library-l1 \
    test --lib values_local_index_i32 -- --nocapture --test-threads=1
run_cargo_case cli-positive-l2 \
    test --test "$CLI_TEST_TARGET" "$CLI_POSITIVE_TEST" -- --exact --nocapture --test-threads=1
run_cargo_case cli-near-misses-l2 \
    test --test "$CLI_TEST_TARGET" "$CLI_NEGATIVE_TEST" -- --exact --nocapture --test-threads=1
run_cargo_case cli-stale-fixture-source-diagnostic-l2 \
    test --test "$CLI_TEST_TARGET" "$CLI_STALE_TEST" -- --exact --nocapture --test-threads=1

write_source_manifest "$artifact_dir/compilation-inputs.after.manifest.sha256"
sha256sum "$artifact_dir/compilation-inputs.after.manifest.sha256" \
    | awk '{print $1}' > "$artifact_dir/compilation-inputs.after.sha256"
sha256sum -- "$FIXTURE" > "$artifact_dir/fixture.after.sha256"
sha256sum -- "$STALE_FIXTURE" > "$artifact_dir/stale-fixture.after.sha256"
write_vcs_identity "$artifact_dir/vcs.after.txt"
git status --short --untracked-files=no > "$artifact_dir/git-status.after.txt"

validation_errors=()
if ! cmp -s \
    "$artifact_dir/compilation-inputs.before.manifest.sha256" \
    "$artifact_dir/compilation-inputs.after.manifest.sha256"; then
    validation_errors+=("compilation inputs changed during capture")
fi
if ! cmp -s "$artifact_dir/fixture.before.sha256" "$artifact_dir/fixture.after.sha256"; then
    validation_errors+=("ValuesLocalIndexI32 Result-branch fixture changed during capture")
fi
if ! cmp -s "$artifact_dir/stale-fixture.before.sha256" "$artifact_dir/stale-fixture.after.sha256"; then
    validation_errors+=("retired straightline source-diagnostic fixture changed during capture")
fi
if ! cmp -s "$artifact_dir/vcs.before.txt" "$artifact_dir/vcs.after.txt"; then
    validation_errors+=("branch or HEAD changed during capture")
fi
if [ "$toolchain_status" -ne 0 ]; then
    validation_errors+=("a toolchain version capture failed")
fi

if [ "$(cat "$artifact_dir/library-l1/command.exit")" -ne 0 ]; then
    validation_errors+=("ValuesLocalIndexI32 library L1 filter failed")
elif ! grep -Eq '^running [1-9][0-9]* tests?$' "$artifact_dir/library-l1/command.log" || \
    ! grep -Fq 'test result: ok.' "$artifact_dir/library-l1/command.log"; then
    validation_errors+=("library L1 filter did not produce a positive passing test summary")
fi
if [ "$(cat "$artifact_dir/cli-positive-l2/command.exit")" -ne 0 ]; then
    validation_errors+=("exact ValuesLocalIndexI32 positive CLI L2 test failed")
elif ! grep -Fq "test $CLI_POSITIVE_TEST ... ok" "$artifact_dir/cli-positive-l2/command.log"; then
    validation_errors+=("exact ValuesLocalIndexI32 positive CLI test did not report a passing test")
fi
if [ "$(cat "$artifact_dir/cli-near-misses-l2/command.exit")" -ne 0 ]; then
    validation_errors+=("exact ValuesLocalIndexI32 near-miss CLI L2 test failed")
elif ! grep -Fq "test $CLI_NEGATIVE_TEST ... ok" "$artifact_dir/cli-near-misses-l2/command.log"; then
    validation_errors+=("exact ValuesLocalIndexI32 near-miss CLI test did not report a passing test")
fi
if [ "$(cat "$artifact_dir/cli-stale-fixture-source-diagnostic-l2/command.exit")" -ne 0 ]; then
    validation_errors+=("stale straightline source-diagnostic CLI test failed")
elif ! grep -Fq "test $CLI_STALE_TEST ... ok" "$artifact_dir/cli-stale-fixture-source-diagnostic-l2/command.log"; then
    validation_errors+=("stale straightline source-diagnostic test did not report a passing test")
fi

if [ "${#validation_errors[@]}" -eq 0 ]; then
    printf 'validation=PASS\nprofile=ValuesLocalIndexI32ResultBranch\nlevel=L1/L2\nglobal_map_any_v2_production_ready=false\npositive_cli_test=%s\nnegative_cli_test=%s\nstale_source_diagnostic_test=%s\n' \
        "$CLI_POSITIVE_TEST" "$CLI_NEGATIVE_TEST" "$CLI_STALE_TEST" \
        > "$artifact_dir/artifact-validation.txt"
else
    {
        printf 'validation=FAIL\nprofile=ValuesLocalIndexI32ResultBranch\nlevel=L1/L2\nglobal_map_any_v2_production_ready=false\npositive_cli_test=%s\nnegative_cli_test=%s\nstale_source_diagnostic_test=%s\n' \
            "$CLI_POSITIVE_TEST" "$CLI_NEGATIVE_TEST" "$CLI_STALE_TEST"
        for error in "${validation_errors[@]}"; do
            printf 'reason=%s\n' "$error"
        done
    } > "$artifact_dir/artifact-validation.txt"
fi

(
    cd "$artifact_dir"
    find . -type f ! -name artifacts.sha256 ! -name artifacts.sha256.sha256 -print0 \
        | sort -z | xargs -0 -r sha256sum > artifacts.sha256
    sha256sum --check artifacts.sha256
    sha256sum artifacts.sha256 > artifacts.sha256.sha256
)

printf 'artifact_dir=%s\n' "$artifact_dir"
if [ "${#validation_errors[@]}" -ne 0 ]; then
    cat "$artifact_dir/artifact-validation.txt" >&2
    exit 1
fi
cat "$artifact_dir/artifact-validation.txt"
