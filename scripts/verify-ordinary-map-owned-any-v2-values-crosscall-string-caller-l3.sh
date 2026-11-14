#!/usr/bin/env bash
set -euo pipefail
set -o noclobber

if [[ $# -gt 1 || ${1:-} == --help ]]; then
    printf 'Usage: %s [current-YYYYMMDD-HHMMSS-pid-random]\n' "$0"
    exit 2
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT_DIR"
RUN_ID="${1:-current-$(date -u +%Y%m%d-%H%M%S)-$$-$RANDOM}"
[[ "$RUN_ID" =~ ^[a-z0-9][a-z0-9-]{0,80}$ ]] || {
    printf 'invalid Values cross-call String caller L3 run id: %s\n' "$RUN_ID" >&2
    exit 2
}
LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
ARTIFACT_ENV=MIMI_VALUES_CROSSCALL_STRING_CALLER_L3_ARTIFACT_DIR
ARTIFACT_DIR_DEFAULT="$ROOT_DIR/evidence/0.1.11-dev/gates/values-crosscall-string-caller-l3-$RUN_ID"
ARTIFACT_DIR_REQUESTED="${MIMI_VALUES_CROSSCALL_STRING_CALLER_L3_EVIDENCE_DIR:-$ARTIFACT_DIR_DEFAULT}"
case "$ARTIFACT_DIR_REQUESTED" in
    /*) ;;
    *) ARTIFACT_DIR_REQUESTED="$ROOT_DIR/$ARTIFACT_DIR_REQUESTED" ;;
esac
if [[ -e "$ARTIFACT_DIR_REQUESTED" || -L "$ARTIFACT_DIR_REQUESTED" ]]; then
    printf 'refusing to reuse Values cross-call String caller L3 evidence path: %s\n' \
        "$ARTIFACT_DIR_REQUESTED" >&2
    exit 2
fi
ARTIFACT_DIR="$(realpath -m -- "$ARTIFACT_DIR_REQUESTED")"
TARGET_DIR_DEFAULT="/media/ontos-hpc/P7000Z2T/mimi-target-cache-values-crosscall-string-caller-l3-$RUN_ID"
TARGET_DIR_REQUESTED="${MIMI_VALUES_CROSSCALL_STRING_CALLER_L3_TARGET_DIR:-$TARGET_DIR_DEFAULT}"
TEST_TARGET=canonical_mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_memcheck_l3
TEST_NAME=ordinary_map_owned_any_v2_values_crosscall_string_caller_default_and_explicit_native_builds_are_strict_memcheck_clean
PROFILE_V1=ordinary-map-owned-any-v2-values-crosscall-string-caller-v1
PROFILE_V2=ordinary-map-owned-any-v2-values-crosscall-string-caller-v2
MIR_RECEIPT_SCHEMA_V1=mimi-ordinary-map-owned-any-v2-values-crosscall-string-caller-v1
MIR_RECEIPT_SCHEMA_V2=mimi-ordinary-map-owned-any-v2-values-crosscall-string-caller-v2
ROUTE_CENSUS_SCHEMA=mimi-mir-route-receipt-v1
if [[ "$PROFILE_V1" == "$PROFILE_V2" || "$MIR_RECEIPT_SCHEMA_V1" == "$MIR_RECEIPT_SCHEMA_V2" ]]; then
    printf 'Values cross-call String caller V1 and Index1 profiles and schemas must remain distinct\n' >&2
    exit 2
fi
PROFILE_SET="$PROFILE_V1,$PROFILE_V2"
FIXTURE_V1="$ROOT_DIR/tests/fixtures/mir_ordinary_map_owned_any_v2_values_crosscall_string_caller.mimi"
FIXTURE_V2="$ROOT_DIR/tests/fixtures/mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1.mimi"
RELEVANT_ENVIRONMENT=(
    PATH HOME LLVM_SYS_181_PREFIX LLVM_CONFIG_PATH CARGO_HOME RUSTUP_HOME
    RUSTUP_TOOLCHAIN CARGO_TARGET_DIR CARGO_BUILD_TARGET TMPDIR
    CARGO_BUILD_JOBS CARGO_INCREMENTAL CARGO_NET_OFFLINE CARGO_TERM_COLOR
    RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTDOCFLAGS CARGO_ENCODED_RUSTDOCFLAGS
    RUSTC RUSTDOC RUSTC_BOOTSTRAP RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
    RUSTC_LINKER CARGO_BUILD_RUSTC CARGO_BUILD_RUSTDOC
    CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER
    CARGO_BUILD_RUSTFLAGS LLVM_CONFIG CC CXX AR RANLIB LD CPPFLAGS
    CFLAGS CXXFLAGS LDFLAGS LD_LIBRARY_PATH DYLD_LIBRARY_PATH
    DYLD_FALLBACK_LIBRARY_PATH LIBRARY_PATH CPATH LIBCLANG_PATH
    BINDGEN_EXTRA_CLANG_ARGS PKG_CONFIG PKG_CONFIG_PATH PKG_CONFIG_LIBDIR
    PKG_CONFIG_SYSROOT_DIR LD_PRELOAD LC_ALL LANG RUST_TEST_THREADS
    RUST_TEST_NOCAPTURE
)

for tool in bash cargo rustc valgrind env sha256sum realpath git find sort cmp grep awk file date diff comm cp mkdir sed tr wc basename dirname cat; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'Values cross-call String caller strict L3 requires %s\n' "$tool" >&2
        exit 1
    }
done

for fixture in "$FIXTURE_V1" "$FIXTURE_V2"; do
    if [[ ! -f "$fixture" ]]; then
        printf 'Values cross-call String caller fixture is unavailable: %s\n' "$fixture" >&2
        exit 1
    fi
done

if [[ ! -d "$LLVM_SYS_181_PREFIX" ]]; then
    printf 'repository LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi
if [[ -z "${MIMI_VALUES_CROSSCALL_STRING_CALLER_L3_TARGET_DIR:-}" \
    && ! -d /media/ontos-hpc/P7000Z2T ]]; then
    printf 'external Values cross-call String caller target volume is unavailable: %s\n' \
        /media/ontos-hpc/P7000Z2T >&2
    exit 1
fi
LLVM_SYS_181_PREFIX="$(realpath -- "$LLVM_SYS_181_PREFIX")"
case "$ARTIFACT_DIR/" in
    "$ROOT_DIR/evidence/"*) ;;
    *)
        printf 'Values cross-call String caller evidence must remain under the repository evidence tree: %s\n' "$ARTIFACT_DIR" >&2
        exit 2
        ;;
esac
if [[ -e "$TARGET_DIR_REQUESTED" || -L "$TARGET_DIR_REQUESTED" ]]; then
    printf 'refusing to reuse Values cross-call String caller L3 target cache: %s\n' \
        "$TARGET_DIR_REQUESTED" >&2
    exit 2
fi
TARGET_DIR="$(realpath -m -- "$TARGET_DIR_REQUESTED")"
case "$TARGET_DIR/" in
    "$ROOT_DIR/"*)
        printf 'Cargo target cache must be external to the repository: %s\n' "$TARGET_DIR" >&2
        exit 2
        ;;
esac
if [[ -e "$TARGET_DIR" || -L "$TARGET_DIR" ]]; then
    printf 'refusing to reuse Values cross-call String caller L3 target cache: %s\n' "$TARGET_DIR" >&2
    exit 2
fi
mkdir -p -- "$(dirname "$TARGET_DIR")"
mkdir -- "$TARGET_DIR"
TARGET_DIR="$(realpath -- "$TARGET_DIR")"
case "$TARGET_DIR/" in
    "$ROOT_DIR/"*)
        printf 'Cargo target cache must be external to the repository: %s\n' "$TARGET_DIR" >&2
        exit 2
        ;;
esac
EXTERNAL_TMPDIR_REQUESTED="$TARGET_DIR/tmp"
mkdir -p -- "$EXTERNAL_TMPDIR_REQUESTED"
EXTERNAL_TMPDIR="$(realpath -- "$EXTERNAL_TMPDIR_REQUESTED")"
case "$EXTERNAL_TMPDIR/" in
    "$ROOT_DIR/"*)
        printf 'Cargo TMPDIR must be external to the repository: %s\n' "$EXTERNAL_TMPDIR" >&2
        exit 2
        ;;
esac
case "$EXTERNAL_TMPDIR/" in
    "$TARGET_DIR/"*) ;;
    *)
        printf 'Cargo TMPDIR must remain beneath the external target cache: %s\n' "$EXTERNAL_TMPDIR" >&2
        exit 2
        ;;
esac
TMPDIR="$EXTERNAL_TMPDIR"
RUSTFLAGS="-C debuginfo=0"
CARGO_INCREMENTAL=0
LLVM_SYS_181_PREFIX="$(realpath -- "$LLVM_SYS_181_PREFIX")"
CARGO_TARGET_DIR="$TARGET_DIR"
CARGO_BUILD_JOBS=1
export LLVM_SYS_181_PREFIX CARGO_TARGET_DIR CARGO_BUILD_JOBS TMPDIR RUSTFLAGS CARGO_INCREMENTAL

if [[ -e "$ARTIFACT_DIR" || -L "$ARTIFACT_DIR" ]]; then
    printf 'refusing to overwrite Values cross-call String caller L3 evidence: %s\n' "$ARTIFACT_DIR" >&2
    exit 2
fi
mkdir -p -- "$(dirname "$ARTIFACT_DIR")"
mkdir -- "$ARTIFACT_DIR"
ARTIFACT_DIR="$(realpath -- "$ARTIFACT_DIR")"
export "$ARTIFACT_ENV=$ARTIFACT_DIR"
unset EFFECTIVE_ENVIRONMENT_SHA256 || true
EFFECTIVE_ENVIRONMENT_SHA256="$(env -0 | LC_ALL=C sort -z | sha256sum | awk '{print $1}')"
[[ "$EFFECTIVE_ENVIRONMENT_SHA256" =~ ^[0-9a-f]{64}$ ]] || {
    printf 'could not bind the effective runner environment\n' >&2
    exit 1
}

source_snapshot_manifest() {
    (
        cd "$ROOT_DIR"
        {
            for source_tree in src std stdlib tests scripts; do
                if [[ -d "$source_tree" ]]; then
                    find "$source_tree" -type f -print0
                fi
            done
            if [[ -d .cargo ]]; then
                find .cargo -type f -print0
            fi
            find . -maxdepth 1 -type f \
                \( -name 'Cargo*' -o -name '.gitignore' -o -name 'build.rs' \) -print0
        } | LC_ALL=C sort -zu | while IFS= read -r -d '' source_path; do
            sha256sum -- "$source_path"
        done
    )
}

record_tool() {
    local label="$1" executable="$2" status=0 directory="$ARTIFACT_DIR/toolchain/$1"
    shift 2
    mkdir -- "$directory"
    {
        printf '%q ' "$executable" "$@"
        printf '\n'
    } > "$directory/command.txt"
    printf '%s\0' "$executable" "$@" > "$directory/command.argv.nul"
    if "$executable" "$@" > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$directory/process.status"
    return "$status"
}

write_relevant_environment() {
    local name
    local -a environment_names
    mapfile -t environment_names < <(
        {
            printf '%s\n' "${RELEVANT_ENVIRONMENT[@]}"
            compgen -e | grep -E '^(CARGO_BUILD_|CARGO_PROFILE_|CARGO_TARGET_|CARGO_ALIAS_)' || true
        } | LC_ALL=C sort -u
    )
    for name in "${environment_names[@]}"; do
        if [[ -v "$name" ]]; then
            printf '%s=%q\n' "$name" "${!name}"
        else
            printf '%s=<unset>\n' "$name"
        fi
    done
    printf 'effective_environment_sha256=%s\n' "$EFFECTIVE_ENVIRONMENT_SHA256"
}

source_snapshot_manifest > "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256"
sha256sum -- "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256" \
    | awk '{print $1}' > "$ARTIFACT_DIR/source-snapshot.before.sha256"
mkdir -p -- "$ARTIFACT_DIR/toolchain"
mkdir -- "$ARTIFACT_DIR/source-fixtures"
cp -- "$FIXTURE_V1" "$ARTIFACT_DIR/source-fixtures/v1.mimi"
cp -- "$FIXTURE_V2" "$ARTIFACT_DIR/source-fixtures/v2-index1.mimi"
sha256sum -- "$ARTIFACT_DIR/source-fixtures/v1.mimi" > "$ARTIFACT_DIR/source-fixtures/v1.sha256"
sha256sum -- "$ARTIFACT_DIR/source-fixtures/v2-index1.mimi" > "$ARTIFACT_DIR/source-fixtures/v2-index1.sha256"
{
    printf 'case=v1\nfixture=%s\nroute_profile=%s\nexpected_mir_receipt_schema=%s\n' \
        "$FIXTURE_V1" "$PROFILE_V1" "$MIR_RECEIPT_SCHEMA_V1"
    printf 'case=v2-index1\nfixture=%s\nroute_profile=%s\nexpected_mir_receipt_schema=%s\n' \
        "$FIXTURE_V2" "$PROFILE_V2" "$MIR_RECEIPT_SCHEMA_V2"
} > "$ARTIFACT_DIR/source-fixtures/profiles.txt"

version_status=0
TOOLCHAIN_TOOLS=(bash cargo rustc valgrind env sha256sum realpath git find sort cmp grep awk file date diff comm cp mkdir sed tr wc basename dirname cat)
for tool in "${TOOLCHAIN_TOOLS[@]}"; do
    executable="$(type -P -- "$tool")"
    case "$tool" in
        bash|cargo|valgrind|env) version_arguments=(--version) ;;
        rustc) version_arguments=(--version --verbose) ;;
        awk) version_arguments=(-W version) ;;
        sha256sum|realpath|git|find|sort|cmp|grep|file|basename|cp|mkdir|sed|tr|wc|dirname|cat) version_arguments=(--version) ;;
        date|diff|comm) version_arguments=(--version) ;;
    esac
    if ! record_tool "$tool" "$executable" "${version_arguments[@]}"; then
        version_status=1
    fi
done

llvm_config="$LLVM_SYS_181_PREFIX/bin/llvm-config"
if [[ -x "$llvm_config" ]]; then
    if ! record_tool llvm-config "$llvm_config" --version; then
        version_status=1
    fi
else
    printf 'llvm_config=not-present-in-wrapper\n' > "$ARTIFACT_DIR/toolchain/llvm-config.txt"
fi

{
    printf 'run_id=%s\n' "$RUN_ID"
    printf 'route_profiles=%s\n' "$PROFILE_SET"
    printf 'v1_fixture=%s\n' "$FIXTURE_V1"
    printf 'v1_mir_receipt_schema=%s\n' "$MIR_RECEIPT_SCHEMA_V1"
    printf 'v2_index1_fixture=%s\n' "$FIXTURE_V2"
    printf 'v2_index1_mir_receipt_schema=%s\n' "$MIR_RECEIPT_SCHEMA_V2"
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
    printf 'effective_environment_sha256=%s\n' "$EFFECTIVE_ENVIRONMENT_SHA256"
    printf 'CARGO_TARGET_DIR=%s\n' "$TARGET_DIR"
    printf 'TMPDIR=%s\n' "$TMPDIR"
    printf 'RUSTFLAGS=%s\n' "$RUSTFLAGS"
    printf 'CARGO_INCREMENTAL=%s\n' "$CARGO_INCREMENTAL"
    printf 'fresh_target_cache=true\nfresh_artifact_directory=true\n'
    printf 'external_target_cache=true\n'
    printf 'runner_script=%s\n' "$ROOT_DIR/scripts/verify-ordinary-map-owned-any-v2-values-crosscall-string-caller-l3.sh"
    printf 'evidence_directory=%s\n' "$ARTIFACT_DIR"
    printf 'runner_arguments=%s\n' "$RUN_ID"
    printf 'test_target=%s\n' "$TEST_TARGET"
    printf 'test_name=%s\n' "$TEST_NAME"
    printf 'git_head='; git rev-parse HEAD
    printf 'git_branch='; git branch --show-current
    for tool in "${TOOLCHAIN_TOOLS[@]}"; do
        tool_executable="$(type -P -- "$tool")"
        printf '%s_path=%s\n' "$tool" "$tool_executable"
        printf '%s_executable_sha256=' "$tool"
        sha256sum -- "$tool_executable" | awk '{print $1}'
        printf '%s_version=' "$tool"
        cat "$ARTIFACT_DIR/toolchain/$tool/stdout.bin"
    done
    if [[ -x "$llvm_config" ]]; then
        printf 'llvm_config_path=%s\nllvm_config_version=' "$llvm_config"
        cat "$ARTIFACT_DIR/toolchain/llvm-config/stdout.bin"
        printf 'llvm_config_executable_sha256='
        sha256sum -- "$llvm_config" | awk '{print $1}'
    fi
    toolchain_directory="$ROOT_DIR"
    while :; do
        for config_name in rust-toolchain rust-toolchain.toml .cargo/config .cargo/config.toml; do
            config_path="$toolchain_directory/$config_name"
            if [[ -f "$config_path" ]]; then
                config_realpath="$(realpath -- "$config_path")"
                printf 'toolchain_input_path=%q\n' "$config_realpath"
                printf 'toolchain_input_sha256='
                sha256sum -- "$config_realpath" | awk '{print $1}'
            fi
        done
        [[ "$toolchain_directory" == / ]] && break
        toolchain_directory="$(dirname -- "$toolchain_directory")"
    done
    cargo_home="${CARGO_HOME:-${HOME:-}/.cargo}"
    for config_name in config config.toml; do
        config_path="$cargo_home/$config_name"
        if [[ -f "$config_path" ]]; then
            config_realpath="$(realpath -- "$config_path")"
            printf 'toolchain_input_path=%q\n' "$config_realpath"
            printf 'toolchain_input_sha256='
            sha256sum -- "$config_realpath" | awk '{print $1}'
        fi
    done
} > "$ARTIFACT_DIR/toolchain.txt" 2> "$ARTIFACT_DIR/toolchain.stderr"

cargo_arguments=(
    test
    --manifest-path "$ROOT_DIR/Cargo.toml"
    --target-dir "$TARGET_DIR"
    --test "$TEST_TARGET"
    "$TEST_NAME"
    --
    --exact
    --ignored
    --nocapture
    --test-threads=1
)
{
    printf 'LLVM_SYS_181_PREFIX=%q CARGO_TARGET_DIR=%q TMPDIR=%q CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=%q RUSTFLAGS=%q %s=%q ' \
        "$LLVM_SYS_181_PREFIX" "$TARGET_DIR" "$TMPDIR" "$CARGO_INCREMENTAL" "$RUSTFLAGS" "$ARTIFACT_ENV" "$ARTIFACT_DIR"
    printf '%q ' "$(type -P -- cargo)" "${cargo_arguments[@]}"
    printf '\n'
} > "$ARTIFACT_DIR/suite.command.txt"
printf '%s\0' "$(type -P -- cargo)" "${cargo_arguments[@]}" \
    > "$ARTIFACT_DIR/suite.argv.nul"
{
    printf 'run_id=%s\n' "$RUN_ID"
    printf 'route_profiles=%s\n' "$PROFILE_SET"
    printf 'v1_fixture=%s\n' "$FIXTURE_V1"
    printf 'v1_route_profile=%s\n' "$PROFILE_V1"
    printf 'v1_mir_receipt_schema=%s\n' "$MIR_RECEIPT_SCHEMA_V1"
    printf 'v2_index1_fixture=%s\n' "$FIXTURE_V2"
    printf 'v2_index1_route_profile=%s\n' "$PROFILE_V2"
    printf 'v2_index1_mir_receipt_schema=%s\n' "$MIR_RECEIPT_SCHEMA_V2"
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
    printf 'CARGO_TARGET_DIR=%s\n' "$TARGET_DIR"
    printf 'TMPDIR=%s\n' "$TMPDIR"
    printf 'CARGO_BUILD_JOBS=1\n'
    printf 'fresh_target_cache=true\nfresh_artifact_directory=true\n'
    printf '%s=%s\n' "$ARTIFACT_ENV" "$ARTIFACT_DIR"
    printf 'test_harness_threads=1\n'
    printf 'working_directory=%s\n' "$ROOT_DIR"
    printf 'relevant_environment_begin\n'
    write_relevant_environment
    printf 'relevant_environment_end\n'
} > "$ARTIFACT_DIR/suite.environment.txt"
{
    printf 'runner=%s\n' "$ROOT_DIR/scripts/verify-ordinary-map-owned-any-v2-values-crosscall-string-caller-l3.sh"
    printf 'runner_argument=%s\n' "$RUN_ID"
    printf 'working_directory=%s\n' "$ROOT_DIR"
    printf 'evidence_directory=%s\n' "$ARTIFACT_DIR"
    printf 'cargo_target_directory=%s\n' "$TARGET_DIR"
    printf 'tmp_directory=%s\n' "$TMPDIR"
    printf 'fresh_target_cache=true\nfresh_artifact_directory=true\n'
    printf 'inherited_variables=other_environment_inherited_by_the_runner\n'
    printf 'relevant_environment_begin\n'
    write_relevant_environment
    printf 'relevant_environment_end\n'
} > "$ARTIFACT_DIR/runner.environment.txt"
{
    printf 'LLVM_SYS_181_PREFIX=%q CARGO_TARGET_DIR=%q TMPDIR=%q %q %q\n' \
        "$LLVM_SYS_181_PREFIX" "$TARGET_DIR" "$TMPDIR" \
        "$ROOT_DIR/scripts/verify-ordinary-map-owned-any-v2-values-crosscall-string-caller-l3.sh" "$RUN_ID"
} > "$ARTIFACT_DIR/runner.command.txt"
printf '%s\0' \
    "$ROOT_DIR/scripts/verify-ordinary-map-owned-any-v2-values-crosscall-string-caller-l3.sh" \
    "$RUN_ID" > "$ARTIFACT_DIR/runner.argv.nul"

suite_status=0
if [[ "$version_status" -ne 0 ]]; then
    suite_status=1
    : > "$ARTIFACT_DIR/suite.stdout"
    printf 'tool version capture failed; see toolchain/\n' > "$ARTIFACT_DIR/suite.stderr"
else
    if env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        CARGO_TARGET_DIR="$TARGET_DIR" TMPDIR="$TMPDIR" CARGO_BUILD_JOBS=1 \
        "$ARTIFACT_ENV=$ARTIFACT_DIR" cargo "${cargo_arguments[@]}" \
        > "$ARTIFACT_DIR/suite.stdout" 2> "$ARTIFACT_DIR/suite.stderr"; then
        suite_status=0
    else
        suite_status=$?
    fi
fi
printf 'exit_code=%s\nsuccess=%s\n' "$suite_status" \
    "$([[ "$suite_status" -eq 0 ]] && printf true || printf false)" \
    > "$ARTIFACT_DIR/suite.process.status"

snapshot_status=0
: >| "$ARTIFACT_DIR/source-snapshot.diff"
if source_snapshot_manifest > "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256" \
    2> "$ARTIFACT_DIR/source-snapshot.after.stderr"; then
    sha256sum -- "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256" \
        | awk '{print $1}' > "$ARTIFACT_DIR/source-snapshot.after.sha256"
    if cmp -s "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256" \
        "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256" \
        && cmp -s "$ARTIFACT_DIR/source-snapshot.before.sha256" \
            "$ARTIFACT_DIR/source-snapshot.after.sha256"; then
        printf 'stable=true\n' > "$ARTIFACT_DIR/source-snapshot.status"
    else
        diff -u "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256" \
            "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256" \
            >| "$ARTIFACT_DIR/source-snapshot.diff" || true
        printf 'stable=false\n' > "$ARTIFACT_DIR/source-snapshot.status"
        snapshot_status=1
    fi
else
    printf 'stable=false\nmanifest_capture_failed=true\n' > "$ARTIFACT_DIR/source-snapshot.status"
    snapshot_status=1
fi

source_status=0
for case_id in v1 v2-index1; do
    case "$case_id" in
        v1) fixture="$FIXTURE_V1" ;;
        v2-index1) fixture="$FIXTURE_V2" ;;
    esac
    if ! cmp -s "$fixture" "$ARTIFACT_DIR/source-fixtures/$case_id.mimi"; then
        printf 'source fixture snapshot changed: %s\n' "$fixture" >&2
        source_status=1
    fi
    current_hash="$(sha256sum -- "$fixture" | awk '{print $1}')"
    saved_hash="$(awk '{print $1}' "$ARTIFACT_DIR/source-fixtures/$case_id.sha256")"
    if [[ -z "$current_hash" || "$current_hash" != "$saved_hash" ]]; then
        printf 'source fixture hash changed: %s\n' "$fixture" >&2
        source_status=1
    fi
done
if [[ ! -f "$ARTIFACT_DIR/source-snapshot.status" ]] \
    || ! grep -Fxq 'stable=true' "$ARTIFACT_DIR/source-snapshot.status"; then
    source_status=1
fi

# Pre-create final records so the exact inventory check sees all required paths.
: > "$ARTIFACT_DIR/gate.status"
: > "$ARTIFACT_DIR/runner.process.status"
: > "$ARTIFACT_DIR/SHA256SUMS"
: > "$ARTIFACT_DIR/evidence-layout-check.txt"
: > "$ARTIFACT_DIR/expected-evidence-files.txt"
: > "$ARTIFACT_DIR/actual-evidence-files.txt"
: > "$ARTIFACT_DIR/expected-evidence-directories.txt"
: > "$ARTIFACT_DIR/actual-evidence-directories.txt"

ignore_status=0
: > "$ARTIFACT_DIR/binary-ignore-check.txt"
: > "$ARTIFACT_DIR/binary-ignore-check.stderr"
for source_path in \
    tests/canonical_mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_memcheck_l3.rs \
    scripts/verify-ordinary-map-owned-any-v2-values-crosscall-string-caller-l3.sh; do
    if git -C "$ROOT_DIR" check-ignore -q -- "$source_path" \
        2>> "$ARTIFACT_DIR/binary-ignore-check.stderr"; then
        printf 'source file is unexpectedly ignored: %s\n' "$source_path" \
            >> "$ARTIFACT_DIR/binary-ignore-check.txt"
        ignore_status=1
    else
        check_status=$?
        if [[ "$check_status" == 1 ]]; then
            printf 'visible source: %s\n' "$source_path" \
                >> "$ARTIFACT_DIR/binary-ignore-check.txt"
        else
            printf 'git check-ignore failed for source %s with exit %s\n' "$source_path" "$check_status" \
                >> "$ARTIFACT_DIR/binary-ignore-check.txt"
            ignore_status=1
        fi
    fi
done

for case_id in v1 v2-index1; do
    for mode in default explicit-mir; do
        binary="$ARTIFACT_DIR/native/$case_id/$mode/program"
        relative_binary="${binary#"$ROOT_DIR"/}"
        if [[ ! -x "$binary" ]]; then
            printf 'missing generated ELF binary: %s\n' "$relative_binary" \
                >> "$ARTIFACT_DIR/binary-ignore-check.txt"
            ignore_status=1
            continue
        fi
        file_status=0
        if description="$(file -b -- "$binary" 2>> "$ARTIFACT_DIR/binary-ignore-check.stderr")"; then
            file_status=0
        else
            file_status=$?
        fi
        printf 'binary=%s\nfile_type=%s\nfile_exit_code=%s\n' "$relative_binary" "$description" "$file_status" \
            >> "$ARTIFACT_DIR/binary-ignore-check.txt"
        if [[ "$file_status" -ne 0 || "$description" != *ELF* ]]; then
            printf 'generated program is not identified as ELF: %s\n' "$relative_binary" \
                >> "$ARTIFACT_DIR/binary-ignore-check.txt"
            ignore_status=1
        fi
        if git -C "$ROOT_DIR" check-ignore -v -- "$relative_binary" \
            >> "$ARTIFACT_DIR/binary-ignore-check.txt" \
            2>> "$ARTIFACT_DIR/binary-ignore-check.stderr"; then
            :
        else
            printf 'generated ELF binary is not ignored: %s\n' "$relative_binary" \
                >> "$ARTIFACT_DIR/binary-ignore-check.txt"
            ignore_status=1
        fi
    done
done

while IFS= read -r -d '' evidence_file; do
    relative_evidence="${evidence_file#"$ROOT_DIR"/}"
    case "$relative_evidence" in
        evidence/*/native/v1/default/program|evidence/*/native/v1/explicit-mir/program|\
        evidence/*/native/v2-index1/default/program|evidence/*/native/v2-index1/explicit-mir/program) continue ;;
    esac
    if git -C "$ROOT_DIR" check-ignore -q -- "$relative_evidence" \
        2>> "$ARTIFACT_DIR/binary-ignore-check.stderr"; then
        printf 'non-binary evidence is unexpectedly ignored: %s\n' "$relative_evidence" \
            >> "$ARTIFACT_DIR/binary-ignore-check.txt"
        ignore_status=1
    else
        check_status=$?
        if [[ "$check_status" == 1 ]]; then
            printf 'visible evidence: %s\n' "$relative_evidence" \
                >> "$ARTIFACT_DIR/binary-ignore-check.txt"
        else
            printf 'git check-ignore failed for evidence %s with exit %s\n' \
                "$relative_evidence" "$check_status" \
                >> "$ARTIFACT_DIR/binary-ignore-check.txt"
            ignore_status=1
        fi
    fi
done < <(find "$ARTIFACT_DIR" -type f -print0)
if [[ -s "$ARTIFACT_DIR/binary-ignore-check.stderr" ]]; then
    printf 'binary visibility checks emitted errors; inspect binary-ignore-check.stderr\n' >&2
    ignore_status=1
fi

validation_status=0
if [[ "$suite_status" -eq 0 ]]; then
    summary="$ARTIFACT_DIR/test-summary.txt"
    for expected in \
        'fixture_count=2' 'native_build_count=4' 'strict_memcheck_count=4' 'case_count=2' \
        'all_default_explicit_identity_equal=true' 'expected_stdout_hex=747275650a' \
        "v1_fixture=$(basename -- "$FIXTURE_V1")" \
        "v1_route_profile=$PROFILE_V1" "v1_observed_route_profile=$PROFILE_V1" \
        "v1_route_receipt_schema=$ROUTE_CENSUS_SCHEMA" \
        "v1_observed_mir_receipt_schema=$MIR_RECEIPT_SCHEMA_V1" \
        "v2-index1_fixture=$(basename -- "$FIXTURE_V2")" \
        "v2-index1_route_profile=$PROFILE_V2" \
        "v2-index1_observed_route_profile=$PROFILE_V2" \
        "v2-index1_route_receipt_schema=$ROUTE_CENSUS_SCHEMA" \
        "v2-index1_observed_mir_receipt_schema=$MIR_RECEIPT_SCHEMA_V2"; do
        if [[ ! -f "$summary" ]] || ! grep -Fxq "$expected" "$summary"; then
            printf 'L3 summary is missing %s\n' "$expected" >&2
            validation_status=1
        fi
    done
    [[ "$(find "$ARTIFACT_DIR/native" -type f -name program | wc -l | tr -d '[:space:]')" == 4 ]] || validation_status=1
    [[ "$(find "$ARTIFACT_DIR/native" -type f -name valgrind.log | wc -l | tr -d '[:space:]')" == 4 ]] || validation_status=1
    [[ "$(find "$ARTIFACT_DIR/native" -type f -name memcheck-summary.txt | wc -l | tr -d '[:space:]')" == 4 ]] || validation_status=1
    [[ "$(find "$ARTIFACT_DIR/native" -type f -name program.sha256 | wc -l | tr -d '[:space:]')" == 4 ]] || validation_status=1
    [[ "$(find "$ARTIFACT_DIR/native" -type f -path '*/memcheck/valgrind-command/command.argv.json' | wc -l | tr -d '[:space:]')" == 4 ]] || validation_status=1
    [[ "$(find "$ARTIFACT_DIR/native" -type f -path '*/native-command/command.argv.json' | wc -l | tr -d '[:space:]')" == 4 ]] || validation_status=1

    for case_id in v1 v2-index1; do
        case "$case_id" in
            v1)
                profile="$PROFILE_V1"
                receipt_schema="$MIR_RECEIPT_SCHEMA_V1"
                summary_prefix=v1
                ;;
            v2-index1)
                profile="$PROFILE_V2"
                receipt_schema="$MIR_RECEIPT_SCHEMA_V2"
                summary_prefix=v2-index1
                ;;
        esac
        for mode in default explicit-mir; do
            mode_dir="$ARTIFACT_DIR/native/$case_id/$mode"
            if [[ ! -x "$mode_dir/program" ]]; then
                printf 'missing executable native artifact: %s\n' "$mode_dir/program" >&2
                validation_status=1
                continue
            fi
            for status_file in \
                "$mode_dir/build-command/process.status" \
                "$mode_dir/native-command/process.status" \
                "$mode_dir/binary-hash-command/process.status" \
                "$mode_dir/memcheck/valgrind-command/process.status"; do
                if [[ ! -f "$status_file" ]] \
                    || ! grep -Fxq 'exit_code=0' "$status_file" \
                    || ! grep -Fxq 'success=true' "$status_file"; then
                    printf 'successful command status is missing from %s\n' "$status_file" >&2
                    validation_status=1
                fi
            done
            measured_binary_hash="$(awk 'NR == 1 {print $1}' "$mode_dir/binary-hash-command/stdout.bin" 2>/dev/null || true)"
            retained_binary_hash="$(cat "$mode_dir/program.sha256" 2>/dev/null || true)"
            [[ "$measured_binary_hash" =~ ^[0-9a-f]{64}$ ]] || validation_status=1
            [[ "$measured_binary_hash" == "$retained_binary_hash" ]] || validation_status=1
            printf 'true\n' | cmp -s - "$mode_dir/expected-stdout.bin" || validation_status=1
            printf 'true\n' | cmp -s - "$mode_dir/native-command/stdout.bin" || validation_status=1
            [[ ! -s "$mode_dir/native-command/stderr.bin" ]] || validation_status=1
            printf 'true\n' | cmp -s - "$mode_dir/memcheck/valgrind-command/stdout.bin" || validation_status=1
            [[ ! -s "$mode_dir/memcheck/valgrind-command/stderr.bin" ]] || validation_status=1
            for expected in \
                'definitely lost: 0 bytes in 0 blocks' \
                'indirectly lost: 0 bytes in 0 blocks' \
                'possibly lost: 0 bytes in 0 blocks' \
                'ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)'; do
                if [[ ! -f "$mode_dir/memcheck/memcheck-summary.txt" ]] \
                    || ! grep -Fq "$expected" "$mode_dir/memcheck/memcheck-summary.txt"; then
                    printf '%s Memcheck summary is missing %s\n' "$mode_dir" "$expected" >&2
                    validation_status=1
                fi
            done
            reachable_count="$(grep -Ec '^still reachable:' "$mode_dir/memcheck/memcheck-summary.txt" 2>/dev/null || true)"
            if [[ "$reachable_count" != 1 ]]; then
                printf '%s must retain exactly one still-reachable row\n' "$mode_dir" >&2
                validation_status=1
            else
                reachable_line="$(grep -F 'still reachable:' "$mode_dir/memcheck/memcheck-summary.txt")"
                summary_key="${summary_prefix}_${mode//-/_}_still_reachable"
                grep -Fxq "$summary_key=$reachable_line" "$summary" || validation_status=1
                if [[ "$reachable_line" != 'still reachable: 0 bytes in 0 blocks' \
                    && "$reachable_line" != 'still reachable: 0 bytes in 0 blocks (inferred from all-free summary)' ]]; then
                    printf '%s has nonzero still-reachable allocations: %s\n' \
                        "$mode_dir" "$reachable_line" >&2
                    validation_status=1
                fi
            fi

            identity="$mode_dir/route-identity.txt"
            for expected in \
                "case=$case_id" "mode=$mode" "profile=$profile" \
                "route_receipt_schema=$ROUTE_CENSUS_SCHEMA" \
                "observed_mir_receipt_schema=$receipt_schema"; do
                if [[ ! -f "$identity" ]] || ! grep -Fxq "$expected" "$identity"; then
                    printf '%s route identity is missing %s\n' "$mode_dir" "$expected" >&2
                    validation_status=1
                fi
            done
            mir_digest="$(sed -n 's/^mir_digest=//p' "$identity" 2>/dev/null || true)"
            receipt_digest="$(sed -n 's/^receipt_identity_digest=//p' "$identity" 2>/dev/null || true)"
            [[ "$mir_digest" =~ ^[0-9a-f]{64}$ ]] || validation_status=1
            [[ "$receipt_digest" =~ ^[0-9a-f]{64}$ ]] || validation_status=1
            build_stderr="$mode_dir/build-command/stderr.bin"
            [[ "$(grep -Fc 'MIMI_ROUTE_CENSUS route-admission ' "$build_stderr" 2>/dev/null || true)" == 1 ]] || validation_status=1
            [[ "$(grep -Fc 'MIMI_ROUTE_CENSUS route-specialized-schema ' "$build_stderr" 2>/dev/null || true)" == 1 ]] || validation_status=1
            [[ "$(grep -Fc 'MIMI_ROUTE_CENSUS canonical-native-build ' "$build_stderr" 2>/dev/null || true)" == 1 ]] || validation_status=1
            route_line="$(grep -F 'MIMI_ROUTE_CENSUS route-admission ' "$build_stderr" 2>/dev/null || true)"
            specialized_schema_line="$(grep -F 'MIMI_ROUTE_CENSUS route-specialized-schema ' "$build_stderr" 2>/dev/null || true)"
            native_line="$(grep -F 'MIMI_ROUTE_CENSUS canonical-native-build ' "$build_stderr" 2>/dev/null || true)"
            grep -Fq "checker_profile=$profile" <<< "$route_line" || validation_status=1
            grep -Fq "mode=$mode" <<< "$route_line" || validation_status=1
            grep -Fq "mir_digest=$mir_digest" <<< "$route_line" || validation_status=1
            grep -Fq "receipt_identity_digest=$receipt_digest" <<< "$route_line" || validation_status=1
            expected_specialized_schema_line="MIMI_ROUTE_CENSUS route-specialized-schema consumer=$mode checker_profile=$profile receipt_profile=$profile schema=$receipt_schema mir_digest=$mir_digest receipt_identity_digest=$receipt_digest"
            [[ "$specialized_schema_line" == "$expected_specialized_schema_line" ]] || validation_status=1
            grep -Fq "route_id=$profile" <<< "$native_line" || validation_status=1
            grep -Fq "profile=$profile" <<< "$native_line" || validation_status=1
            grep -Fq "schema=$ROUTE_CENSUS_SCHEMA" <<< "$native_line" || validation_status=1
            grep -Fq "consumer=$mode" <<< "$native_line" || validation_status=1
            grep -Fq "mir_digest=$mir_digest" <<< "$native_line" || validation_status=1
            grep -Fq "route_identity_digest=$receipt_digest" <<< "$native_line" || validation_status=1
            if grep -Fq 'canonical route disposition: legacy' "$build_stderr"; then
                printf '%s/%s native build selected a legacy fallback\n' "$case_id" "$mode" >&2
                validation_status=1
            fi
        done

        default_identity="$ARTIFACT_DIR/native/$case_id/default/route-identity.txt"
        explicit_identity="$ARTIFACT_DIR/native/$case_id/explicit-mir/route-identity.txt"
        for field in mir_digest receipt_identity_digest; do
            default_value="$(sed -n "s/^${field}=//p" "$default_identity" 2>/dev/null || true)"
            explicit_value="$(sed -n "s/^${field}=//p" "$explicit_identity" 2>/dev/null || true)"
            [[ -n "$default_value" && "$default_value" == "$explicit_value" ]] || validation_status=1
            summary_value="$(sed -n "s/^${summary_prefix}_${field}=//p" "$summary" 2>/dev/null || true)"
            [[ "$summary_value" == "$default_value" ]] || validation_status=1
        done
        grep -Fxq "${summary_prefix}_default_explicit_identity_equal=true" "$summary" || validation_status=1
    done
fi

expected_files=(
    source-snapshot.before.manifest.sha256 source-snapshot.before.sha256
    source-snapshot.after.manifest.sha256 source-snapshot.after.sha256
    source-snapshot.status source-snapshot.after.stderr source-snapshot.diff
    source-fixtures/v1.mimi source-fixtures/v1.sha256
    source-fixtures/v2-index1.mimi source-fixtures/v2-index1.sha256
    source-fixtures/profiles.txt toolchain.txt toolchain.stderr
    runner.environment.txt runner.command.txt runner.argv.nul
    binary-ignore-check.txt binary-ignore-check.stderr
    suite.command.txt suite.argv.nul suite.environment.txt suite.stdout suite.stderr
    suite.process.status test-summary.txt gate.status runner.process.status
    SHA256SUMS evidence-layout-check.txt expected-evidence-files.txt actual-evidence-files.txt
    expected-evidence-directories.txt actual-evidence-directories.txt
)
for tool in "${TOOLCHAIN_TOOLS[@]}"; do
    for file in command.txt command.argv.nul stdout.bin stderr.bin process.status; do
        expected_files+=("toolchain/$tool/$file")
    done
done
if [[ -x "$llvm_config" ]]; then
    for file in command.txt command.argv.nul stdout.bin stderr.bin process.status; do
        expected_files+=("toolchain/llvm-config/$file")
    done
else
    expected_files+=(toolchain/llvm-config.txt)
fi
for case_id in v1 v2-index1; do
    for mode in default explicit-mir; do
        for directory in build-command native-command binary-hash-command memcheck/valgrind-command; do
            for file in command.argv.json command.cwd.txt command.environment.txt stdout.bin stderr.bin process.status; do
                expected_files+=("native/$case_id/$mode/$directory/$file")
            done
        done
        expected_files+=(
            "native/$case_id/$mode/expected-stdout.bin"
            "native/$case_id/$mode/program"
            "native/$case_id/$mode/program.sha256"
            "native/$case_id/$mode/route-identity.txt"
            "native/$case_id/$mode/memcheck/valgrind.log"
            "native/$case_id/$mode/memcheck/memcheck-summary.txt"
        )
    done
done
printf '%s\n' "${expected_files[@]}" | LC_ALL=C sort >| "$ARTIFACT_DIR/expected-evidence-files.txt"
(
    cd -- "$ARTIFACT_DIR"
    find . -type f -printf '%P\n' | LC_ALL=C sort
) >| "$ARTIFACT_DIR/actual-evidence-files.txt"

expected_directories=(toolchain native source-fixtures)
for tool in "${TOOLCHAIN_TOOLS[@]}"; do
    expected_directories+=("toolchain/$tool")
done
if [[ -x "$llvm_config" ]]; then
    expected_directories+=(toolchain/llvm-config)
fi
for case_id in v1 v2-index1; do
    expected_directories+=("native/$case_id")
    for mode in default explicit-mir; do
        expected_directories+=(
            "native/$case_id/$mode"
            "native/$case_id/$mode/build-command"
            "native/$case_id/$mode/native-command"
            "native/$case_id/$mode/binary-hash-command"
            "native/$case_id/$mode/memcheck"
            "native/$case_id/$mode/memcheck/isolated-home"
            "native/$case_id/$mode/memcheck/valgrind-command"
        )
    done
done
printf '%s\n' "${expected_directories[@]}" | LC_ALL=C sort >| "$ARTIFACT_DIR/expected-evidence-directories.txt"
(
    cd -- "$ARTIFACT_DIR"
    find . -mindepth 1 -type d -printf '%P\n' | LC_ALL=C sort
) >| "$ARTIFACT_DIR/actual-evidence-directories.txt"

inventory_status=0
if cmp -s "$ARTIFACT_DIR/expected-evidence-files.txt" "$ARTIFACT_DIR/actual-evidence-files.txt" \
    && cmp -s "$ARTIFACT_DIR/expected-evidence-directories.txt" \
        "$ARTIFACT_DIR/actual-evidence-directories.txt"; then
    printf 'exact_file_inventory=true\nexact_directory_inventory=true\n' \
        >| "$ARTIFACT_DIR/evidence-layout-check.txt"
else
    {
        if cmp -s "$ARTIFACT_DIR/expected-evidence-files.txt" "$ARTIFACT_DIR/actual-evidence-files.txt"; then
            printf 'exact_file_inventory=true\n'
        else
            printf 'exact_file_inventory=false\nmissing_files:\n'
            comm -23 "$ARTIFACT_DIR/expected-evidence-files.txt" "$ARTIFACT_DIR/actual-evidence-files.txt"
            printf 'unexpected_files:\n'
            comm -13 "$ARTIFACT_DIR/expected-evidence-files.txt" "$ARTIFACT_DIR/actual-evidence-files.txt"
        fi
        if cmp -s "$ARTIFACT_DIR/expected-evidence-directories.txt" \
            "$ARTIFACT_DIR/actual-evidence-directories.txt"; then
            printf 'exact_directory_inventory=true\n'
        else
            printf 'exact_directory_inventory=false\nmissing_directories:\n'
            comm -23 "$ARTIFACT_DIR/expected-evidence-directories.txt" \
                "$ARTIFACT_DIR/actual-evidence-directories.txt"
            printf 'unexpected_directories:\n'
            comm -13 "$ARTIFACT_DIR/expected-evidence-directories.txt" \
                "$ARTIFACT_DIR/actual-evidence-directories.txt"
        fi
    } >| "$ARTIFACT_DIR/evidence-layout-check.txt"
    inventory_status=1
fi

final_status=0
if [[ "$suite_status" -ne 0 || "$snapshot_status" -ne 0 || "$source_status" -ne 0 \
    || "$ignore_status" -ne 0 || "$validation_status" -ne 0 || "$inventory_status" -ne 0 ]]; then
    final_status=1
fi
printf 'route_profiles=%s\nexit_code=%s\nsource_tree_unchanged=%s\nfixture_snapshots_equal=%s\nexact_evidence_inventory=%s\nexact_file_inventory=%s\nexact_directory_inventory=%s\nglobal_map_any_v2_production_ready=false\n' \
    "$PROFILE_SET" "$final_status" \
    "$( [[ "$snapshot_status" -eq 0 ]] && printf true || printf false )" \
    "$( [[ "$source_status" -eq 0 ]] && printf true || printf false )" \
    "$( [[ "$inventory_status" -eq 0 ]] && printf true || printf false )" \
    "$( grep -Fxq 'exact_file_inventory=true' "$ARTIFACT_DIR/evidence-layout-check.txt" && printf true || printf false )" \
    "$( grep -Fxq 'exact_directory_inventory=true' "$ARTIFACT_DIR/evidence-layout-check.txt" && printf true || printf false )" \
    >| "$ARTIFACT_DIR/gate.status"
printf 'exit_code=%s\nsuccess=%s\n' "$final_status" \
    "$( [[ "$final_status" -eq 0 ]] && printf true || printf false )" \
    >| "$ARTIFACT_DIR/runner.process.status"
(
    cd -- "$ARTIFACT_DIR"
    find . -type f ! -name SHA256SUMS \
        ! -path './native/*/default/program' \
        ! -path './native/*/explicit-mir/program' \
        -print0 | LC_ALL=C sort -z | while IFS= read -r -d '' file; do
            sha256sum -- "$file"
        done
) >| "$ARTIFACT_DIR/SHA256SUMS"
if [[ "$final_status" -eq 0 ]]; then
    printf 'Values cross-call String caller strict L3 evidence captured at %s\n' "$ARTIFACT_DIR"
else
    printf 'Values cross-call String caller strict L3 gate failed; inspect %s\n' "$ARTIFACT_DIR" >&2
fi
exit "$final_status"
