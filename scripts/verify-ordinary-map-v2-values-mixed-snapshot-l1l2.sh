#!/usr/bin/env bash
set -euo pipefail
set -o pipefail
set -o noclobber

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
FIXTURE=tests/fixtures/mir_ordinary_map_any_owner_v2_values_mixed_snapshot.mimi
CLI_TEST_SOURCE=tests/canonical_mir_ordinary_map_any_owner_v2_cli.rs
CLI_TEST_NAME=map_any_owner_v2_exact_values_mixed_snapshot_uses_its_production_profile
REFERENCE_TEST_SOURCE=tests/canonical_mir_ordinary_map_any_v2_values_mixed_snapshot_reference.rs
REFERENCE_TEST_NAME=values_mixed_snapshot_materializes_its_receipt_and_executes_in_reference_mir
ARTIFACT_ENV=MIMI_MAP_V2_VALUES_MIXED_SNAPSHOT_L1L2_ARTIFACT_DIR

if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper
fi

for tool in cargo rustc sha256sum realpath git mktemp tee; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'ValuesMixedSnapshot L1/L2 capture requires %s\n' "$tool" >&2
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
    requested="${TMPDIR:-/tmp}/mimi-map-v2-values-mixed-snapshot-l1l2-$(date -u +%Y%m%dT%H%M%SZ)-$$"
fi
case "$requested" in
    /*) ;;
    *) requested="$ROOT_DIR/$requested" ;;
esac
if [ -e "$requested" ] || [ -L "$requested" ]; then
    printf 'refusing to overwrite existing ValuesMixedSnapshot L1/L2 artifact path: %s\n' "$requested" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/scripts/"*|"$ROOT_DIR/target/"*)
        printf 'ValuesMixedSnapshot L1/L2 artifacts must be outside source/build trees: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
unset "$ARTIFACT_ENV"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/mimi-map-v2-values-mixed-snapshot-l1l2.XXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT

source_input_paths() {
    for source_dir in src std; do
        if [ -d "$source_dir" ]; then
            find "$source_dir" -type f -print0 || printf '%s\0' __SOURCE_ENUMERATION_FAILED__
        else
            printf '%s\0' "__MISSING_SOURCE_DIRECTORY__/$source_dir"
        fi
    done
    printf '%s\0' "$CLI_TEST_SOURCE" "$REFERENCE_TEST_SOURCE" "$FIXTURE" \
        scripts/verify-ordinary-map-v2-values-mixed-snapshot-l1l2.sh
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

write_fixture_hash() {
    local destination="$1"
    (
        cd "$ROOT_DIR"
        if [ -f "$FIXTURE" ]; then
            sha256sum -- "$FIXTURE" || printf 'UNREADABLE\t%s\n' "$FIXTURE"
        else
            printf 'MISSING\t%s\n' "$FIXTURE"
        fi
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
snapshot_before="$(sha256sum "$artifact_dir/compilation-inputs.before.manifest.sha256" | awk '{print $1}')"
printf '%s\n' "$snapshot_before" > "$artifact_dir/compilation-inputs.before.sha256"
sha256sum -- "$FIXTURE" > "$artifact_dir/fixture.before.sha256"

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
    for variable in PATH RUSTC RUSTUP_TOOLCHAIN RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_HOME CARGO_TARGET_DIR LIBCLANG_PATH CC CXX CFLAGS LDFLAGS MIMI_ROUTE_CENSUS MIMI_VERBOSE; do
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
        printf 'cd %q && env LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 %q' \
            "$ROOT_DIR" "$LLVM_SYS_181_PREFIX" "$CARGO_BIN"
        printf ' %q' "${cargo_arguments[@]}"
        printf '\n'
    } > "$case_dir/command.txt"
    printf '%s\0' "$CARGO_BIN" "${cargo_arguments[@]}" > "$case_dir/command.argv.nul"
    {
        printf 'cwd=%s\nLLVM_SYS_181_PREFIX=%s\nCARGO_BUILD_JOBS=1\n' \
            "$ROOT_DIR" "$LLVM_SYS_181_PREFIX"
        cat "$artifact_dir/toolchain/environment.txt"
    } > "$case_dir/command.environment.txt"
    if (
        cd "$ROOT_DIR"
        env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" CARGO_BUILD_JOBS=1 \
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

run_cargo_case library \
    test --lib mixed_snapshot -- --nocapture --test-threads=1
run_cargo_case reference \
    test --test canonical_mir_ordinary_map_any_v2_values_mixed_snapshot_reference \
    "$REFERENCE_TEST_NAME" -- --exact --nocapture --test-threads=1
run_cargo_case cli-exact \
    test --test canonical_mir_ordinary_map_any_owner_v2_cli \
    "$CLI_TEST_NAME" -- --exact --nocapture --test-threads=1

write_source_manifest "$artifact_dir/compilation-inputs.after.manifest.sha256"
snapshot_after="$(sha256sum "$artifact_dir/compilation-inputs.after.manifest.sha256" | awk '{print $1}')"
printf '%s\n' "$snapshot_after" > "$artifact_dir/compilation-inputs.after.sha256"
sha256sum -- "$FIXTURE" > "$artifact_dir/fixture.after.sha256"
write_vcs_identity "$artifact_dir/vcs.after.txt"
git status --short --untracked-files=no > "$artifact_dir/git-status.after.txt"

validation_errors=()
if ! cmp -s \
    "$artifact_dir/compilation-inputs.before.manifest.sha256" \
    "$artifact_dir/compilation-inputs.after.manifest.sha256"; then
    validation_errors+=("compilation inputs changed during capture")
fi
if ! cmp -s "$artifact_dir/fixture.before.sha256" "$artifact_dir/fixture.after.sha256"; then
    validation_errors+=("ValuesMixedSnapshot fixture changed during capture")
fi
if ! cmp -s "$artifact_dir/vcs.before.txt" "$artifact_dir/vcs.after.txt"; then
    validation_errors+=("branch or HEAD changed during capture")
fi
if [ "$toolchain_status" -ne 0 ]; then
    validation_errors+=("a toolchain version capture failed")
fi

if [ "$(cat "$artifact_dir/library/command.exit")" -ne 0 ]; then
    validation_errors+=("cargo test --lib mixed_snapshot failed")
elif ! grep -Eq '^running [1-9][0-9]* tests?$' "$artifact_dir/library/command.log" || \
    ! grep -Fq 'test result: ok.' "$artifact_dir/library/command.log"; then
    validation_errors+=("library filter did not produce a positive passing test summary")
fi
if [ "$(cat "$artifact_dir/reference/command.exit")" -ne 0 ]; then
    validation_errors+=("exact ValuesMixedSnapshot Reference MIR test failed")
elif ! grep -Fq "test $REFERENCE_TEST_NAME ... ok" "$artifact_dir/reference/command.log"; then
    validation_errors+=("exact ValuesMixedSnapshot Reference MIR test did not report a passing test")
fi
if [ "$(cat "$artifact_dir/cli-exact/command.exit")" -ne 0 ]; then
    validation_errors+=("exact ValuesMixedSnapshot CLI test failed")
elif ! grep -Fq "test $CLI_TEST_NAME ... ok" "$artifact_dir/cli-exact/command.log"; then
    validation_errors+=("exact ValuesMixedSnapshot CLI test did not report a passing test")
fi

if [ "${#validation_errors[@]}" -eq 0 ]; then
    printf 'validation=PASS\nprofile=ValuesMixedSnapshot\nlevel=L1/L2\nglobal_map_any_v2_production_ready=false\nreference_test=%s\n' \
        "$REFERENCE_TEST_NAME" \
        > "$artifact_dir/artifact-validation.txt"
else
    {
        printf 'validation=FAIL\nprofile=ValuesMixedSnapshot\nlevel=L1/L2\nglobal_map_any_v2_production_ready=false\nreference_test=%s\n' \
            "$REFERENCE_TEST_NAME"
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
