#!/usr/bin/env bash
set -euo pipefail
set -o noclobber

CLEARED_BUILD_RUNTIME_ENV=(
    BINDGEN_EXTRA_CLANG_ARGS CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS
    CC CFLAGS CXX CXXFLAGS LD_LIBRARY_PATH LD_PRELOAD LDFLAGS LIBCLANG_PATH
    MIMI_ASAN MIMI_TEST_FORCE_NO_Z3 RUSTC_WORKSPACE_WRAPPER RUSTC_WRAPPER RUSTFLAGS
)
declare -A INITIAL_ENV_PROVENANCE=()
inherited_environment_names=()
mapfile -t inherited_environment_names < <(compgen -e)
for variable in "${CLEARED_BUILD_RUNTIME_ENV[@]}"; do
    if [[ -v "$variable" ]]; then
        INITIAL_ENV_PROVENANCE["$variable"]=inherited-set
    else
        INITIAL_ENV_PROVENANCE["$variable"]=inherited-unset
    fi
    unset "$variable"
done
valgrind_environment_names=()
for variable in "${inherited_environment_names[@]}"; do
    case "$variable" in
        VALGRIND_*)
            valgrind_environment_names+=("$variable")
            INITIAL_ENV_PROVENANCE["$variable"]=inherited-set
            unset "$variable"
            ;;
    esac
done
if [[ ! " ${valgrind_environment_names[*]} " == *" VALGRIND_OPTS "* ]]; then
    valgrind_environment_names+=(VALGRIND_OPTS)
    INITIAL_ENV_PROVENANCE[VALGRIND_OPTS]=inherited-unset
fi
valgrind_environment_unsets=()
runner_environment_unsets=()
for variable in "${CLEARED_BUILD_RUNTIME_ENV[@]}"; do
    runner_environment_unsets+=(-u "$variable")
done
for variable in "${valgrind_environment_names[@]}"; do
    valgrind_environment_unsets+=(-u "$variable")
done
runner_environment_unsets+=("${valgrind_environment_unsets[@]}")

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
MIMI_STDLIB="$ROOT_DIR/std"
export MIMI_STDLIB
if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper; fi
if [ -x "$LLVM_SYS_181_PREFIX/bin/llvm-config" ]; then
    LLVM_CONFIG_BIN="$LLVM_SYS_181_PREFIX/bin/llvm-config"
elif [ -x "$LLVM_SYS_181_PREFIX/llvm-config" ]; then
    LLVM_CONFIG_BIN="$LLVM_SYS_181_PREFIX/llvm-config"
elif command -v llvm-config >/dev/null 2>&1; then
    LLVM_CONFIG_BIN="$(command -v llvm-config)"
else
    printf 'llvm-config is unavailable for LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi
ARTIFACT_ENV=MIMI_MAP_V2_ZERO_MISS_I32_L3_ARTIFACT_DIR
TEST_TARGET=canonical_mir_ordinary_map_any_v2_zero_miss_i32_memcheck_l3
TEST_NAME=ordinary_map_owned_any_v2_zero_miss_i32_cli_and_native_memcheck_are_strict
MIR_RECEIPT_HEADER=mimi-mir-route-manifest-v1
CHECKER_INSPECTION_HEADER=mimi-checker-inspection-manifest-v1
CHECKER_INSPECTION_SCHEMA=mimi-checker-inspection-receipt-v1
CHECKER_INSPECTION_PROFILE=cli-checker-inspection-v1

for tool in bash cargo rustc valgrind sha256sum realpath git uname find sort wc env; do
    command -v "$tool" >/dev/null 2>&1 || { printf 'ZeroMissI32 strict L3 requires %s\n' "$tool" >&2; exit 1; }
done
[ -d "$LLVM_SYS_181_PREFIX" ] || { printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2; exit 1; }

artifact_dir=""
if [[ -v MIMI_MAP_V2_ZERO_MISS_I32_L3_ARTIFACT_DIR && -n "$MIMI_MAP_V2_ZERO_MISS_I32_L3_ARTIFACT_DIR" ]]; then
    requested="$MIMI_MAP_V2_ZERO_MISS_I32_L3_ARTIFACT_DIR"
    case "$requested" in /*) ;; *) requested="$ROOT_DIR/$requested" ;; esac
else
    requested="${TMPDIR:-/tmp}/mimi-map-v2-zero-miss-i32-l3-$$"
fi
case "$requested" in /*) ;; *) requested="$ROOT_DIR/$requested" ;; esac
if [ -e "$requested" ] || [ -L "$requested" ]; then
    printf 'refusing to overwrite existing ZeroMissI32 L3 artifact path: %s\n' "$requested" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    */.git/*)
        printf 'ZeroMissI32 L3 artifacts must not be written under Git metadata: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
case "$artifact_dir/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/scripts/"*|"$ROOT_DIR/target/"*)
        printf 'ZeroMissI32 L3 artifacts must be outside source/build trees: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
export MIMI_MAP_V2_ZERO_MISS_I32_L3_ARTIFACT_DIR="$artifact_dir"

if [[ -v MIMI_MAP_V2_ZERO_MISS_I32_L3_LOG && -n "$MIMI_MAP_V2_ZERO_MISS_I32_L3_LOG" ]]; then
    log_file="$MIMI_MAP_V2_ZERO_MISS_I32_L3_LOG"
else
    log_file="/tmp/mimi-map-v2-zero-miss-i32-l3-$$.log"
fi
case "$log_file" in /*) ;; *) log_file="$ROOT_DIR/$log_file" ;; esac
if [ -e "$log_file" ] || [ -L "$log_file" ]; then
    printf 'refusing to overwrite existing ZeroMissI32 L3 log: %s\n' "$log_file" >&2
    exit 2
fi
log_file_absolute="$(realpath -m -- "$log_file")"
case "$log_file_absolute/" in
    */.git/*)
        printf 'ZeroMissI32 L3 suite log must not be written under Git metadata: %s\n' "$log_file_absolute" >&2
        exit 2 ;;
esac
if [ "$log_file_absolute" = "$artifact_dir" ] || [[ "$log_file_absolute" == "$artifact_dir/"* ]]; then
    printf 'ZeroMissI32 L3 suite log must be outside the artifact directory: %s\n' "$log_file_absolute" >&2
    exit 2
fi
case "$log_file_absolute/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/scripts/"*|"$ROOT_DIR/target/"*)
        printf 'ZeroMissI32 L3 suite log must be outside source/build trees: %s\n' "$log_file_absolute" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname "$log_file_absolute")"
scratch="$(mktemp -d /tmp/mimi-map-v2-zero-miss-i32-l3.XXXXXX)"
trap 'rm -rf -- "$scratch"' EXIT

is_sha256() {
    [[ "$1" =~ ^[0-9a-f]{64}$ ]]
}

source_snapshot_manifest() {
    local destination="$1"
    local errors_file="$2"
    local base="$(basename "$destination")"
    local paths="$scratch/$base.paths.nul"
    local sorted_paths="$scratch/$base.sorted.paths.nul"
    local status=0
    local path_bytes observed_path_bytes=0 source_path
    local -a source_paths=()
    local LC_ALL=C
    if ! : > "$destination" || ! : > "$errors_file" || ! : > "$paths"; then
        printf 'source_snapshot_initialization_failed=%s\n' "$destination" >&2
        return 1
    fi
    for source_dir in src tests std; do
        if [ ! -d "$source_dir" ]; then
            printf 'missing_source_directory=%s\n' "$source_dir" >> "$errors_file"
            status=1
            continue
        fi
        if ! find "$source_dir" \( -type f -o -type l \) -print0 >> "$paths" 2>> "$errors_file"; then
            printf 'source_enumeration_failed=%s\n' "$source_dir" >> "$errors_file"
            status=1
        fi
    done
    if [ ! -f Cargo.toml ]; then
        printf 'required_source_missing=Cargo.toml\n' >> "$errors_file"
        status=1
    else
        if ! printf '%s\0' Cargo.toml >> "$paths"; then
            printf 'source_path_write_failed=Cargo.toml\n' >> "$errors_file"
            status=1
        fi
    fi
    for input in Cargo.lock build.rs rust-toolchain rust-toolchain.toml .cargo/config .cargo/config.toml; do
        if [ -e "$input" ] && [ ! -f "$input" ]; then
            printf 'optional_source_not_regular_file=%s\n' "$input" >> "$errors_file"
            status=1
        elif [ -f "$input" ]; then
            if ! printf '%s\0' "$input" >> "$paths"; then
                printf 'source_path_write_failed=%s\n' "$input" >> "$errors_file"
                status=1
            fi
        fi
    done
    for input in \
        scripts/verify-ordinary-map-v2-zero-miss-i32-l3.sh; do
        if [ ! -f "$input" ]; then
            printf 'required_source_missing=%s\n' "$input" >> "$errors_file"
            status=1
        else
            if ! printf '%s\0' "$input" >> "$paths"; then
                printf 'source_path_write_failed=%s\n' "$input" >> "$errors_file"
                status=1
            fi
        fi
    done
    if ! sort -zu "$paths" -o "$sorted_paths" 2>> "$errors_file"; then
        printf 'source_path_sort_failed\n' >> "$errors_file"
        return 1
    fi
    if ! mapfile -d '' -t source_paths < "$sorted_paths"; then
        printf 'source_path_manifest_read_failed=%s\n' "$sorted_paths" >> "$errors_file"
        status=1
    fi
    if ! path_bytes="$(wc -c < "$sorted_paths")"; then
        printf 'source_path_manifest_size_failed=%s\n' "$sorted_paths" >> "$errors_file"
        status=1
    else
        path_bytes="${path_bytes//[[:space:]]/}"
        if [[ ! "$path_bytes" =~ ^[0-9]+$ ]]; then
            printf 'source_path_manifest_size_malformed=%s\n' "$sorted_paths" >> "$errors_file"
            status=1
        else
            for source_path in "${source_paths[@]}"; do
                observed_path_bytes=$((observed_path_bytes + ${#source_path} + 1))
            done
            if [[ "$observed_path_bytes" -ne "$path_bytes" ]]; then
                printf 'source_path_manifest_truncated=%s expected_bytes=%s observed_bytes=%s\n' \
                    "$sorted_paths" "$path_bytes" "$observed_path_bytes" >> "$errors_file"
                status=1
            fi
        fi
    fi
    for source_path in "${source_paths[@]}"; do
        source_absolute=''
        if ! source_absolute="$(realpath -m -- "$ROOT_DIR/$source_path" 2>> "$errors_file")"; then
            printf 'source_path_resolution_failed=%s\n' "$source_path" >> "$errors_file"
            status=1
            continue
        fi
        if [ ! -f "$source_absolute" ]; then
            printf 'source_disappeared_before_hash=%s\n' "$source_path" >> "$errors_file"
            status=1
        elif ! sha256sum -- "$source_path" >> "$destination" 2>> "$errors_file"; then
            printf 'source_hash_failed=%s\n' "$source_path" >> "$errors_file"
            status=1
        fi
    done
    return "$status"
}

record_tool() {
    local label="$1"
    local command_name="$2"
    shift 2
    local executable
    local directory="$artifact_dir/toolchain/$label"
    if [[ "$command_name" == /* ]]; then
        executable="$command_name"
    else
        executable="$(command -v -- "$command_name")"
    fi
    mkdir -- "$directory"
    printf '%q ' "$executable" "$@" > "$directory/command.txt"
    printf '\n' >> "$directory/command.txt"
    printf '%s\0' "$executable" "$@" > "$directory/command.argv.nul"
    if env "${runner_environment_unsets[@]}" \
        "$executable" "$@" > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$directory/process.status"
    return "$status"
}

cd "$ROOT_DIR"
snapshot_before_status=0
if ! source_snapshot_manifest \
    "$scratch/source-before.manifest" \
    "$artifact_dir/source-snapshot.before.errors.txt"; then
    snapshot_before_status=1
fi
snapshot_before=''
if manifest_hash_line="$(sha256sum -- "$scratch/source-before.manifest" 2>> "$artifact_dir/source-snapshot.before.errors.txt")"; then
    snapshot_before="${manifest_hash_line%% *}"
    if ! is_sha256 "$snapshot_before"; then
        snapshot_before_status=1
        printf 'manifest_hash_malformed=source-snapshot.before.manifest.sha256\n' >> "$artifact_dir/source-snapshot.before.errors.txt"
    fi
else
    snapshot_before_status=1
    printf 'manifest_hash_failed=source-snapshot.before.manifest.sha256\n' >> "$artifact_dir/source-snapshot.before.errors.txt"
fi
if [ -n "$artifact_dir" ]; then
    cp "$scratch/source-before.manifest" "$artifact_dir/source-snapshot.before.manifest.sha256"
    printf '%s\n' "$snapshot_before" > "$artifact_dir/source-snapshot.before.sha256"
    mkdir -- "$artifact_dir/toolchain"
    toolchain_status=0
    record_tool bash bash --version || toolchain_status=$?
    record_tool rustc rustc --version --verbose || toolchain_status=$?
    record_tool cargo cargo --version --verbose || toolchain_status=$?
    record_tool valgrind valgrind --version || toolchain_status=$?
    record_tool sha256sum sha256sum --version || toolchain_status=$?
    record_tool llvm-config "$LLVM_CONFIG_BIN" --version || toolchain_status=$?
    record_tool uname uname -a || toolchain_status=$?
    bash_bin="$(command -v bash)"
    rustc_bin="$(command -v rustc)"
    cargo_bin="$(command -v cargo)"
    valgrind_bin="$(command -v valgrind)"
    sha256sum_bin="$(command -v sha256sum)"
    uname_bin="$(command -v uname)"
    sha256sum -- "$bash_bin" "$rustc_bin" "$cargo_bin" "$valgrind_bin" \
        "$sha256sum_bin" "$LLVM_CONFIG_BIN" "$uname_bin" \
        > "$artifact_dir/toolchain/binaries.sha256"
    {
        printf 'bash_path=%s\nbash_version=' "$bash_bin"
        cat "$artifact_dir/toolchain/bash/stdout.bin"
        printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
        printf 'llvm_config_path=%s\nllvm_config_version=' "$LLVM_CONFIG_BIN"
        cat "$artifact_dir/toolchain/llvm-config/stdout.bin"
        printf 'rustc_path=%s\nrustc_version=' "$(command -v rustc)"
        cat "$artifact_dir/toolchain/rustc/stdout.bin"
        printf 'cargo_path=%s\ncargo_version=' "$(command -v cargo)"
        cat "$artifact_dir/toolchain/cargo/stdout.bin"
        printf 'valgrind_path=%s\nvalgrind_version=' "$(command -v valgrind)"
        cat "$artifact_dir/toolchain/valgrind/stdout.bin"
        printf 'sha256sum_path=%s\nsha256sum_version=' "$(command -v sha256sum)"
        cat "$artifact_dir/toolchain/sha256sum/stdout.bin"
        printf 'uname_path=%s\nplatform=' "$uname_bin"
        cat "$artifact_dir/toolchain/uname/stdout.bin"
    } > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"
    {
        printf 'cwd=%s\n' "$ROOT_DIR"
        for variable in PATH LLVM_SYS_181_PREFIX CARGO_BUILD_JOBS CARGO_HOME CARGO_TARGET_DIR \
            RUSTUP_TOOLCHAIN RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_TARGET \
            RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER LIBCLANG_PATH BINDGEN_EXTRA_CLANG_ARGS \
            CC CXX CFLAGS CXXFLAGS \
            LDFLAGS TMPDIR MIMI_ROUTE_CENSUS MIMI_VERBOSE MIMI_STDLIB \
            MIMI_ASAN MIMI_TEST_FORCE_NO_Z3 LD_PRELOAD LD_LIBRARY_PATH \
            MIMI_MAP_V2_ZERO_MISS_I32_L3_LOG VALGRIND_OPTS "$ARTIFACT_ENV"; do
            case "$variable" in
                CARGO_BUILD_JOBS) value=1 ;;
                MIMI_ROUTE_CENSUS) value=1 ;;
                MIMI_VERBOSE) value=1 ;;
                MIMI_STDLIB) value="$ROOT_DIR/std" ;;
                VALGRIND_OPTS) value='<unset>' ;;
                LLVM_SYS_181_PREFIX) value="$LLVM_SYS_181_PREFIX" ;;
                "$ARTIFACT_ENV") value="$artifact_dir" ;;
                *)
                    if [[ " ${CLEARED_BUILD_RUNTIME_ENV[*]} " == *" $variable "* ]]; then
                        value='<unset>'
                    elif [[ -v "$variable" ]]; then
                        value="${!variable}"
                    else
                        value='<unset>'
                    fi
                    ;;
            esac
            if [ "$variable" = VALGRIND_OPTS ] || \
                [[ " ${CLEARED_BUILD_RUNTIME_ENV[*]} " == *" $variable "* ]]; then
                printf '%s=<unset>\n' "$variable"
            else
                printf '%s=%q\n' "$variable" "$value"
            fi
        done
        for variable in "${CLEARED_BUILD_RUNTIME_ENV[@]}"; do
            printf '%s_inherited_provenance=%s\n' \
                "$variable" "${INITIAL_ENV_PROVENANCE[$variable]}"
        done
        printf 'cargo_test_environment_policy=build/runtime override variables listed above are unset; PATH HOME Cargo cache and selected LLVM/toolchain values remain recorded\n'
        printf 'native_process_environment_policy=build/runtime override variables listed above are unset for each captured child process\n'
        printf 'valgrind_environment_policy=VALGRIND_* and build/runtime override variables are unset; HOME is isolated per Memcheck command\n'
        for variable in "${CLEARED_BUILD_RUNTIME_ENV[@]}"; do
            printf 'runner_cleared_environment=%s\n' "$variable"
        done
        for variable in "${valgrind_environment_names[@]}"; do
            printf '%s_inherited_provenance=%s\n' \
                "$variable" "${INITIAL_ENV_PROVENANCE[$variable]}"
            if [ "$variable" != VALGRIND_OPTS ]; then
                printf '%s=<unset>\n' "$variable"
            fi
        done
    } > "$artifact_dir/runner-environment.txt"
    {
        printf 'branch='
        git branch --show-current
        printf 'head='
        git rev-parse HEAD
        printf 'script=%s\nfixture=%s\ntest_target=%s\ntest_name=%s\n' \
            "$ROOT_DIR/scripts/verify-ordinary-map-v2-zero-miss-i32-l3.sh" \
            "$ROOT_DIR/tests/fixtures/mir_ordinary_map_any_owner_v2_zero_miss_i32.mimi" \
            "$TEST_TARGET" "$TEST_NAME"
    } > "$artifact_dir/vcs-and-inputs.txt"
fi

cargo_arguments=(
    test --manifest-path "$ROOT_DIR/Cargo.toml"
    --test "$TEST_TARGET" "$TEST_NAME" --
    --exact --ignored --nocapture --test-threads=1
)
helper_test_names=(
    canonical_all_free_report_without_zero_class_rows_is_accepted
    full_zero_heap_summary_is_accepted
    malformed_duplicate_in_use_rows_are_rejected
    partial_leak_summary_set_is_rejected
    malformed_or_nonzero_suppressed_summary_is_rejected
    nonzero_suppressed_error_summary_is_rejected
    route_census_stderr_is_allowed
    typed_runtime_trap_and_cli_error_are_rejected
)
helper_arguments=(
    test --manifest-path "$ROOT_DIR/Cargo.toml"
    --test "$TEST_TARGET" -- --test-threads=1
)
helper_status=0
helper_validation_status=0
test_status=0
if [ -n "$artifact_dir" ]; then
    harness_directory="$artifact_dir/harness-tests"
    mkdir -- "$harness_directory"
    {
        printf 'env'
        for variable in "${CLEARED_BUILD_RUNTIME_ENV[@]}" "${valgrind_environment_names[@]}"; do
            printf ' -u %q' "$variable"
        done
        printf ' LLVM_SYS_181_PREFIX=%q MIMI_STDLIB=%q CARGO_BUILD_JOBS=1 MIMI_MAP_V2_ZERO_MISS_I32_L3_ARTIFACT_DIR=%q MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 %q ' \
            "$LLVM_SYS_181_PREFIX" "$MIMI_STDLIB" "$artifact_dir" "$(command -v cargo)"
        printf '%q ' "${helper_arguments[@]}"
        printf '\n'
    } > "$harness_directory/command.txt"
    printf '%s\0' "$(command -v cargo)" "${helper_arguments[@]}" > "$harness_directory/argv.nul"
    cp "$artifact_dir/runner-environment.txt" "$harness_directory/environment.txt"
    if [ "$toolchain_status" -ne 0 ]; then
        helper_status="$toolchain_status"
        : > "$harness_directory/stdout"
        printf 'toolchain version command failed; see toolchain captures\n' > "$harness_directory/stderr"
    elif env "${runner_environment_unsets[@]}" LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" MIMI_STDLIB="$MIMI_STDLIB" CARGO_BUILD_JOBS=1 MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
        MIMI_MAP_V2_ZERO_MISS_I32_L3_ARTIFACT_DIR="$artifact_dir" \
        cargo "${helper_arguments[@]}" > "$harness_directory/stdout" 2> "$harness_directory/stderr"; then
        helper_status=0
    else
        helper_status=$?
    fi
    printf '%s\n' "$helper_status" > "$harness_directory/exit"
    {
        printf '===== stdout =====\n'; cat "$harness_directory/stdout"
        printf '\n===== stderr =====\n'; cat "$harness_directory/stderr"
    } > "$harness_directory/log"
    cat "$harness_directory/stdout"
    cat "$harness_directory/stderr" >&2
    cp "$harness_directory/log" "$artifact_dir/harness-tests.log"
    if [ "$helper_status" -ne 0 ]; then
        helper_validation_status=1
    else
        helper_output="$harness_directory/stdout"
        grep -Eq '8 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out' "$helper_output" || helper_validation_status=1
        for helper_test in "${helper_test_names[@]}"; do
            [[ "$(grep -Ec "^test .*::${helper_test} \.\.\. ok$" "$helper_output" || true)" == 1 ]] || helper_validation_status=1
        done
        [[ "$(grep -Ec "^test ${TEST_NAME} \.\.\. ignored" "$helper_output" || true)" == 1 ]] || helper_validation_status=1
    fi
    printf 'helper_validation_status=%s\n' "$helper_validation_status" > "$harness_directory/validation.txt"

    {
        printf 'env'
        for variable in "${CLEARED_BUILD_RUNTIME_ENV[@]}" "${valgrind_environment_names[@]}"; do
            printf ' -u %q' "$variable"
        done
        printf ' LLVM_SYS_181_PREFIX=%q MIMI_STDLIB=%q CARGO_BUILD_JOBS=1 MIMI_MAP_V2_ZERO_MISS_I32_L3_ARTIFACT_DIR=%q MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 %q ' \
            "$LLVM_SYS_181_PREFIX" "$MIMI_STDLIB" "$artifact_dir" "$(command -v cargo)"
        printf '%q ' "${cargo_arguments[@]}"
        printf '\n'
    } > "$artifact_dir/suite.command.txt"
    printf '%s\0' "$(command -v cargo)" "${cargo_arguments[@]}" > "$artifact_dir/suite.argv.nul"
    cp "$artifact_dir/runner-environment.txt" "$artifact_dir/suite.environment.txt"
    if [ "$toolchain_status" -ne 0 ]; then
        test_status="$toolchain_status"
        : > "$artifact_dir/suite.stdout"
        printf 'toolchain version command failed; see toolchain captures\n' > "$artifact_dir/suite.stderr"
    elif env "${runner_environment_unsets[@]}" LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" MIMI_STDLIB="$MIMI_STDLIB" CARGO_BUILD_JOBS=1 MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
        MIMI_MAP_V2_ZERO_MISS_I32_L3_ARTIFACT_DIR="$artifact_dir" \
        cargo "${cargo_arguments[@]}" > "$artifact_dir/suite.stdout" 2> "$artifact_dir/suite.stderr"; then
        test_status=0
    else
        test_status=$?
    fi
    printf '%s\n' "$test_status" > "$artifact_dir/suite.exit"
    {
        printf '===== stdout =====\n'; cat "$artifact_dir/suite.stdout"
        printf '\n===== stderr =====\n'; cat "$artifact_dir/suite.stderr"
    } > "$artifact_dir/suite.log"
    cat "$artifact_dir/suite.stdout"
    cat "$artifact_dir/suite.stderr" >&2
    cp "$artifact_dir/suite.log" "$log_file"
else
    if env "${runner_environment_unsets[@]}" -u MIMI_MAP_V2_ZERO_MISS_I32_L3_ARTIFACT_DIR \
        LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" MIMI_STDLIB="$MIMI_STDLIB" CARGO_BUILD_JOBS=1 MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
        cargo "${cargo_arguments[@]}" 2>&1 | tee "$log_file"; then
        test_status=0
    else
        test_status=$?
    fi
fi

snapshot_after_status=0
if ! source_snapshot_manifest \
    "$scratch/source-after.manifest" \
    "$artifact_dir/source-snapshot.after.errors.txt"; then
    snapshot_after_status=1
fi
snapshot_after=''
if manifest_hash_line="$(sha256sum -- "$scratch/source-after.manifest" 2>> "$artifact_dir/source-snapshot.after.errors.txt")"; then
    snapshot_after="${manifest_hash_line%% *}"
    if ! is_sha256 "$snapshot_after"; then
        snapshot_after_status=1
        printf 'manifest_hash_malformed=source-snapshot.after.manifest.sha256\n' >> "$artifact_dir/source-snapshot.after.errors.txt"
    fi
else
    snapshot_after_status=1
    printf 'manifest_hash_failed=source-snapshot.after.manifest.sha256\n' >> "$artifact_dir/source-snapshot.after.errors.txt"
fi
printf 'source_snapshot_sha256=%s\nsource_snapshot_sha256_after=%s\n' "$snapshot_before" "$snapshot_after"
snapshot_status=0
if [ "$snapshot_before_status" -ne 0 ] || [ "$snapshot_after_status" -ne 0 ]; then
    printf 'source enumeration or hashing failed; partial evidence is retained in source-snapshot.*.errors.txt\n' >&2
    snapshot_status=98
elif ! cmp -s "$scratch/source-before.manifest" "$scratch/source-after.manifest"; then
    printf 'source snapshot changed during the ZeroMissI32 strict L3 run\n' >&2
    snapshot_status=98
fi

validation_status=0
if [ -n "$artifact_dir" ]; then
    validation_status="$helper_validation_status"
    cp "$scratch/source-after.manifest" "$artifact_dir/source-snapshot.after.manifest.sha256"
    printf '%s\n' "$snapshot_after" > "$artifact_dir/source-snapshot.after.sha256"
    if [ "$test_status" -eq 0 ]; then
        summary="$artifact_dir/native/positive/run-summary.txt"
        if [ ! -s "$summary" ]; then
            printf 'successful ZeroMissI32 L3 test left no positive run summary\n' >&2
            validation_status=1
        else
            expected_summary=(
                "route_profile=map-any-owner-v2-zero-miss-i32-v1"
                'global_map_any_v2_production_ready=false'
                'case_count=1'
                'case_labels=zero-miss-i32'
                'cli_receipt_count=1'
                'route_admission_count=6'
                'route_admission_paths=default-run,explicit-mir-run,default-build,explicit-mir-build,default-verify,explicit-mir-verify'
                'default_run_count=1'
                'explicit_mir_run_count=1'
                'native_build_count=2'
                'memcheck_run_count=2'
                'default_native_build_receipt_count=1'
                'explicit_native_build_receipt_count=1'
                'default_verify_count=1'
                'explicit_mir_verify_count=1'
                'negative_case_count=1'
                'negative_receipt_attempt_count=1'
                'negative_mir_receipt_success_count=0'
                'negative_checker_inspection_receipt_count=1'
                'negative_receipt_catalog_rejection_count=0'
                'negative_route_attempt_count=6'
                'legacy_fallback_count=0'
                'expected_stdout_hex=747275650a'
            )
            for expected in "${expected_summary[@]}"; do
                grep -Fxq "$expected" "$summary" || validation_status=1
                key="${expected%%=*}"
                [[ "$(grep -Ec "^${key}=" "$summary")" -eq 1 ]] || validation_status=1
            done

            case_directory="$artifact_dir/native/positive/zero-miss-i32"
            case_summary="$case_directory/run-summary.txt"
            fixture_path="$ROOT_DIR/tests/fixtures/mir_ordinary_map_any_owner_v2_zero_miss_i32.mimi"
            [ -s "$case_summary" ] || validation_status=1
            for expected in \
                'case_label=zero-miss-i32' \
                "fixture_path=$fixture_path" \
                'route_profile=map-any-owner-v2-zero-miss-i32-v1' \
                'route_admission_count=6' \
                'route_admission_paths=default-run,explicit-mir-run,default-build,explicit-mir-build,default-verify,explicit-mir-verify' \
                'native_build_count=2' \
                'memcheck_run_count=2' \
                'global_map_any_v2_production_ready=false' \
                'cli_receipt_profile=cli-mir-v1' \
                'case_count=1' \
                'cli_receipt_count=1' \
                'default_run_count=1' \
                'explicit_mir_run_count=1' \
                'default_native_build_receipt_count=1' \
                'explicit_native_build_receipt_count=1' \
                'default_verify_count=1' \
                'explicit_mir_verify_count=1' \
                'legacy_fallback_count=0' \
                'expected_stdout_hex=747275650a'; do
                grep -Fxq "$expected" "$case_summary" || validation_status=1
                key="${expected%%=*}"
                [[ "$(grep -Ec "^${key}=" "$case_summary")" -eq 1 ]] || validation_status=1
            done
            cmp -s "$fixture_path" "$case_directory/source.fixture.mimi" || validation_status=1

            for capture in command.argv.json command.environment.txt stdout.bin stderr.bin process.status; do
                [[ "$(find "$case_directory" -type f -name "$capture" | wc -l)" == 12 ]] || validation_status=1
            done

            receipt_manifest="$case_directory/receipt/stdout.bin"
            for pair in 'schema receipt_schema' 'mir_digest mir_digest' \
                'type_desc_digest type_desc_digest' 'abi_digest abi_digest' \
                'ffi_digest ffi_digest' 'ownership_digest ownership_digest' \
                'flow_transition_digest flow_transition_digest' 'root_owners root_owners'; do
                read -r receipt_field summary_field <<< "$pair"
                receipt_value="$(sed -n "s/^${receipt_field}=//p" "$receipt_manifest")"
                [ -n "$receipt_value" ] || { validation_status=1; continue; }
                grep -Fxq "${summary_field}=${receipt_value}" "$case_summary" || validation_status=1
            done
            for expected in source_sha256 verifier_source_hash_blake3; do
                [[ "$(grep -Ec "^${expected}=" "$case_summary")" -eq 1 ]] || validation_status=1
            done
            source_hash_manifest="$case_directory/source-hash.expected.txt"
            [ -s "$source_hash_manifest" ] || validation_status=1
            grep -Fxq 'verifier_source_hash_algorithm=blake3' "$source_hash_manifest" || validation_status=1
            for pair in 'fixture_source_sha256 source_sha256' \
                'verifier_source_hash verifier_source_hash_blake3'; do
                read -r expected_field summary_field <<< "$pair"
                expected_value="$(sed -n "s/^${expected_field}=//p" "$source_hash_manifest")"
                [ -n "$expected_value" ] || { validation_status=1; continue; }
                grep -Fxq "${summary_field}=${expected_value}" "$case_summary" || validation_status=1
            done

            [[ "$(find "$case_directory" -type f -name program | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$case_directory" -type f -name valgrind.log | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$case_directory" -type f -name memcheck-summary.txt | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native/positive" -type f -name program | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native/positive" -type f -name valgrind.log | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native/positive" -type f -name memcheck-summary.txt | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name program ! -perm -111 | wc -l)" == 0 ]] || validation_status=1

            expected_stdout="$case_directory/default-memcheck/expected-native-stdout.bin"
            [ -s "$expected_stdout" ] || validation_status=1
            printf 'true\n' > "$scratch/expected-stdout.bin"
            cmp -s "$scratch/expected-stdout.bin" "$expected_stdout" || validation_status=1
            for mode in default-run explicit-run default-memcheck explicit-memcheck; do
                cmp -s "$expected_stdout" "$case_directory/$mode/stdout.bin" || validation_status=1
            done
            for mode in default-run explicit-run default-memcheck explicit-memcheck; do
                status_file="$case_directory/$mode/process.status"
                [ -s "$status_file" ] || { validation_status=1; continue; }
                [[ "$(grep -Ec '^exit_code=' "$status_file" || true)" == 1 ]] || validation_status=1
                [[ "$(grep -Ec '^success=' "$status_file" || true)" == 1 ]] || validation_status=1
                grep -Fxq 'exit_code=1' "$status_file" || validation_status=1
                grep -Fxq 'success=false' "$status_file" || validation_status=1
            done
            for mode in default-build explicit-build default-verify explicit-verify receipt; do
                status_file="$case_directory/$mode/process.status"
                [ -s "$status_file" ] || { validation_status=1; continue; }
                [[ "$(grep -Ec '^exit_code=' "$status_file" || true)" == 1 ]] || validation_status=1
                [[ "$(grep -Ec '^success=' "$status_file" || true)" == 1 ]] || validation_status=1
                grep -Fxq 'exit_code=0' "$status_file" || validation_status=1
                grep -Fxq 'success=true' "$status_file" || validation_status=1
            done
            for mode in default-memcheck explicit-memcheck; do
                grep -Fxq 'expected_exit_code=1' \
                    "$case_directory/$mode/expected-native-result.txt" || validation_status=1
                grep -Fxq 'expected_stdout_bytes=5' \
                    "$case_directory/$mode/expected-native-result.txt" || validation_status=1
            done
            for mode in default-memcheck explicit-memcheck; do
                report="$case_directory/$mode/memcheck-summary.txt"
                [ ! -s "$case_directory/$mode/stderr.bin" ] || validation_status=1
                [ -s "$report" ] || { validation_status=1; continue; }
                for expected in \
                    'in use at exit: 0 bytes in 0 blocks' \
                    'All heap blocks were freed -- no leaks are possible' \
                    'definitely lost: 0 bytes in 0 blocks' \
                    'indirectly lost: 0 bytes in 0 blocks' \
                    'possibly lost: 0 bytes in 0 blocks' \
                    'still reachable: 0 bytes in 0 blocks'; do
                    grep -Fq "$expected" "$report" || validation_status=1
                done
                [[ "$(grep -Fxc 'ERROR SUMMARY: 0 errors' "$report")" == 1 ]] || validation_status=1
                [[ "$(grep -Fxc 'ERROR SUPPRESSION SUMMARY: 0 from 0' "$report")" == 1 ]] || validation_status=1
                if grep -Fq 'suppressed:' "$report"; then
                    grep -Fq 'suppressed: 0 bytes in 0 blocks' "$report" || validation_status=1
                fi
            done

            digest="$(sed -n 's/^mir_digest=//p' "$receipt_manifest")"
            identity="$(sed -n 's/^route_receipt_identity_digest=//p' "$case_summary")"
            [[ "$digest" =~ ^[0-9a-f]{64}$ && "$identity" =~ ^[0-9a-f]{64}$ ]] || validation_status=1
            for mode in default-run explicit-run default-build explicit-build default-verify explicit-verify; do
                stderr="$case_directory/$mode/stderr.bin"
                grep -Fq "checker_profile=map-any-owner-v2-zero-miss-i32-v1" "$stderr" || validation_status=1
                grep -Fq "mir_digest=$digest" "$stderr" || validation_status=1
                grep -Fq "receipt_identity_digest=$identity" "$stderr" || validation_status=1
                grep -Fq 'canonical route disposition: legacy' "$stderr" && validation_status=1
            done
            for mode in default-run explicit-run; do
                if grep -aEiq '\[E080[0-9]+\]|error:|error\[|runtime error|runtime trap|runtime failure|panic:|panicked at' \
                    "$case_directory/$mode/stderr.bin"; then
                    validation_status=1
                fi
            done
            grep -Fq "canonical route disposition: canonical (map-any-owner-v2-zero-miss-i32-v1) mir_digest=$digest" \
                "$case_directory/default-run/stderr.bin" || validation_status=1
            grep -Fq "canonical route disposition: explicit canonical (map-any-owner-v2-zero-miss-i32-v1) mir_digest=$digest" \
                "$case_directory/explicit-run/stderr.bin" || validation_status=1
            for mode in default-build explicit-build; do
                grep -Fq 'MIMI_ROUTE_CENSUS canonical-native-build ' "$case_directory/$mode/stderr.bin" || validation_status=1
                grep -Fq "profile=map-any-owner-v2-zero-miss-i32-v1" "$case_directory/$mode/stderr.bin" || validation_status=1
                grep -Fq "mir_digest=$digest" "$case_directory/$mode/stderr.bin" || validation_status=1
                grep -Fq "route_identity_digest=$identity" "$case_directory/$mode/stderr.bin" || validation_status=1
                grep -Fq 'info: canonical MIR native compiled ' "$case_directory/$mode/stderr.bin" || validation_status=1
            done
            for mode in default-verify explicit-verify; do
                grep -Fq 'canonical MIR verifier provenance: ' "$case_directory/$mode/stderr.bin" || validation_status=1
                grep -Fq "profile=map-any-owner-v2-zero-miss-i32-v1" "$case_directory/$mode/stderr.bin" || validation_status=1
                grep -Fq "mir_digest=$digest" "$case_directory/$mode/stderr.bin" || validation_status=1
                grep -Fq "route_identity_digest=$identity" "$case_directory/$mode/stderr.bin" || validation_status=1
            done

            negative_directory="$artifact_dir/native/negative/wrong-miss-unpack-type"
            cmp -s "$negative_directory/source.fixture.mimi" \
                "$artifact_dir/native/negative-inputs/wrong-miss-unpack-type.mimi" || validation_status=1
            negative_receipt="$negative_directory/receipt"
            for capture in command.argv.json command.environment.txt stdout.bin stderr.bin process.status; do
                [[ "$(find "$negative_directory" -type f -name "$capture" | wc -l)" == 7 ]] || validation_status=1
            done
            for capture in command.argv.json command.environment.txt stdout.bin stderr.bin process.status; do
                [[ "$(find "$artifact_dir/native" -type f -name "$capture" | wc -l)" == 21 ]] || validation_status=1
            done
            [[ "$(grep -Ec '^exit_code=' "$negative_receipt/process.status" || true)" == 1 ]] || validation_status=1
            [[ "$(grep -Ec '^success=' "$negative_receipt/process.status" || true)" == 1 ]] || validation_status=1
            grep -Fxq 'exit_code=0' "$negative_receipt/process.status" || validation_status=1
            grep -Fxq 'success=true' "$negative_receipt/process.status" || validation_status=1
            [[ "$(wc -l < "$negative_receipt/stdout.bin")" -eq 8 ]] || validation_status=1
            grep -Fxq "$CHECKER_INSPECTION_HEADER" "$negative_receipt/stdout.bin" || validation_status=1
            grep -Fxq "schema=$CHECKER_INSPECTION_SCHEMA" "$negative_receipt/stdout.bin" || validation_status=1
            grep -Fxq "profile=$CHECKER_INSPECTION_PROFILE" "$negative_receipt/stdout.bin" || validation_status=1
            grep -Fxq 'snapshot_kind=checked-program-only' "$negative_receipt/stdout.bin" || validation_status=1
            grep -Fxq 'mir_materialized=false' "$negative_receipt/stdout.bin" || validation_status=1
            grep -Eq '^source_digest=[0-9a-f]{64}$' "$negative_receipt/stdout.bin" || validation_status=1
            grep -Fxq 'map_any_owner_v2_admission=incomplete_coverage' "$negative_receipt/stdout.bin" || validation_status=1
            grep -Eq '^root_owners=.*function:main' "$negative_receipt/stdout.bin" || validation_status=1
            grep -Eq '^mir_digest=' "$negative_receipt/stdout.bin" && validation_status=1
            grep -Fq 'emitted a read-only Checker inspection receipt for incomplete Map/Any owner-v2 coverage; MIR was not materialized' \
                "$negative_receipt/stderr.bin" || validation_status=1
            grep -Fq 'MIR_ROUTE_CENSUS route-admission ' "$negative_receipt/stderr.bin" && validation_status=1
            grep -Fq "$MIR_RECEIPT_HEADER" "$negative_receipt/stdout.bin" && validation_status=1
            grep -Fxq 'receipt_outcome=checker-inspection-no-mir-materialized' \
                "$negative_directory/receipt-outcome.txt" || validation_status=1
            grep -Fxq 'receipt_exit_code=0' "$negative_directory/receipt-outcome.txt" || validation_status=1
            grep -Fxq 'mir_materialized=false' "$negative_directory/receipt-outcome.txt" || validation_status=1
            for mode in default-run explicit-mir-run default-build explicit-mir-build \
                default-verify explicit-mir-verify; do
                status_file="$negative_directory/$mode/process.status"
                [ -s "$status_file" ] || { validation_status=1; continue; }
                [[ "$(grep -Ec '^exit_code=' "$status_file" || true)" == 1 ]] || validation_status=1
                [[ "$(grep -Ec '^success=' "$status_file" || true)" == 1 ]] || validation_status=1
                grep -Fxq 'exit_code=1' "$status_file" || validation_status=1
                grep -Fxq 'success=false' "$status_file" || validation_status=1
                output="$negative_directory/$mode/stdout.bin"
                stderr="$negative_directory/$mode/stderr.bin"
                if cmp -s "$output" "$expected_stdout"; then
                    validation_status=1
                fi
                case "$mode" in
                    *-run|*-verify) [ ! -s "$output" ] || validation_status=1 ;;
                esac
                diagnostic_file="$scratch/$mode-negative-diagnostic.txt"
                cat "$output" "$stderr" > "$diagnostic_file"
                grep -Fq 'MIR-COVERAGE-001' "$diagnostic_file" || validation_status=1
                for forbidden in \
                    'canonical route disposition: legacy' \
                    'canonical route disposition: canonical (' \
                    'canonical route disposition: explicit canonical (' \
                    'MIMI_ROUTE_CENSUS route-admission ' \
                    'info: canonical MIR native compiled ' \
                    'MIMI_ROUTE_CENSUS canonical-native-build ' \
                    'canonical MIR verifier provenance: '; do
                    grep -Fq "$forbidden" "$stderr" && validation_status=1
                done
            done
            [[ "$(find "$negative_directory" -type f -name program | wc -l)" == 0 ]] || validation_status=1
        fi
    fi
    if [ "$snapshot_status" -ne 0 ]; then
        printf 'FAIL source snapshot changed\n' > "$artifact_dir/artifact-validation.txt"
    elif [ "$test_status" -ne 0 ]; then
        printf 'INCOMPLETE test exit=%s; partial evidence retained\n' "$test_status" > "$artifact_dir/artifact-validation.txt"
    elif [ "$validation_status" -eq 0 ]; then
        pass_marker_pending=1
        {
            printf 'validation=PASS\nprofile=ZeroMissI32\nlevel=L3\nglobal_map_any_v2_production_ready=false\n'
            printf 'test_exit=%s\nsource_snapshot_sha256_before=%s\nsource_snapshot_sha256_after=%s\n' \
                "$test_status" "$snapshot_before" "$snapshot_after"
            printf 'summary=%s\nartifact_hash_manifest=%s\n' \
                "$artifact_dir/native/positive/run-summary.txt" "$artifact_dir/files.sha256"
            printf 'captured_toolchain=%s\nrunner_environment=%s\n' \
                "$artifact_dir/toolchain.txt" "$artifact_dir/runner-environment.txt"
        } > "$scratch/artifact-validation.pass.txt"
    else
        printf 'FAIL required ZeroMissI32 L3 artifact validation\n' > "$artifact_dir/artifact-validation.txt"
        test_status=97
    fi
    pass_marker_pending="${pass_marker_pending:-0}"
    hash_status=0
    if ! : > "$artifact_dir/files.sha256.errors.txt"; then
        hash_status=1
    elif (
        cd "$artifact_dir" || exit 1
        # The final status and hash diagnostics are excluded from the sealed file list.
        find . -type f ! -name files.sha256 ! -name files.sha256.sha256 \
            ! -path './artifact-validation.txt' ! -path './files.sha256.errors.txt' \
            -print0 2>> files.sha256.errors.txt \
            | sort -z 2>> files.sha256.errors.txt \
            | xargs -0 -r sha256sum > files.sha256 2>> files.sha256.errors.txt || exit 1
        sha256sum --check files.sha256 > /dev/null 2>> files.sha256.errors.txt || exit 1
        sha256sum files.sha256 > files.sha256.sha256 2>> files.sha256.errors.txt || exit 1
        sha256sum --check files.sha256.sha256 > /dev/null 2>> files.sha256.errors.txt || exit 1
    ); then
        if [[ "$pass_marker_pending" -eq 1 ]] && \
            ! cp -- "$scratch/artifact-validation.pass.txt" "$artifact_dir/artifact-validation.txt"; then
            printf 'FAIL unable to publish final status after artifact hash validation\n' \
                > "$artifact_dir/artifact-validation.txt"
            test_status=97
        fi
    else
        hash_status=$?
    fi
    if [[ "$hash_status" -ne 0 ]]; then
        if [ -f "$artifact_dir/artifact-validation.txt" ]; then
            printf 'artifact_hash_manifest=FAIL\n' >> "$artifact_dir/artifact-validation.txt"
        else
            printf 'FAIL artifact hash manifest generation/check failed\n' \
                > "$artifact_dir/artifact-validation.txt"
        fi
        if [ "$test_status" -eq 0 ]; then
            test_status=97
        fi
    fi
fi

printf 'acceptance_test_exit=%s\n' "$test_status"
printf 'artifact_dir=%s\n' "$artifact_dir"
[ "$snapshot_status" -eq 0 ] || exit "$snapshot_status"
exit "$test_status"
