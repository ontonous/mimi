#!/usr/bin/env bash
set -euo pipefail
set -o noclobber
while IFS= read -r imported_function; do
    builtin unset -f "$imported_function"
done < <(builtin compgen -A function)

script_path="${BASH_SOURCE[0]}"
script_dir="${script_path%/*}"
[[ "$script_dir" != "$script_path" ]] || script_dir=.
ROOT_DIR="$(cd -- "$script_dir/.." && pwd -P)"
if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper; fi
ARTIFACT_ENV=MIMI_MAP_V2_CHECKED_GET_L3_ARTIFACT_DIR
TARGET_DIR_ENV=MIMI_MAP_V2_CHECKED_GET_CARGO_TARGET_DIR
TEST_TARGET=canonical_mir_ordinary_map_any_v2_checked_get_memcheck_l3
TEST_NAME=ordinary_map_owned_any_v2_checked_get_native_builds_are_strict_memcheck_clean

inherited_path="${PATH:-/usr/bin:/bin}"
system_path=/usr/bin:/bin
[ -x /usr/bin/realpath ] || { printf 'CheckedGet strict L3 requires /usr/bin/realpath\n' >&2; exit 1; }

find_elf_tool() {
    local name="$1" candidate absolute_dir invoked real magic
    PATH="$inherited_path"; export PATH
    candidate="$(type -P -- "$name" || true)"
    [ -n "$candidate" ] || { printf 'CheckedGet strict L3 requires executable %s\n' "$name" >&2; return 1; }
    if [[ "$candidate" == */* ]]; then absolute_dir="${candidate%/*}"; else absolute_dir=.; fi
    [ -n "$absolute_dir" ] || absolute_dir=/
    absolute_dir="$(cd -- "$absolute_dir" && pwd -P)"
    invoked="$absolute_dir/${candidate##*/}"
    [ -f "$invoked" ] && [ -x "$invoked" ] || { printf 'CheckedGet strict L3 tool is not executable: %s\n' "$invoked" >&2; return 1; }
    IFS= read -r -N 4 magic < "$invoked" || true
    [[ "$magic" == $'\177ELF' ]] || { printf 'CheckedGet strict L3 refuses non-ELF tool (possible PATH script shim): %s\n' "$invoked" >&2; return 1; }
    real="$(/usr/bin/realpath -- "$invoked")"
    [ -f "$real" ] && [ -x "$real" ] || { printf 'CheckedGet strict L3 tool target is not executable: %s\n' "$real" >&2; return 1; }
    IFS= read -r -N 4 magic < "$real" || true
    [[ "$magic" == $'\177ELF' ]] || { printf 'CheckedGet strict L3 refuses non-ELF tool target: %s\n' "$real" >&2; return 1; }
    printf '%s\n%s\n' "$invoked" "$real"
}

find_valgrind_tool() {
    local candidate absolute_dir invoked real magic line
    local -a body expected
    PATH="$inherited_path"; export PATH
    candidate="$(type -P -- valgrind || true)"
    [ -n "$candidate" ] || { printf 'CheckedGet strict L3 requires executable valgrind\n' >&2; return 1; }
    if [[ "$candidate" == */* ]]; then absolute_dir="${candidate%/*}"; else absolute_dir=.; fi
    [ -n "$absolute_dir" ] || absolute_dir=/
    absolute_dir="$(cd -- "$absolute_dir" && pwd -P)"
    invoked="$absolute_dir/${candidate##*/}"
    [ -f "$invoked" ] && [ -x "$invoked" ] || { printf 'Valgrind is not an executable file: %s\n' "$invoked" >&2; return 1; }
    real="$(/usr/bin/realpath -- "$invoked")"
    IFS= read -r -N 4 magic < "$real" || true
    if [[ "$magic" == $'\177ELF' ]]; then
        printf '%s\n%s\n%s\n' "$invoked" "$real" "$real"
        return 0
    fi
    IFS= read -r line < "$real"
    [ "$line" = '#!/bin/sh -e' ] || { printf 'Refusing unrecognized Valgrind script shim: %s\n' "$real" >&2; return 1; }
    expected=(
        'DBGPATH=/usr/lib/debug'
        'if [ -z "$LD_LIBRARY_PATH" ]; then'
        'export LD_LIBRARY_PATH="$DBGPATH"'
        'else'
        'export LD_LIBRARY_PATH="$LD_LIBRARY_PATH:$DBGPATH"'
        'fi'
        'export GLIBCPP_FORCE_NEW=1'
        'export GLIBCXX_FORCE_NEW=1'
        'exec $0.bin "$@"'
    )
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line#"${line%%[![:space:]]*}"}"
        line="${line%"${line##*[![:space:]]}"}"
        [[ -z "$line" || "$line" == \#* || "$line" == '#!/bin/sh -e' ]] && continue
        body+=("$line")
    done < "$real"
    [ "${#body[@]}" = "${#expected[@]}" ] || { printf 'Refusing unrecognized Valgrind script shim: %s\n' "$real" >&2; return 1; }
    for index in "${!expected[@]}"; do
        [ "${body[$index]}" = "${expected[$index]}" ] || { printf 'Refusing unrecognized Valgrind script shim: %s\n' "$real" >&2; return 1; }
    done
    local companion="$(/usr/bin/realpath -- "$real.bin")"
    [ -f "$companion" ] && [ -x "$companion" ] || { printf 'Valgrind wrapper has no executable sibling binary: %s.bin\n' "$real" >&2; return 1; }
    IFS= read -r -N 4 magic < "$companion" || true
    [[ "$magic" == $'\177ELF' ]] || { printf 'Valgrind wrapper sibling is not ELF: %s\n' "$companion" >&2; return 1; }
    printf '%s\n%s\n%s\n' "$invoked" "$real" "$companion"
}

mapfile -t cargo_paths < <(find_elf_tool cargo)
mapfile -t rustc_paths < <(find_elf_tool rustc)
mapfile -t valgrind_paths < <(find_valgrind_tool)
mapfile -t sha256sum_paths < <(find_elf_tool sha256sum)
[ "${#cargo_paths[@]}" = 2 ] && [ "${#rustc_paths[@]}" = 2 ] && [ "${#valgrind_paths[@]}" = 3 ] && [ "${#sha256sum_paths[@]}" = 2 ] || exit 1
CARGO_PATH="${cargo_paths[0]}"; CARGO_REAL_PATH="${cargo_paths[1]}"
RUSTC_PATH="${rustc_paths[0]}"; RUSTC_REAL_PATH="${rustc_paths[1]}"
VALGRIND_PATH="${valgrind_paths[0]}"; VALGRIND_REAL_PATH="${valgrind_paths[1]}"
VALGRIND_BINARY_PATH="${valgrind_paths[2]}"
SHA256SUM_PATH="${sha256sum_paths[0]}"; SHA256SUM_REAL_PATH="${sha256sum_paths[1]}"
PATH="$system_path"
export PATH
unset RUSTUP_TOOLCHAIN RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER RUSTFLAGS RUSTDOCFLAGS \
    CARGO_BUILD_TARGET CARGO_BUILD_RUSTC CARGO_BUILD_RUSTC_WRAPPER CARGO_TARGET_DIR \
    CARGO_ENCODED_RUSTFLAGS
[ -d "$LLVM_SYS_181_PREFIX" ] || { printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2; exit 1; }

artifact_dir=""
if [[ ! -v MIMI_MAP_V2_CHECKED_GET_L3_ARTIFACT_DIR || -z "$MIMI_MAP_V2_CHECKED_GET_L3_ARTIFACT_DIR" ]]; then
    printf 'CheckedGet strict L3 requires MIMI_MAP_V2_CHECKED_GET_L3_ARTIFACT_DIR so raw evidence is retained\n' >&2
    exit 2
fi
requested="$MIMI_MAP_V2_CHECKED_GET_L3_ARTIFACT_DIR"
case "$requested" in /*) ;; *) requested="$ROOT_DIR/$requested" ;; esac
if [ -e "$requested" ] || [ -L "$requested" ]; then
    printf 'refusing to overwrite existing CheckedGet L3 artifact path: %s\n' "$requested" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/scripts/"*|"$ROOT_DIR/target/"*)
        printf 'CheckedGet L3 artifacts must be outside source/build trees: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
export MIMI_MAP_V2_CHECKED_GET_L3_ARTIFACT_DIR="$artifact_dir"

if [[ -v MIMI_MAP_V2_CHECKED_GET_L3_LOG && -n "$MIMI_MAP_V2_CHECKED_GET_L3_LOG" ]]; then
    log_file="$MIMI_MAP_V2_CHECKED_GET_L3_LOG"
else
    log_file="/tmp/mimi-map-v2-checked-get-l3-$$.log"
fi
case "$log_file" in /*) ;; *) log_file="$ROOT_DIR/$log_file" ;; esac
if [ -e "$log_file" ] || [ -L "$log_file" ]; then
    printf 'refusing to overwrite existing CheckedGet L3 log: %s\n' "$log_file" >&2
    exit 2
fi
mkdir -p -- "$(dirname "$log_file")"
log_file_absolute="$(realpath -m -- "$log_file")"
if [ -n "$artifact_dir" ] && [[ "$log_file_absolute" == "$artifact_dir/"* ]]; then
    printf 'CheckedGet L3 suite log must be outside the artifact directory: %s\n' "$log_file_absolute" >&2
    exit 2
fi
scratch="$(mktemp -d /tmp/mimi-map-v2-checked-get-l3.XXXXXX)"
trap 'rm -rf -- "$scratch"' EXIT
if [[ -v "$TARGET_DIR_ENV" && -n "${!TARGET_DIR_ENV}" ]]; then
    target_dir_requested="${!TARGET_DIR_ENV}"
    case "$target_dir_requested" in
        /*) ;;
        *) printf '%s must be an absolute Cargo target directory: %s\n' "$TARGET_DIR_ENV" "$target_dir_requested" >&2; exit 2 ;;
    esac
    cargo_target_dir="$(realpath -m -- "$target_dir_requested")"
    if [[ "$cargo_target_dir" != "$target_dir_requested" ]]; then
        printf '%s must already be the physical canonical path: %s\n' "$TARGET_DIR_ENV" "$target_dir_requested" >&2
        exit 2
    fi
    case "$cargo_target_dir/" in
        "$ROOT_DIR/"*) printf 'explicit CheckedGet L3 Cargo target must be outside the source tree: %s\n' "$cargo_target_dir" >&2; exit 2 ;;
    esac
    if [[ -L "$cargo_target_dir" ]]; then
        printf 'explicit CheckedGet L3 Cargo target must not be a symlink: %s\n' "$cargo_target_dir" >&2
        exit 2
    fi
    mkdir -p -- "$cargo_target_dir"
    if [[ ! -d "$cargo_target_dir" || -L "$cargo_target_dir" || "$(realpath -- "$cargo_target_dir")" != "$cargo_target_dir" ]]; then
        printf 'explicit CheckedGet L3 Cargo target must resolve to a regular physical directory: %s\n' "$cargo_target_dir" >&2
        exit 2
    fi
    cargo_target_dir_policy=explicit-external-physical-directory
else
    if [[ ! -d "$ROOT_DIR/target" || -L "$ROOT_DIR/target" ]]; then
        printf 'CheckedGet L3 requires the regular workspace Cargo target directory: %s/target\n' "$ROOT_DIR" >&2
        exit 2
    fi
    cargo_target_dir="$(realpath -- "$ROOT_DIR/target")"
    if [[ "$cargo_target_dir" != "$ROOT_DIR/target" ]]; then
        printf 'CheckedGet L3 refuses a redirected workspace Cargo target: %s\n' "$cargo_target_dir" >&2
        exit 2
    fi
    cargo_target_dir_policy=reuse-regular-workspace-target
    target_dir_requested='<unset>'
fi
case "$cargo_target_dir/" in
    "$artifact_dir/"*) printf 'CheckedGet L3 Cargo target must be outside the artifact directory: %s\n' "$cargo_target_dir" >&2; exit 2 ;;
esac
case "$artifact_dir/" in
    "$cargo_target_dir/"*) printf 'CheckedGet L3 artifacts must be outside the Cargo target directory: %s\n' "$artifact_dir" >&2; exit 2 ;;
esac
case "$log_file_absolute/" in
    "$cargo_target_dir/"*) printf 'CheckedGet L3 suite log must be outside the Cargo target directory: %s\n' "$log_file_absolute" >&2; exit 2 ;;
esac
[[ -n "$cargo_target_dir" && "$cargo_target_dir" = "$(realpath -- "$cargo_target_dir")" ]] || {
    printf 'CheckedGet L3 Cargo target must resolve to its recorded physical path: %s\n' "$cargo_target_dir" >&2
    exit 2
}
export CARGO_TARGET_DIR="$cargo_target_dir"
cache_cargo_home="${HOME:-/tmp}/.cargo"
[ -d "$cache_cargo_home" ] || { printf 'CheckedGet strict L3 requires the stable Cargo cache: %s\n' "$cache_cargo_home" >&2; exit 2; }
cache_cargo_home="$(realpath -- "$cache_cargo_home")"
for cargo_config in "$cache_cargo_home/config" "$cache_cargo_home/config.toml"; do
    if [ -e "$cargo_config" ] || [ -L "$cargo_config" ]; then
        printf 'CheckedGet strict L3 refuses Cargo home config: %s\n' "$cargo_config" >&2
        exit 2
    fi
done
config_scan_dir="$ROOT_DIR"
while :; do
    for cargo_config in "$config_scan_dir"/.cargo/config "$config_scan_dir"/.cargo/config.toml; do
        if [ -e "$cargo_config" ] || [ -L "$cargo_config" ]; then
            printf 'CheckedGet L3 refuses Cargo config on the workspace path: %s\n' "$cargo_config" >&2
            exit 2
        fi
    done
    [ "$config_scan_dir" = / ] && break
    config_scan_dir="${config_scan_dir%/*}"
    [ -n "$config_scan_dir" ] || config_scan_dir=/
done

path_is_within() {
    local candidate="$1" directory="$2"
    [[ -n "$directory" && ( "$candidate" == "$directory" || "$candidate" == "$directory"/* ) ]]
}

source_snapshot_manifest() {
    {
        find src tests std -type f -print0
        [ ! -d .cargo ] || find .cargo -type f -print0
        printf '%s\0' Cargo.toml
        [ ! -f Cargo.lock ] || printf '%s\0' Cargo.lock
        [ ! -f build.rs ] || printf '%s\0' build.rs
        [ ! -f rust-toolchain ] || printf '%s\0' rust-toolchain
        [ ! -f rust-toolchain.toml ] || printf '%s\0' rust-toolchain.toml
        printf '%s\0' scripts/verify-ordinary-map-v2-checked-get-l3.sh
    } | sort -zu | while IFS= read -r -d '' source_path; do
        source_absolute="$(realpath -m -- "$ROOT_DIR/$source_path")"
        if path_is_within "$source_absolute" "$artifact_dir" || path_is_within "$source_absolute" "$log_file_absolute"; then
            continue
        fi
        "$SHA256SUM_PATH" -- "$source_path"
    done
}

record_tool() {
    local label="$1" executable="$2" real="$3"
    shift 3
    local directory="$artifact_dir/toolchain/$label"
    mkdir -- "$directory"
    printf '%q ' "$executable" "$@" > "$directory/command.txt"
    printf '\n' >> "$directory/command.txt"
    printf '%s\0' "$executable" "$@" > "$directory/command.argv.nul"
    printf 'invoked_path=%s\nreal_path=%s\n' "$executable" "$real" > "$directory/identity.txt"
    "$SHA256SUM_PATH" -- "$real" | awk '{print "sha256=" $1}' >> "$directory/identity.txt"
    if /usr/bin/env -i PATH="$system_path" HOME="${HOME:-/tmp}" \
        RUSTUP_HOME="${RUSTUP_HOME:-${HOME:-/tmp}/.rustup}" "$executable" "$@" \
        > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
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
source_snapshot_manifest > "$scratch/source-before.manifest"
snapshot_before="$("$SHA256SUM_PATH" -- "$scratch/source-before.manifest" | awk '{print $1}')"
if [ -n "$artifact_dir" ]; then
    cp "$scratch/source-before.manifest" "$artifact_dir/source-snapshot.before.manifest.sha256"
    printf '%s\n' "$snapshot_before" > "$artifact_dir/source-snapshot.before.sha256"
    mkdir -- "$artifact_dir/toolchain"
    toolchain_status=0
    record_tool rustc "$RUSTC_PATH" "$RUSTC_REAL_PATH" --version --verbose || toolchain_status=$?
    record_tool cargo "$CARGO_PATH" "$CARGO_REAL_PATH" --version --verbose || toolchain_status=$?
    record_tool valgrind "$VALGRIND_PATH" "$VALGRIND_REAL_PATH" --version || toolchain_status=$?
    printf 'binary_path=%s\n' "$VALGRIND_BINARY_PATH" >> "$artifact_dir/toolchain/valgrind/identity.txt"
    "$SHA256SUM_PATH" -- "$VALGRIND_BINARY_PATH" | awk '{print "binary_sha256=" $1}' >> "$artifact_dir/toolchain/valgrind/identity.txt"
    record_tool sha256sum "$SHA256SUM_PATH" "$SHA256SUM_REAL_PATH" --version || toolchain_status=$?
    {
        printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
        printf 'rustc_invoked_path=%s\nrustc_real_path=%s\nrustc_version=' "$RUSTC_PATH" "$RUSTC_REAL_PATH"
        cat "$artifact_dir/toolchain/rustc/stdout.bin"
        printf 'cargo_invoked_path=%s\ncargo_real_path=%s\ncargo_version=' "$CARGO_PATH" "$CARGO_REAL_PATH"
        cat "$artifact_dir/toolchain/cargo/stdout.bin"
        printf 'valgrind_invoked_path=%s\nvalgrind_real_path=%s\nvalgrind_version=' "$VALGRIND_PATH" "$VALGRIND_REAL_PATH"
        cat "$artifact_dir/toolchain/valgrind/stdout.bin"
        printf 'sha256sum_invoked_path=%s\nsha256sum_real_path=%s\nsha256sum_version=' "$SHA256SUM_PATH" "$SHA256SUM_REAL_PATH"
        cat "$artifact_dir/toolchain/sha256sum/stdout.bin"
    } > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"
fi

cargo_arguments=(
    test --verbose --manifest-path "$ROOT_DIR/Cargo.toml"
    --test "$TEST_TARGET" "$TEST_NAME" --
    --exact --ignored --nocapture --test-threads=1
)
test_status=0
if [ -n "$artifact_dir" ]; then
    {
        printf 'LLVM_SYS_181_PREFIX=%q CARGO_TARGET_DIR=%q MIMI_REQUIRE_VALGRIND=1 MIMI_L3_VALGRIND=%q MIMI_L3_SHA256SUM=%q %q ' "$LLVM_SYS_181_PREFIX" "$cargo_target_dir" "$VALGRIND_REAL_PATH" "$SHA256SUM_REAL_PATH" "$CARGO_PATH"
        printf '%q ' "${cargo_arguments[@]}"
        printf '\n'
    } > "$artifact_dir/suite.command.txt"
    printf '%s\0' "$CARGO_PATH" "${cargo_arguments[@]}" > "$artifact_dir/suite.argv.nul"
    printf 'working_directory=%s\nHOME=%s\nRUSTUP_HOME=%s\nRUSTUP_TOOLCHAIN=<unset>\nRUSTFLAGS=<unset>\nCARGO_ENCODED_RUSTFLAGS=<unset>\nCARGO_TARGET_*_RUNNER=<unset>\nCARGO_CONFIG_POLICY=reject-workspace-and-cargo-home-configs\nCARGO_HOME_CONFIG=<absent>\nCARGO_HOME_POLICY=reuse-stable-home-cargo-cache\nCARGO_TARGET_POLICY=%s\n%s=%q\nCARGO_TARGET_DIR_REQUESTED=%q\nCARGO_TARGET_DIR_PHYSICAL=%q\nLLVM_SYS_181_PREFIX=%s\nMIMI_REQUIRE_VALGRIND=1\nMIMI_L3_VALGRIND=%s\nMIMI_L3_SHA256SUM=%s\nPATH=%s\nCARGO_HOME=%s\nCARGO_TARGET_DIR=%s\nRUSTC=%s\n%s=%s\n' \
        "$ROOT_DIR" "${HOME:-/tmp}" "${RUSTUP_HOME:-${HOME:-/tmp}/.rustup}" \
        "$cargo_target_dir_policy" "$TARGET_DIR_ENV" "$target_dir_requested" "$cargo_target_dir" \
        "$LLVM_SYS_181_PREFIX" "$VALGRIND_REAL_PATH" "$SHA256SUM_REAL_PATH" "$system_path" \
        "$cache_cargo_home" "$cargo_target_dir" "$RUSTC_PATH" "$ARTIFACT_ENV" "$artifact_dir" > "$artifact_dir/suite.environment.txt"
    if [ "$toolchain_status" -ne 0 ]; then
        test_status="$toolchain_status"
        : > "$artifact_dir/suite.stdout"
        printf 'toolchain version command failed; see toolchain/ captures\n' > "$artifact_dir/suite.stderr"
    elif /usr/bin/env -i PATH="$system_path" HOME="${HOME:-/tmp}" \
        RUSTUP_HOME="${RUSTUP_HOME:-${HOME:-/tmp}/.rustup}" CARGO_HOME="$cache_cargo_home" \
        CARGO_TARGET_DIR="$cargo_target_dir" RUSTC="$RUSTC_PATH" LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        MIMI_REQUIRE_VALGRIND=1 MIMI_L3_VALGRIND="$VALGRIND_REAL_PATH" MIMI_L3_SHA256SUM="$SHA256SUM_REAL_PATH" \
        MIMI_MAP_V2_CHECKED_GET_L3_ARTIFACT_DIR="$artifact_dir" \
        bash -c 'cd -- "$1"; shift; exec "$@"' _ "$ROOT_DIR" "$CARGO_PATH" "${cargo_arguments[@]}" \
        > "$artifact_dir/suite.stdout" 2> "$artifact_dir/suite.stderr"; then
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
    if /usr/bin/env -i PATH="$system_path" HOME="${HOME:-/tmp}" \
        RUSTUP_HOME="${RUSTUP_HOME:-${HOME:-/tmp}/.rustup}" CARGO_HOME="$cache_cargo_home" \
        CARGO_TARGET_DIR="$cargo_target_dir" RUSTC="$RUSTC_PATH" LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        MIMI_REQUIRE_VALGRIND=1 MIMI_L3_VALGRIND="$VALGRIND_REAL_PATH" MIMI_L3_SHA256SUM="$SHA256SUM_REAL_PATH" \
        bash -c 'cd -- "$1"; shift; exec "$@"' _ "$ROOT_DIR" "$CARGO_PATH" "${cargo_arguments[@]}" 2>&1 | tee "$log_file"; then
        test_status=0
    else
        test_status=$?
    fi
fi

source_snapshot_manifest > "$scratch/source-after.manifest"
snapshot_after="$("$SHA256SUM_PATH" -- "$scratch/source-after.manifest" | awk '{print $1}')"
printf 'source_snapshot_sha256=%s\nsource_snapshot_sha256_after=%s\n' "$snapshot_before" "$snapshot_after"
snapshot_status=0
if ! cmp -s "$scratch/source-before.manifest" "$scratch/source-after.manifest"; then
    printf 'source snapshot changed during the CheckedGet strict L3 run\n' >&2
    snapshot_status=98
fi

validation_status=0
if [ -n "$artifact_dir" ]; then
    cp "$scratch/source-after.manifest" "$artifact_dir/source-snapshot.after.manifest.sha256"
    printf '%s\n' "$snapshot_after" > "$artifact_dir/source-snapshot.after.sha256"
    if [ "$test_status" -eq 0 ]; then
        summary="$artifact_dir/native/run-summary.txt"
        if [ ! -s "$summary" ]; then
            printf 'successful L3 test left no native run summary\n' >&2
            validation_status=1
        else
            grep -Fxq 'route_profile=map-any-owner-v2-checked-get-v1' "$summary" || validation_status=1
            grep -Fxq 'global_map_any_v2_production_ready=false' "$summary" || validation_status=1
            grep -Fxq 'case_count=8' "$summary" || validation_status=1
            grep -Fxq 'native_build_count=16' "$summary" || validation_status=1
            grep -Fxq 'memcheck_run_count=16' "$summary" || validation_status=1
            grep -Fxq 'default_route_receipt_count=8' "$summary" || validation_status=1
            grep -Fxq 'explicit_mir_census_count=8' "$summary" || validation_status=1
            grep -Fxq 'legacy_fallback_count=0' "$summary" || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name program | wc -l)" == 16 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name valgrind.log | wc -l)" == 16 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name memcheck-summary.txt | wc -l)" == 16 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name command.argv.json | wc -l)" == 58 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name process.status | wc -l)" == 58 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name stdout.bin | wc -l)" == 58 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name stderr.bin | wc -l)" == 58 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name '*-identity.txt' | wc -l)" == 19 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -mindepth 1 -maxdepth 1 -type d -name 'case-*' | wc -l)" == 8 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name program ! -perm -111 | wc -l)" == 0 ]] || validation_status=1
            grep -Fxq "real_path=$VALGRIND_REAL_PATH" "$artifact_dir/native/valgrind-tool-identity.txt" || validation_status=1
            grep -Fxq "binary_path=$VALGRIND_BINARY_PATH" "$artifact_dir/native/valgrind-tool-identity.txt" || validation_status=1
            grep -Fxq "real_path=$SHA256SUM_REAL_PATH" "$artifact_dir/native/sha256sum-tool-identity.txt" || validation_status=1

            for report in "$artifact_dir"/native/case-*/default-memcheck/memcheck-summary.txt \
                "$artifact_dir"/native/case-*/explicit-memcheck/memcheck-summary.txt; do
                [ -s "$report" ] || { validation_status=1; continue; }
                for expected in 'definitely lost: 0 bytes in 0 blocks' \
                    'indirectly lost: 0 bytes in 0 blocks' \
                    'possibly lost: 0 bytes in 0 blocks' \
                    'suppressed: 0 bytes in 0 blocks' 'ERROR SUMMARY: 0 errors' \
                    'ERROR SUPPRESSION SUMMARY: 0 from 0'; do
                    grep -Fq "$expected" "$report" || validation_status=1
                done
            done
            case_seen=0
            expected_labels=(hit-i32 hit-bool miss-i32 hit-string hit-label settled-branch-tail-i32 multi-lookup multi-lookup-reversed)
            expected_fixtures=(
                mir_ordinary_map_any_owner_v2_checked_get_hit_i32.mimi
                mir_ordinary_map_any_owner_v2_checked_get_hit_bool.mimi
                mir_ordinary_map_any_owner_v2_checked_get_miss_i32.mimi
                mir_ordinary_map_any_owner_v2_checked_get_hit_string.mimi
                mir_ordinary_map_any_owner_v2_checked_get_hit_label.mimi
                mir_ordinary_map_any_owner_v2_checked_get_settled_branch_tail.mimi
                mir_ordinary_map_any_owner_v2_checked_get_multi_lookup.mimi
                mir_ordinary_map_any_owner_v2_checked_get_multi_lookup_reversed.mimi
            )
            expected_exits=(1 1 0 1 1 1 1 1)
            for index in "${!expected_labels[@]}"; do
                case_directory="$artifact_dir/native/case-$(printf '%02d' "$index")-${expected_labels[$index]}"
                [ -d "$case_directory" ] || { validation_status=1; continue; }
                [ -d "$case_directory" ] || continue
                case_seen=$((case_seen + 1))
                fixture_path="$ROOT_DIR/tests/fixtures/${expected_fixtures[$index]}"
                cmp -s "$fixture_path" "$case_directory/source.fixture.mimi" || validation_status=1
                for mode in default explicit; do
                    result_file="$case_directory/$mode-memcheck/expected-native-result.txt"
                    grep -Fxq "expected_exit_code=${expected_exits[$index]}" "$result_file" || validation_status=1
                    grep -Fxq 'expected_stdout_bytes=0' "$result_file" || validation_status=1
                    [ ! -s "$case_directory/$mode-memcheck/expected-native-stdout.bin" ] || validation_status=1
                done
                for status_file in "$case_directory/receipt/process.status" \
                    "$case_directory/default-build/process.status" "$case_directory/explicit-build/process.status"; do
                    grep -Fxq 'exit_code=0' "$status_file" || validation_status=1
                done
                digest="$(sed -n 's/^mir_digest=//p' "$case_directory/receipt/stdout.bin")"
                [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || validation_status=1
                default_stderr="$case_directory/default-build/stderr.bin"
                [ "$(grep -Fc "canonical route disposition: canonical (map-any-owner-v2-checked-get-v1) mir_digest=$digest" "$default_stderr" || true)" = 1 ] || validation_status=1
                grep -Fq 'canonical route disposition: legacy' "$default_stderr" && validation_status=1
                explicit_stderr="$case_directory/explicit-build/stderr.bin"
                census="$(grep -F 'info: canonical MIR native compiled ' "$explicit_stderr" || true)"
                [ "$(printf '%s\n' "$census" | sed '/^$/d' | wc -l)" = 1 ] || validation_status=1
                counts="${census#info: canonical MIR native compiled }"
                census_digest="${counts##*mir_digest=}"
                counts="${counts%% function(s), 0 fell back; mir_digest=*}"
                emitted="${counts%%/*}"
                expected="${counts#*/}"
                [[ "$emitted" =~ ^[0-9]+$ && "$expected" =~ ^[1-9][0-9]*$ ]] || validation_status=1
                [ "$emitted" = "$expected" ] && [ "$census_digest" = "$digest" ] || validation_status=1
                grep -Fq 'canonical route disposition: legacy' "$explicit_stderr" && validation_status=1
                for mode in default explicit; do
                    directory="$case_directory/$mode-memcheck"
                    for option in '"--leak-check=full"' '"--show-leak-kinds=all"' \
                        '"--errors-for-leak-kinds=definite,indirect,possible"' '"--error-exitcode=97"' '"--log-file='; do
                        grep -Fq "$option" "$directory/command.argv.json" || validation_status=1
                    done
                    grep -Fq "[\"$VALGRIND_REAL_PATH\"," "$directory/command.argv.json" || validation_status=1
                    expected_status="$(sed -n 's/^expected_exit_code=//p' "$directory/expected-native-result.txt")"
                    observed_status="$(sed -n 's/^exit_code=//p' "$directory/process.status")"
                    [ "$expected_status" = "$observed_status" ] || validation_status=1
                    cmp -s "$directory/expected-native-stdout.bin" "$directory/stdout.bin" || validation_status=1
                done
            done
            [ "$case_seen" = 8 ] || validation_status=1
        fi
    fi
    if [ "$snapshot_status" -ne 0 ]; then
        printf 'FAIL source snapshot changed\n' > "$artifact_dir/artifact-validation.txt"
    elif [ "$test_status" -ne 0 ]; then
        printf 'INCOMPLETE test exit=%s; partial evidence retained\n' "$test_status" > "$artifact_dir/artifact-validation.txt"
    elif [ "$validation_status" -eq 0 ]; then
        printf 'PASS CheckedGet-only strict L3; broader Map/Any V2 remains closed\n' > "$artifact_dir/artifact-validation.txt"
    else
        printf 'FAIL required CheckedGet L3 artifact validation\n' > "$artifact_dir/artifact-validation.txt"
        test_status=97
    fi
    (
        cd "$artifact_dir"
        if find . -type l -print -quit | grep -q .; then
            printf 'refusing symlinked L3 evidence files\n' >&2
            exit 1
        fi
        find . -type f ! -name files.sha256 ! -name files.sha256.sha256 -print0 \
            | sort -z | xargs -0 -r "$SHA256SUM_PATH" > files.sha256
        "$SHA256SUM_PATH" --check files.sha256
        "$SHA256SUM_PATH" files.sha256 > files.sha256.sha256
    )
fi

printf 'acceptance_test_exit=%s\n' "$test_status"
[ "$snapshot_status" -eq 0 ] || exit "$snapshot_status"
exit "$test_status"
