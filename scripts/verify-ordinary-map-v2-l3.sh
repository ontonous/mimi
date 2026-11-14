#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"

cd "$ROOT_DIR"
if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    printf 'ordinary Map v2 L3 requires an explicit external CARGO_TARGET_DIR\n' >&2
    exit 2
fi
CARGO_TARGET_DIR="$(realpath -m -- "$CARGO_TARGET_DIR")"
if [[ "$CARGO_TARGET_DIR" == "/" || ( -e "$CARGO_TARGET_DIR" && ! -d "$CARGO_TARGET_DIR" ) ]]; then
    printf 'ordinary Map v2 L3 Cargo target must be a non-root directory: %s\n' "$CARGO_TARGET_DIR" >&2
    exit 2
fi
case "$CARGO_TARGET_DIR" in
    "$ROOT_DIR"|"$ROOT_DIR"/*)
        printf 'ordinary Map v2 L3 Cargo target must be outside the repository: %s\n' "$CARGO_TARGET_DIR" >&2
        exit 2
        ;;
esac
export CARGO_TARGET_DIR
export CARGO_BUILD_JOBS=1
export CARGO_INCREMENTAL=0
export CARGO_TERM_COLOR=never
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0

resolve_command_path() {
    local name="$1" found parent leaf
    found="$(command -v "$name" 2>/dev/null || true)"
    [[ -n "$found" ]] || return 1
    case "$found" in
        /*) ;;
        *) found="$PWD/$found" ;;
    esac
    parent="$(cd -- "$(dirname -- "$found")" 2>/dev/null && pwd -P)" || return 1
    leaf="$(basename -- "$found")"
    found="$parent/$leaf"
    [[ -f "$found" && -x "$found" ]] || return 1
    printf '%s\n' "$found"
}

reject_rustc_environment_overrides() {
    local name
    for name in \
        RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER \
        CARGO_BUILD_RUSTC CARGO_BUILD_RUSTC_WRAPPER \
        CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER; do
        if [[ ${!name+x} ]]; then
            printf 'ordinary Map v2 L3 refuses an inherited Rust compiler override: %s\n' "$name" >&2
            return 1
        fi
    done
}

cargo_config_candidates() {
    local search_root="$1" cargo_home="$2" ancestor
    ancestor="$(realpath -m -- "$search_root")" || return 1
    while :; do
        printf '%s/.cargo/config\n' "$ancestor"
        printf '%s/.cargo/config.toml\n' "$ancestor"
        [[ "$ancestor" == / ]] && break
        ancestor="$(dirname -- "$ancestor")"
    done
    cargo_home="$(realpath -m -- "$cargo_home")" || return 1
    printf '%s/config\n' "$cargo_home"
    printf '%s/config.toml\n' "$cargo_home"
}

reject_cargo_config_sources() {
    local search_root="$1" cargo_home="$2" config_path config_dir
    if [[ -L "$cargo_home" ]]; then
        printf 'ordinary Map v2 L3 refuses a symlinked CARGO_HOME directory: %s\n' "$cargo_home" >&2
        return 1
    fi
    while IFS= read -r config_path; do
        config_dir="$(dirname -- "$config_path")"
        if [[ -L "$config_dir" ]]; then
            printf 'ordinary Map v2 L3 refuses a symlinked Cargo config directory: %s\n' "$config_dir" >&2
            return 1
        fi
        if [[ -L "$config_path" ]]; then
            printf 'ordinary Map v2 L3 refuses a symlinked Cargo config source: %s\n' "$config_path" >&2
            return 1
        fi
        if [[ -e "$config_path" ]]; then
            printf 'ordinary Map v2 L3 refuses Cargo config sources because effective rustc selection cannot be proven: %s\n' \
                "$config_path" >&2
            return 1
        fi
    done < <(cargo_config_candidates "$search_root" "$cargo_home" | sort -u)
}

if ! command -v valgrind >/dev/null 2>&1; then
    printf 'ordinary Map v2 L3 requires Valgrind\n' >&2
    exit 1
fi

if [ ! -d "$LLVM_SYS_181_PREFIX" ]; then
    printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi

CARGO_COMMAND="$(resolve_command_path cargo)" || {
    printf 'ordinary Map v2 L3 requires an executable Cargo command\n' >&2
    exit 1
}
RUSTC_COMMAND="$(resolve_command_path rustc)" || {
    printf 'ordinary Map v2 L3 requires an executable rustc command\n' >&2
    exit 1
}
if ! reject_rustc_environment_overrides; then
    exit 2
fi
if ! reject_cargo_config_sources "$ROOT_DIR" "${CARGO_HOME:-${HOME:-}/.cargo}"; then
    exit 2
fi
# Cargo receives the exact PATH-resolved rustc whose shim and selected toolchain are
# recorded below. Wrapper and config overrides are rejected above.
export RUSTC="$RUSTC_COMMAND"
VALGRIND_COMMAND="$(resolve_command_path valgrind)" || {
    printf 'ordinary Map v2 L3 requires an executable Valgrind command\n' >&2
    exit 1
}

artifact_dir=""
artifact_log=""
if [ -n "${MIMI_MAP_V2_L3_ARTIFACT_DIR:-}" ]; then
    requested_artifact_dir="$MIMI_MAP_V2_L3_ARTIFACT_DIR"
    case "$requested_artifact_dir" in
        /*) ;;
        *) requested_artifact_dir="$ROOT_DIR/$requested_artifact_dir" ;;
    esac
    if [ -e "$requested_artifact_dir" ] || [ -L "$requested_artifact_dir" ]; then
        printf 'refusing to overwrite existing Map v2 L3 artifact path: %s\n' "$requested_artifact_dir" >&2
        exit 2
    fi
    artifact_dir="$(realpath -m -- "$requested_artifact_dir")"

    case "$artifact_dir" in
        "$ROOT_DIR"|"$ROOT_DIR"/*)
            printf 'Map v2 L3 artifacts must be outside the repository tree: %s\n' "$artifact_dir" >&2
            exit 2
            ;;
    esac
    case "$CARGO_TARGET_DIR" in
        "$artifact_dir"|"$artifact_dir"/*)
            printf 'Map v2 L3 Cargo target cannot be inside the evidence directory: %s\n' "$CARGO_TARGET_DIR" >&2
            exit 2
            ;;
    esac
    case "$artifact_dir" in
        "$CARGO_TARGET_DIR"|"$CARGO_TARGET_DIR"/*)
            printf 'Map v2 L3 evidence cannot be inside the Cargo target directory: %s\n' "$artifact_dir" >&2
            exit 2
            ;;
    esac
    if [ -e "$artifact_dir" ] || [ -L "$artifact_dir" ]; then
        printf 'refusing to overwrite existing Map v2 L3 artifact path: %s\n' "$artifact_dir" >&2
        exit 2
    fi
    mkdir -p "$(dirname "$artifact_dir")"
    mkdir -- "$artifact_dir"
    artifact_dir="$(realpath -- "$artifact_dir")"
    export MIMI_MAP_V2_L3_ARTIFACT_DIR="$artifact_dir"
    artifact_log="$artifact_dir/suite.log"
fi

path_is_within() {
    local candidate="$1"
    local directory="$2"
    [[ -n "$directory" && ( "$candidate" == "$directory" || "$candidate" == "$directory"/* ) ]]
}

validate_log_path_is_external() {
    local candidate="$1"
    if path_is_within "$candidate" "$ROOT_DIR"; then
        printf 'Map v2 L3 suite logs must be outside the repository tree: %s\n' "$candidate" >&2
        return 1
    fi
    if [[ -n "$artifact_dir" ]] && path_is_within "$candidate" "$artifact_dir"; then
        printf 'Map v2 L3 suite log cannot alias the evidence tree: %s\n' "$candidate" >&2
        return 1
    fi
    if path_is_within "$candidate" "$CARGO_TARGET_DIR"; then
        printf 'Map v2 L3 suite log cannot be inside the Cargo target tree: %s\n' "$candidate" >&2
        return 1
    fi
}

still_reachable_counts() {
    local summary="$1"
    local bytes blocks
    read -r bytes blocks < <(
        sed -nE 's/^still reachable: ([0-9,]+) bytes in ([0-9,]+) blocks.*/\1 \2/p' "$summary" \
            | tr -d ','
    )
    [[ "$bytes" =~ ^[0-9]+$ && "$blocks" =~ ^[0-9]+$ ]] || return 1
    printf '%s %s\n' "$bytes" "$blocks"
}

source_snapshot_manifest() {
    local root source_path source_absolute symlink_path cargo_home
    local config_path
    reject_cargo_config_sources "$ROOT_DIR" "${CARGO_HOME:-${HOME:-}/.cargo}" || return 1
    for root in src tests std; do
        if [[ ! -d "$root" || -L "$root" ]]; then
            printf 'required source root is missing or symlinked: %s\n' "$root" >&2
            return 1
        fi
        symlink_path="$(find "$root" -type l -print -quit)" || return 1
        if [[ -n "$symlink_path" ]]; then
            printf 'source tree contains an unsupported symlink: %s\n' "$symlink_path" >&2
            return 1
        fi
    done
    if [[ -L .cargo ]]; then
        printf 'Cargo config directory must not be a symlink\n' >&2
        return 1
    elif [[ -d .cargo ]]; then
        symlink_path="$(find .cargo -type l -print -quit)" || return 1
        if [[ -n "$symlink_path" ]]; then
            printf 'Cargo config tree contains an unsupported symlink: %s\n' "$symlink_path" >&2
            return 1
        fi
    fi

    for source_path in Cargo.toml Cargo.lock build.rs scripts/verify-ordinary-map-v2-l3.sh \
        .cargo/config .cargo/config.toml rust-toolchain rust-toolchain.toml; do
        if [[ -L "$source_path" ]]; then
            printf 'build or toolchain input must not be a symlink: %s\n' "$source_path" >&2
            return 1
        fi
    done
    cargo_home="$(realpath -m -- "${CARGO_HOME:-${HOME:-}/.cargo}")" || return 1
    for config_path in "$cargo_home/config" "$cargo_home/config.toml"; do
        if [[ -L "$config_path" ]]; then
            printf 'Cargo home config must not be a symlink: %s\n' "$config_path" >&2
            return 1
        fi
    done

    {
        find src tests std -type f -print0
        if [[ -d .cargo ]]; then
            find .cargo -type f -print0
        fi
        printf '%s\0' Cargo.toml Cargo.lock scripts/verify-ordinary-map-v2-l3.sh
        for source_path in build.rs .cargo/config .cargo/config.toml rust-toolchain rust-toolchain.toml; do
            if [ -f "$source_path" ]; then
                printf '%s\0' "$source_path"
            fi
        done
        for config_path in "$cargo_home/config" "$cargo_home/config.toml"; do
            if [[ -f "$config_path" ]]; then
                printf '%s\0' "$config_path"
            fi
        done
    } | sort -zu | while IFS= read -r -d '' source_path; do
        case "$source_path" in
            /*) source_absolute="$(realpath -m -- "$source_path")" ;;
            *) source_absolute="$(realpath -m -- "$ROOT_DIR/$source_path")" ;;
        esac
        if path_is_within "$source_absolute" "$artifact_dir" \
            || path_is_within "$source_absolute" "$log_file_absolute"; then
            continue
        fi
        sha256sum -- "$source_path"
    done
}

CURRENT_STAGE=preflight
CURRENT_STAGE_DIR=""
CURRENT_STAGE_SOURCE_BEFORE=""
FINAL_RESULT_WRITTEN=0
EXIT_REASON=early-exit
snapshot_status=0
validation_status=0

write_artifact_manifest() {
    (
        cd "$artifact_dir"
        find . -type f \
            ! -name files.sha256 \
            ! -name files.sha256.tmp \
            ! -name files.sha256.sha256 \
            -print0 | sort -z | xargs -0 -r sha256sum > files.sha256.tmp
        mv -- files.sha256.tmp files.sha256
        sha256sum --check files.sha256
        sha256sum files.sha256 > files.sha256.sha256
        sha256sum --check files.sha256.sha256
    )
}

write_early_exit_receipt() {
    local status="$1"
    {
        printf 'result=INCOMPLETE\n'
        printf 'completion=early_exit\n'
        printf 'reason=%s\n' "$EXIT_REASON"
        printf 'exit_code=%s\n' "$status"
        printf 'current_stage=%s\n' "$CURRENT_STAGE"
        printf 'snapshot_exit=%s\n' "${snapshot_status:-unknown}"
        printf 'validation_exit=%s\n' "${validation_status:-unknown}"
    } > "$artifact_dir/suite-result.txt"
    printf 'INCOMPLETE runner exited before all stages completed; see suite-result.txt\n' \
        > "$artifact_dir/artifact-validation.txt"
    write_artifact_manifest || true
}

on_exit() {
    local status="$1"
    trap - EXIT INT TERM
    if [[ -n "$artifact_dir" && -d "$artifact_dir" && "$FINAL_RESULT_WRITTEN" -ne 1 ]]; then
        write_early_exit_receipt "$status" || true
    fi
    exit "$status"
}

record_stage_tool_identity() {
    local output_path="$1" label path resolved config_path llvm_config rustup_path
    local llvm_libdir llvm_includedir llvm_ldflags llvm_libs llvm_system_libs
    local llvm_libnames llvm_libname llvm_library
    local -a llvm_libnames_array=()
    {
        printf 'stage=%s\n' "$CURRENT_STAGE"
        printf 'cargo_rustc_policy=explicit PATH-resolved RUSTC; wrappers and Cargo config sources rejected\n'
        printf 'effective.RUSTC=%q\n' "$RUSTC"
        printf 'effective.RUSTC_WRAPPER=<unset-and-rejected-if-inherited>\n'
        printf 'effective.RUSTC_WORKSPACE_WRAPPER=<unset-and-rejected-if-inherited>\n'
        printf 'exported_environment_sha256=%s\n' "$(env -0 | sort -z | sha256sum | awk '{print $1}')"
        printf 'LLVM_SYS_181_PREFIX=%q\n' "$LLVM_SYS_181_PREFIX"
        printf 'LLVM_SYS_181_PREFIX_realpath=%s\n' "$(realpath -- "$LLVM_SYS_181_PREFIX")"
        for label in cargo rustc valgrind cc clang ld; do
            case "$label" in
                cargo) path="$CARGO_COMMAND" ;;
                rustc) path="$RUSTC_COMMAND" ;;
                valgrind) path="$VALGRIND_COMMAND" ;;
                *) path="$(resolve_command_path "$label" 2>/dev/null || true)" ;;
            esac
            if [[ -z "$path" ]]; then
                printf '%s_path=<unavailable>\n' "$label"
                continue
            fi
            resolved="$(realpath -e -- "$path")" || return 1
            printf '%s_path=%s\n%s_realpath=%s\n' "$label" "$path" "$label" "$resolved"
            sha256sum -- "$resolved" || return 1
        done

        printf 'cargo_version:\n'
        "$CARGO_COMMAND" --version --verbose || return 1
        printf 'rustc_version:\n'
        "$RUSTC_COMMAND" --version --verbose || return 1
        printf 'valgrind_version:\n'
        "$VALGRIND_COMMAND" --version || return 1

        rustup_path="$(resolve_command_path rustup 2>/dev/null || true)"
        if [[ -n "$rustup_path" ]]; then
            printf 'rustup_path=%s\n' "$rustup_path"
            sha256sum -- "$(realpath -e -- "$rustup_path")" || return 1
            printf 'rustup_active_toolchain:\n'
            "$rustup_path" show active-toolchain || return 1
            for label in cargo rustc; do
                resolved="$("$rustup_path" which "$label" 2>/dev/null || true)"
                if [[ -n "$resolved" && -f "$resolved" ]]; then
                    printf 'rustup_%s_path=%s\n' "$label" "$resolved"
                    sha256sum -- "$resolved" || return 1
                else
                    printf 'rustup_%s_path=<unavailable>\n' "$label"
                fi
            done
        else
            printf 'rustup_path=<unavailable>\n'
        fi

        llvm_config="$LLVM_SYS_181_PREFIX/bin/llvm-config"
        if [[ ! -x "$llvm_config" ]]; then
            llvm_config="$LLVM_SYS_181_PREFIX/llvm-config"
        fi
        if [[ -x "$llvm_config" ]]; then
            resolved="$(realpath -e -- "$llvm_config")" || return 1
            printf 'llvm_config_path=%s\n' "$resolved"
            sha256sum -- "$resolved" || return 1
            printf 'llvm_config_version:\n'
            "$resolved" --version || return 1
            printf 'llvm_config_prefix:\n'
            "$resolved" --prefix || return 1
            llvm_libdir="$("$resolved" --libdir)" || return 1
            llvm_includedir="$("$resolved" --includedir)" || return 1
            llvm_ldflags="$("$resolved" --ldflags)" || return 1
            llvm_libs="$("$resolved" --libs)" || return 1
            llvm_system_libs="$("$resolved" --system-libs)" || return 1
            llvm_libnames="$("$resolved" --libnames)" || return 1
            printf 'llvm_config_libdir=%s\n' "$llvm_libdir"
            printf 'llvm_config_includedir=%s\n' "$llvm_includedir"
            printf 'llvm_config_ldflags=%q\n' "$llvm_ldflags"
            printf 'llvm_config_libs=%q\n' "$llvm_libs"
            printf 'llvm_config_system_libs=%q\n' "$llvm_system_libs"
            printf 'llvm_config_libnames=%q\n' "$llvm_libnames"
            read -r -a llvm_libnames_array <<< "$llvm_libnames"
            for llvm_libname in "${llvm_libnames_array[@]}"; do
                llvm_library="$llvm_libdir/$llvm_libname"
                if [[ ! -f "$llvm_library" ]]; then
                    printf 'LLVM shared library is unavailable: %s\n' "$llvm_library" >&2
                    return 1
                fi
                resolved="$(realpath -e -- "$llvm_library")" || return 1
                printf 'llvm_library_path=%s\n' "$llvm_library"
                printf 'llvm_library_realpath=%s\n' "$resolved"
                sha256sum -- "$resolved" || return 1
            done
        else
            printf 'llvm_config_path=<unavailable>\n'
            return 1
        fi

        config_path="$(realpath -m -- "${CARGO_HOME:-${HOME:-}/.cargo}")"
        printf 'cargo_config_policy=fail-closed-no-applicable-config-files\n'
        for config_path in "$config_path/config" "$config_path/config.toml"; do
            if [[ -f "$config_path" && ! -L "$config_path" ]]; then
                printf 'cargo_config_path=%s\n' "$config_path"
                sha256sum -- "$config_path" || return 1
            elif [[ -L "$config_path" ]]; then
                printf 'cargo_config_symlink=%s\n' "$config_path"
                return 1
            fi
        done
    } > "$output_path" 2> "$output_path.stderr"
}

begin_stage_context() {
    local stage="$1" artifact_env_name="$2" artifact_env_value="$3" name
    CURRENT_STAGE="$stage"
    CURRENT_STAGE_DIR=""
    if [[ -n "$artifact_dir" ]]; then
        CURRENT_STAGE_DIR="$artifact_dir/stage-context/$stage"
        mkdir -p -- "$CURRENT_STAGE_DIR"
        source_snapshot_manifest > "$CURRENT_STAGE_DIR/source.before.manifest.sha256"
        CURRENT_STAGE_SOURCE_BEFORE="$(sha256sum -- "$CURRENT_STAGE_DIR/source.before.manifest.sha256" | awk '{print $1}')"
        printf '%s\n' "$CURRENT_STAGE_SOURCE_BEFORE" > "$CURRENT_STAGE_DIR/source.before.sha256"
        {
            printf 'stage=%s\n' "$stage"
            printf 'started_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
            printf 'repository=%s\n' "$ROOT_DIR"
            printf 'working_directory=%s\n' "$ROOT_DIR"
            printf 'source_snapshot_sha256=%s\n' "$CURRENT_STAGE_SOURCE_BEFORE"
            printf 'cargo_command=%q\n' "$CARGO_COMMAND"
            printf 'LLVM_SYS_181_PREFIX=%q\n' "$LLVM_SYS_181_PREFIX"
            printf 'effective.CARGO_TARGET_DIR=%q\n' "$CARGO_TARGET_DIR"
            printf 'effective.CARGO_BUILD_JOBS=%q\n' "$CARGO_BUILD_JOBS"
            printf 'effective.CARGO_INCREMENTAL=%q\n' "$CARGO_INCREMENTAL"
            printf 'effective.CARGO_TERM_COLOR=%q\n' "$CARGO_TERM_COLOR"
            printf 'effective.CARGO_PROFILE_DEV_DEBUG=%q\n' "$CARGO_PROFILE_DEV_DEBUG"
            printf 'effective.CARGO_PROFILE_TEST_DEBUG=%q\n' "$CARGO_PROFILE_TEST_DEBUG"
            printf 'effective.MIMI_REQUIRE_VALGRIND=1\n'
            printf 'exported_environment_sha256=%s\n' "$(env -0 | sort -z | sha256sum | awk '{print $1}')"
            printf 'stage_artifact_environment_name=%s\n' "$artifact_env_name"
            printf 'stage_artifact_environment_value=%q\n' "$artifact_env_value"
            for name in CARGO_HOME CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_TARGET CARGO_NET_OFFLINE \
                CARGO_PROFILE_DEV_OPT_LEVEL CARGO_PROFILE_TEST_OPT_LEVEL CARGO_PROFILE_DEV_DEBUG \
                CARGO_PROFILE_TEST_DEBUG CARGO_BUILD_RUSTC CARGO_BUILD_RUSTC_WRAPPER \
                CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER RUSTFLAGS RUSTDOCFLAGS RUSTUP_TOOLCHAIN RUSTC \
                RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER RUSTDOC CC CFLAGS CXX CXXFLAGS AR LD \
                LIBCLANG_PATH LLVM_CONFIG_PATH PKG_CONFIG_PATH PKG_CONFIG_LIBDIR LD_LIBRARY_PATH \
                MIMI_STDLIB MIMI_ASAN MIMI_OPT MIMI_VALUE_GLUE MIMI_LOOP_UNROLL_CAP \
                MIMI_REACHABLE_DISPATCH MIMI_EAGER_SPAWN MIMI_VERBOSE TMPDIR PATH HOME; do
                if [[ ${!name+x} ]]; then
                    printf 'env.%s=%q\n' "$name" "${!name}"
                else
                    printf 'env.%s=<unset>\n' "$name"
                fi
            done
        } > "$CURRENT_STAGE_DIR/environment.txt"
        record_stage_tool_identity "$CURRENT_STAGE_DIR/tools.before.txt"
    else
        CURRENT_STAGE_SOURCE_BEFORE="$(source_snapshot_manifest | sha256sum | awk '{print $1}')"
        printf 'stage=%s source_snapshot_sha256_before=%s\n' "$stage" "$CURRENT_STAGE_SOURCE_BEFORE"
    fi
}

finish_stage_context() {
    local stage="$1" cargo_status="$2" after_digest
    if [[ "$CURRENT_STAGE" != "$stage" ]]; then
        printf 'stage context mismatch: current=%s finished=%s\n' "$CURRENT_STAGE" "$stage" >&2
        validation_status=1
        return 1
    fi
    if [[ -n "$artifact_dir" ]]; then
        printf 'exit_code=%s\n' "$cargo_status" > "$CURRENT_STAGE_DIR/cargo.exit"
        source_snapshot_manifest > "$CURRENT_STAGE_DIR/source.after.manifest.sha256"
        after_digest="$(sha256sum -- "$CURRENT_STAGE_DIR/source.after.manifest.sha256" | awk '{print $1}')"
        printf '%s\n' "$after_digest" > "$CURRENT_STAGE_DIR/source.after.sha256"
        if [[ "$CURRENT_STAGE_SOURCE_BEFORE" == "$after_digest" ]] \
            && cmp -s "$CURRENT_STAGE_DIR/source.before.manifest.sha256" "$CURRENT_STAGE_DIR/source.after.manifest.sha256"; then
            printf 'source_snapshot_stable=true\n' > "$CURRENT_STAGE_DIR/source-stability.txt"
        else
            printf 'source_snapshot_stable=false\n' > "$CURRENT_STAGE_DIR/source-stability.txt"
            snapshot_status=98
            printf 'source snapshot changed during stage %s\n' "$stage" >&2
        fi
        record_stage_tool_identity "$CURRENT_STAGE_DIR/tools.after.txt"
        if ! cmp -s "$CURRENT_STAGE_DIR/tools.before.txt" "$CURRENT_STAGE_DIR/tools.after.txt"; then
            printf 'tool_identity_stable=false\n' > "$CURRENT_STAGE_DIR/tool-stability.txt"
            validation_status=1
            printf 'tool identity changed during stage %s\n' "$stage" >&2
        else
            printf 'tool_identity_stable=true\n' > "$CURRENT_STAGE_DIR/tool-stability.txt"
        fi
        printf 'stage=%s source_snapshot_sha256_after=%s cargo_exit_code=%s\n' \
            "$stage" "$after_digest" "$cargo_status"
    else
        after_digest="$(source_snapshot_manifest | sha256sum | awk '{print $1}')"
        if [[ "$CURRENT_STAGE_SOURCE_BEFORE" != "$after_digest" ]]; then
            snapshot_status=98
            printf 'source snapshot changed during stage %s\n' "$stage" >&2
        fi
        printf 'stage=%s source_snapshot_sha256_after=%s cargo_exit_code=%s\n' \
            "$stage" "$after_digest" "$cargo_status"
    fi
}

validate_single_cargo_test_result() {
    local output_path="$1" test_name="$2" label="$3"
    local running_count result_count test_line_count result_line
    running_count="$(grep -Ec '^running 1 test$' "$output_path" 2>/dev/null || true)"
    result_count="$(grep -Ec '^test result:' "$output_path" 2>/dev/null || true)"
    test_line_count="$(grep -Fc "test $test_name ... ok" "$output_path" 2>/dev/null || true)"
    result_line="$(grep '^test result:' "$output_path" 2>/dev/null || true)"
    if [[ "$running_count" != 1 || "$result_count" != 1 || "$test_line_count" != 1 \
        || ! "$result_line" =~ ^test\ result:\ ok\.\ 1\ passed\;\ 0\ failed\;\ 0\ ignored\;\ 0\ measured\;\ [0-9]+\ filtered\ out\;\ finished\ in\ [0-9]+([.][0-9]+)?s$ ]]; then
        printf '%s did not report exactly one passing test (%s): %s\n' \
            "$label" "$test_name" "$output_path" >&2
        return 1
    fi
}

run_plain_single_cargo_test() {
    local test_name="$1" label="$2" append_log="$3" args_name="$4"
    local captured_output
    local -n cargo_args_ref="$args_name"
    captured_output="$(mktemp "${TMPDIR:-/tmp}/mimi-map-v2-l3-${label}-$$.XXXXXX")"
    if [[ "$append_log" == true ]]; then
        if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
            MIMI_REQUIRE_VALGRIND=1 \
            "$CARGO_COMMAND" "${cargo_args_ref[@]}" 2>&1 \
            | tee -a "$log_file" "$captured_output"; then
            test_status=0
        else
            test_status=$?
        fi
    else
        if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
            MIMI_REQUIRE_VALGRIND=1 \
            "$CARGO_COMMAND" "${cargo_args_ref[@]}" 2>&1 \
            | tee "$log_file" "$captured_output"; then
            test_status=0
        else
            test_status=$?
        fi
    fi
    if ! validate_single_cargo_test_result "$captured_output" "$test_name" "$label"; then
        validation_status=1
    fi
    rm -f -- "$captured_output"
}

validate_unmatched_cargo_test_results() {
    local output_path="$1" label="$2" reported_count summary_count result_line expected_test
    local -a reported_counts=()
    mapfile -t reported_counts < <(sed -nE 's/^running ([0-9]+) tests?$/\1/p' "$output_path" 2>/dev/null || true)
    if [[ ${#reported_counts[@]} -eq 1 ]]; then
        reported_count="${reported_counts[0]}"
    else
        reported_count="${reported_counts[*]:-none}"
    fi
    summary_count="$(grep -Ec '^test result:' "$output_path" 2>/dev/null || true)"
    result_line="$(grep '^test result:' "$output_path" 2>/dev/null || true)"
    if [[ "$reported_count" != 4 || "$summary_count" != 1 \
        || ! "$result_line" =~ ^test\ result:\ ok\.\ 4\ passed\;\ 0\ failed\;\ 0\ ignored\;\ 0\ measured\;\ [0-9]+\ filtered\ out\;\ finished\ in\ [0-9]+([.][0-9]+)?s$ ]]; then
        printf '%s expected exactly four passing unmatched fixtures; observed running=%s result=%s\n' \
            "$label" "$reported_count" "${result_line:-none}" >&2
        return 1
    fi
    for expected_test in \
        ordinary_map_any_v2_unmatched_dynamic_string_keys_has_active_semantic_coverage \
        ordinary_map_any_v2_unmatched_record_unit_has_active_semantic_coverage \
        ordinary_map_any_v2_unmatched_is_empty_control_has_active_semantic_coverage \
        ordinary_map_any_v2_unmatched_clone_has_active_semantic_coverage; do
        if [[ "$(grep -Fc "test $expected_test ... ok" "$output_path" 2>/dev/null || true)" != 1 ]]; then
            printf '%s did not report the required test as passing exactly once: %s\n' \
                "$label" "$expected_test" >&2
            return 1
        fi
    done
}

record_single_test_result_validation() {
    local output_path="$1" test_name="$2" label="$3" receipt_path="$4"
    if validate_single_cargo_test_result "$output_path" "$test_name" "$label"; then
        {
            printf 'validation=PASS\n'
            printf 'required_test=%s\n' "$test_name"
            printf 'running_count=1\n'
            printf 'result_count=1\n'
            printf 'passed_count=1\n'
        } > "$receipt_path"
    else
        {
            printf 'validation=FAIL\n'
            printf 'required_test=%s\n' "$test_name"
            printf 'running_count=%s\n' "$(grep -Ec '^running 1 test$' "$output_path" 2>/dev/null || true)"
            printf 'result_count=%s\n' "$(grep -Ec '^test result:' "$output_path" 2>/dev/null || true)"
            printf 'passed_test_line_count=%s\n' "$(grep -Fc "test $test_name ... ok" "$output_path" 2>/dev/null || true)"
        } > "$receipt_path"
        validation_status=1
    fi
}

record_unmatched_test_result_validation() {
    local output_path="$1" label="$2" receipt_path="$3"
    if validate_unmatched_cargo_test_results "$output_path" "$label"; then
        {
            printf 'validation=PASS\n'
            printf 'running_count=4\n'
            printf 'result_count=1\n'
            printf 'passed_count=4\n'
            printf 'required_test=ordinary_map_any_v2_unmatched_dynamic_string_keys_has_active_semantic_coverage\n'
            printf 'required_test=ordinary_map_any_v2_unmatched_record_unit_has_active_semantic_coverage\n'
            printf 'required_test=ordinary_map_any_v2_unmatched_is_empty_control_has_active_semantic_coverage\n'
            printf 'required_test=ordinary_map_any_v2_unmatched_clone_has_active_semantic_coverage\n'
        } > "$receipt_path"
    else
        {
            printf 'validation=FAIL\n'
            printf 'running_count=%s\n' "$(sed -nE 's/^running ([0-9]+) tests?$/\1/p' "$output_path" 2>/dev/null | paste -sd ',' -)"
            printf 'result_count=%s\n' "$(grep -Ec '^test result:' "$output_path" 2>/dev/null || true)"
            printf 'result_line=%s\n' "$(grep '^test result:' "$output_path" 2>/dev/null | tr '\n' ' ')"
        } > "$receipt_path"
        validation_status=1
    fi
}

trap 'on_exit $?' EXIT
trap 'EXIT_REASON=SIGINT; exit 130' INT
trap 'EXIT_REASON=SIGTERM; exit 143' TERM

cd "$ROOT_DIR"
if [ -n "${MIMI_MAP_V2_L3_LOG:-}" ]; then
    log_file="$MIMI_MAP_V2_L3_LOG"
else
    log_file="${TMPDIR:-/tmp}/mimi-map-v2-l3-$$.log"
fi
if [ -e "$log_file" ] || [ -L "$log_file" ]; then
    printf 'refusing to overwrite existing Map v2 L3 log: %s\n' "$log_file" >&2
    exit 2
fi
log_file_absolute="$(realpath -m -- "$log_file")"
if ! validate_log_path_is_external "$log_file_absolute"; then
    exit 2
fi
mkdir -p "$(dirname "$log_file_absolute")"
log_file="$log_file_absolute"

if [ -n "$artifact_dir" ]; then
    source_snapshot_manifest > "$artifact_dir/source-snapshot.before.manifest.sha256"
    snapshot_before="$(sha256sum -- "$artifact_dir/source-snapshot.before.manifest.sha256" | awk '{print $1}')"
    printf '%s\n' "$snapshot_before" > "$artifact_dir/source-snapshot.before.sha256"
else
    snapshot_before="$(source_snapshot_manifest | sha256sum | awk '{print $1}')"
fi
printf 'source_snapshot_sha256=%s\n' "$snapshot_before"
printf 'explicit_mir_census=retained_in_log:%s\n' "$log_file"

test_name=map_any_owner_v2_default_and_explicit_native_builds_are_strict_memcheck_clean
cargo_arguments=(
    test
    --locked
    --manifest-path "$ROOT_DIR/Cargo.toml"
    --test canonical_mir_ordinary_map_any_owner_v2_memcheck_l3
    "$test_name"
    --
    --exact
    --ignored
    --nocapture
    --test-threads=1
)
direct_tuple_get_l1_test_name=ordinary_map_owned_any_v2_direct_tuple_get_l1_default_and_explicit_builds_are_strict_memcheck_clean
direct_tuple_get_l1_cargo_arguments=(
    test
    --locked
    --manifest-path "$ROOT_DIR/Cargo.toml"
    --test canonical_mir_ordinary_map_any_owner_v2_memcheck_l3
    "$direct_tuple_get_l1_test_name"
    --
    --exact
    --ignored
    --nocapture
    --test-threads=1
)
direct_print_result_test_name=ordinary_map_owned_any_v2_direct_print_result_default_and_explicit_native_builds_are_strict_memcheck_clean
direct_print_result_cargo_arguments=(
    test
    --locked
    --manifest-path "$ROOT_DIR/Cargo.toml"
    --test canonical_mir_ordinary_map_any_owner_v2_memcheck_l3
    "$direct_print_result_test_name"
    --
    --exact
    --ignored
    --nocapture
    --test-threads=1
)
computed_key_string_test_name=ordinary_map_owned_any_v2_computed_key_string_get_default_and_explicit_builds_are_strict_memcheck_clean
computed_key_string_cargo_arguments=(
    test
    --locked
    --manifest-path "$ROOT_DIR/Cargo.toml"
    --test canonical_mir_ordinary_map_any_owner_v2_memcheck_l3
    "$computed_key_string_test_name"
    --
    --exact
    --ignored
    --nocapture
    --test-threads=1
)
keys_snapshot_test_name=ordinary_map_owned_any_v2_keys_snapshot_default_and_explicit_builds_are_strict_memcheck_clean
keys_snapshot_cargo_arguments=(
    test
    --locked
    --manifest-path "$ROOT_DIR/Cargo.toml"
    --test canonical_mir_ordinary_map_any_owner_v2_memcheck_l3
    "$keys_snapshot_test_name"
    --
    --exact
    --ignored
    --nocapture
    --test-threads=1
)
heterogeneous_values_test_name=ordinary_map_owned_any_v2_values_heterogeneous_string_index1_default_and_explicit_builds_are_strict_memcheck_clean
heterogeneous_values_cargo_arguments=(
    test
    --locked
    --manifest-path "$ROOT_DIR/Cargo.toml"
    --test canonical_mir_ordinary_map_any_owner_v2_memcheck_l3
    "$heterogeneous_values_test_name"
    --
    --exact
    --ignored
    --nocapture
    --test-threads=1
)
unmatched_fixtures_test_filter=ordinary_map_any_v2_unmatched_
unmatched_fixtures_cargo_arguments=(
    test
    --locked
    --manifest-path "$ROOT_DIR/Cargo.toml"
    --test canonical_mir_ordinary_map_any_v2_unmatched_fixtures_l3
    "$unmatched_fixtures_test_filter"
    --
    --ignored
    --nocapture
    --test-threads=1
)

begin_stage_context map-owner MIMI_MAP_V2_L3_ARTIFACT_DIR "$artifact_dir"
test_status=0
if [ -n "$artifact_dir" ]; then
    {
        printf 'LLVM_SYS_181_PREFIX=%q MIMI_REQUIRE_VALGRIND=1 ' "$LLVM_SYS_181_PREFIX"
        printf '%q ' "$CARGO_COMMAND" "${cargo_arguments[@]}"
        printf '\n'
    } > "$artifact_dir/suite.command.txt"
    printf '%s\0' "$CARGO_COMMAND" "${cargo_arguments[@]}" > "$artifact_dir/suite.argv.nul"
    {
        printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
        printf 'MIMI_REQUIRE_VALGRIND=1\n'
        printf 'MIMI_MAP_V2_L3_ARTIFACT_DIR=%s\n' "$artifact_dir"
        printf 'effective.CARGO_TARGET_DIR=%q\n' "$CARGO_TARGET_DIR"
        printf 'effective.CARGO_BUILD_JOBS=1\n'
        printf 'effective.CARGO_INCREMENTAL=0\n'
        printf 'effective.CARGO_TERM_COLOR=never\n'
        printf 'effective.CARGO_PROFILE_DEV_DEBUG=0\n'
        printf 'effective.CARGO_PROFILE_TEST_DEBUG=0\n'
        for name in CARGO_HOME CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_TARGET \
            CARGO_NET_OFFLINE CARGO_BUILD_RUSTC CARGO_BUILD_RUSTC_WRAPPER \
            CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_PROFILE_DEV_OPT_LEVEL CARGO_PROFILE_TEST_OPT_LEVEL \
            CARGO_PROFILE_DEV_DEBUG CARGO_PROFILE_TEST_DEBUG RUSTFLAGS RUSTDOCFLAGS \
            RUSTUP_TOOLCHAIN RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER RUSTDOC \
            CC CFLAGS CXX CXXFLAGS AR LD LIBCLANG_PATH LLVM_CONFIG_PATH \
            PKG_CONFIG_PATH PKG_CONFIG_LIBDIR LD_LIBRARY_PATH PATH HOME; do
            if [[ ${!name+x} ]]; then
                printf 'env.%s=%q\n' "$name" "${!name}"
            else
                printf 'env.%s=<unset>\n' "$name"
            fi
        done
        for name in cargo rustc valgrind cc clang ld; do
            tool_path="$(command -v "$name" || true)"
            if [[ -z "$tool_path" ]]; then
                printf '%s_path=<unavailable>\n' "$name"
                continue
            fi
            resolved_tool="$(realpath -- "$tool_path")"
            printf '%s_path=%s\n' "$name" "$tool_path"
            printf '%s_realpath=%s\n' "$name" "$resolved_tool"
            if [[ -f "$resolved_tool" ]]; then
                sha256sum -- "$resolved_tool"
            fi
        done
        printf 'cargo_version='; "$CARGO_COMMAND" --version --verbose
        printf 'rustc_version='; "$RUSTC_COMMAND" --version --verbose
        printf 'valgrind_version='; "$VALGRIND_COMMAND" --version
        printf 'rustup_active_toolchain='; rustup show active-toolchain 2>/dev/null || true
        for tool in cargo rustc; do
            if command -v rustup >/dev/null 2>&1; then
                toolchain_path="$(rustup which "$tool" 2>/dev/null || true)"
                if [[ -n "$toolchain_path" && -f "$toolchain_path" ]]; then
                    printf 'rustup_%s_path=%s\n' "$tool" "$toolchain_path"
                    sha256sum -- "$toolchain_path"
                else
                    printf 'rustup_%s_path=<unavailable>\n' "$tool"
                fi
            fi
        done
        for config in "${CARGO_HOME:-${HOME:-}/.cargo}/config" \
            "${CARGO_HOME:-${HOME:-}/.cargo}/config.toml"; do
            if [[ -f "$config" && ! -L "$config" ]]; then
                printf 'cargo_config_path=%s\n' "$config"
                sha256sum -- "$config"
            elif [[ -L "$config" ]]; then
                printf 'cargo_config_symlink=%s\n' "$config"
                resolved_config="$(realpath -- "$config" 2>/dev/null || true)"
                if [[ -n "$resolved_config" && -f "$resolved_config" ]]; then
                    printf 'cargo_config_realpath=%s\n' "$resolved_config"
                    sha256sum -- "$resolved_config"
                fi
            fi
        done
        if [[ -x "$LLVM_SYS_181_PREFIX/bin/llvm-config" ]]; then
            llvm_config="$LLVM_SYS_181_PREFIX/bin/llvm-config"
            printf 'llvm_config_path=%s\n' "$(realpath -- "$llvm_config")"
            sha256sum -- "$llvm_config"
            "$llvm_config" --version
        else
            printf 'llvm_config_path=<unavailable>\n'
        fi
    } > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"

    if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        MIMI_REQUIRE_VALGRIND=1 \
        MIMI_MAP_V2_L3_ARTIFACT_DIR="$artifact_dir" \
        "$CARGO_COMMAND" "${cargo_arguments[@]}" \
            > "$artifact_dir/suite.stdout" \
            2> "$artifact_dir/suite.stderr"; then
        test_status=0
    else
        test_status=$?
    fi
    {
        printf '===== captured stdout =====\n'
        cat "$artifact_dir/suite.stdout"
        printf '\n===== captured stderr =====\n'
        cat "$artifact_dir/suite.stderr"
    } > "$artifact_log"
    cat "$artifact_dir/suite.stdout"
    cat "$artifact_dir/suite.stderr" >&2
    if [ "$log_file" != "$artifact_log" ]; then
        cp -n -- "$artifact_log" "$log_file"
    fi
    printf '%s\n' "$test_status" > "$artifact_dir/acceptance-test.exit"
else
    run_plain_single_cargo_test "$test_name" map-owner false cargo_arguments
fi
if [[ -n "$artifact_dir" ]]; then
    record_single_test_result_validation \
        "$artifact_dir/suite.stdout" "$test_name" map-owner \
        "$artifact_dir/test-result-validation.txt"
fi
finish_stage_context map-owner "$test_status"

begin_stage_context direct-tuple-get-l1 MIMI_MAP_V2_L3_ARTIFACT_DIR "$artifact_dir/direct-tuple-get-l1"
direct_tuple_get_l1_test_status=0
if [ -n "$artifact_dir" ]; then
    {
        printf 'LLVM_SYS_181_PREFIX=%q MIMI_REQUIRE_VALGRIND=1 ' "$LLVM_SYS_181_PREFIX"
        printf 'MIMI_MAP_V2_L3_ARTIFACT_DIR=%q ' "$artifact_dir/direct-tuple-get-l1"
        printf '%q ' "$CARGO_COMMAND" "${direct_tuple_get_l1_cargo_arguments[@]}"
        printf '\n'
    } > "$artifact_dir/direct-tuple-get-l1-suite.command.txt"
    printf '%s\0' "$CARGO_COMMAND" "${direct_tuple_get_l1_cargo_arguments[@]}" \
        > "$artifact_dir/direct-tuple-get-l1-suite.argv.nul"
    if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        MIMI_REQUIRE_VALGRIND=1 \
        MIMI_MAP_V2_L3_ARTIFACT_DIR="$artifact_dir/direct-tuple-get-l1" \
        "$CARGO_COMMAND" "${direct_tuple_get_l1_cargo_arguments[@]}" \
            > "$artifact_dir/direct-tuple-get-l1-suite.stdout" \
            2> "$artifact_dir/direct-tuple-get-l1-suite.stderr"; then
        direct_tuple_get_l1_test_status=0
    else
        direct_tuple_get_l1_test_status=$?
    fi
    {
        printf '\n===== direct tuple Get L1 L3 captured stdout =====\n'
        cat "$artifact_dir/direct-tuple-get-l1-suite.stdout"
        printf '\n===== direct tuple Get L1 L3 captured stderr =====\n'
        cat "$artifact_dir/direct-tuple-get-l1-suite.stderr"
    } >> "$artifact_log"
    cat "$artifact_dir/direct-tuple-get-l1-suite.stdout"
    cat "$artifact_dir/direct-tuple-get-l1-suite.stderr" >&2
    if [ "$log_file" != "$artifact_log" ]; then
        {
            printf '\n===== direct tuple Get L1 L3 captured stdout =====\n'
            cat "$artifact_dir/direct-tuple-get-l1-suite.stdout"
            printf '\n===== direct tuple Get L1 L3 captured stderr =====\n'
            cat "$artifact_dir/direct-tuple-get-l1-suite.stderr"
        } >> "$log_file"
    fi
    printf '%s\n' "$direct_tuple_get_l1_test_status" \
        > "$artifact_dir/direct-tuple-get-l1-acceptance-test.exit"
else
    run_plain_single_cargo_test "$direct_tuple_get_l1_test_name" direct-tuple-get-l1 true direct_tuple_get_l1_cargo_arguments
    direct_tuple_get_l1_test_status="$test_status"
fi
if [[ -n "$artifact_dir" ]]; then
    record_single_test_result_validation \
        "$artifact_dir/direct-tuple-get-l1-suite.stdout" "$direct_tuple_get_l1_test_name" direct-tuple-get-l1 \
        "$artifact_dir/direct-tuple-get-l1-test-result-validation.txt"
fi
finish_stage_context direct-tuple-get-l1 "$direct_tuple_get_l1_test_status"

begin_stage_context direct-print-result MIMI_MAP_V2_L3_ARTIFACT_DIR "$artifact_dir/direct-print-result"
direct_print_result_test_status=0
if [ -n "$artifact_dir" ]; then
    {
        printf 'LLVM_SYS_181_PREFIX=%q MIMI_REQUIRE_VALGRIND=1 ' "$LLVM_SYS_181_PREFIX"
        printf 'MIMI_MAP_V2_L3_ARTIFACT_DIR=%q ' "$artifact_dir/direct-print-result"
        printf '%q ' "$CARGO_COMMAND" "${direct_print_result_cargo_arguments[@]}"
        printf '\n'
    } > "$artifact_dir/direct-print-result-suite.command.txt"
    printf '%s\0' "$CARGO_COMMAND" "${direct_print_result_cargo_arguments[@]}" \
        > "$artifact_dir/direct-print-result-suite.argv.nul"
    if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        MIMI_REQUIRE_VALGRIND=1 \
        MIMI_MAP_V2_L3_ARTIFACT_DIR="$artifact_dir/direct-print-result" \
        "$CARGO_COMMAND" "${direct_print_result_cargo_arguments[@]}" \
            > "$artifact_dir/direct-print-result-suite.stdout" \
            2> "$artifact_dir/direct-print-result-suite.stderr"; then
        direct_print_result_test_status=0
    else
        direct_print_result_test_status=$?
    fi
    {
        printf '\n===== DirectPrintResult L3 captured stdout =====\n'
        cat "$artifact_dir/direct-print-result-suite.stdout"
        printf '\n===== DirectPrintResult L3 captured stderr =====\n'
        cat "$artifact_dir/direct-print-result-suite.stderr"
    } >> "$artifact_log"
    cat "$artifact_dir/direct-print-result-suite.stdout"
    cat "$artifact_dir/direct-print-result-suite.stderr" >&2
    if [ "$log_file" != "$artifact_log" ]; then
        {
            printf '\n===== DirectPrintResult L3 captured stdout =====\n'
            cat "$artifact_dir/direct-print-result-suite.stdout"
            printf '\n===== DirectPrintResult L3 captured stderr =====\n'
            cat "$artifact_dir/direct-print-result-suite.stderr"
        } >> "$log_file"
    fi
    printf '%s\n' "$direct_print_result_test_status" \
        > "$artifact_dir/direct-print-result-acceptance-test.exit"
else
    run_plain_single_cargo_test "$direct_print_result_test_name" direct-print-result true direct_print_result_cargo_arguments
    direct_print_result_test_status="$test_status"
fi
if [[ -n "$artifact_dir" ]]; then
    record_single_test_result_validation \
        "$artifact_dir/direct-print-result-suite.stdout" "$direct_print_result_test_name" direct-print-result \
        "$artifact_dir/direct-print-result-test-result-validation.txt"
fi
finish_stage_context direct-print-result "$direct_print_result_test_status"

begin_stage_context computed-key-string-get MIMI_MAP_V2_L3_ARTIFACT_DIR "$artifact_dir/computed-key-string-get"
computed_key_string_test_status=0
if [ -n "$artifact_dir" ]; then
    {
        printf 'LLVM_SYS_181_PREFIX=%q MIMI_REQUIRE_VALGRIND=1 ' "$LLVM_SYS_181_PREFIX"
        printf 'MIMI_MAP_V2_L3_ARTIFACT_DIR=%q ' "$artifact_dir/computed-key-string-get"
        printf '%q ' "$CARGO_COMMAND" "${computed_key_string_cargo_arguments[@]}"
        printf '\n'
    } > "$artifact_dir/computed-key-string-get-suite.command.txt"
    printf '%s\0' "$CARGO_COMMAND" "${computed_key_string_cargo_arguments[@]}" \
        > "$artifact_dir/computed-key-string-get-suite.argv.nul"
    if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        MIMI_REQUIRE_VALGRIND=1 \
        MIMI_MAP_V2_L3_ARTIFACT_DIR="$artifact_dir/computed-key-string-get" \
        "$CARGO_COMMAND" "${computed_key_string_cargo_arguments[@]}" \
            > "$artifact_dir/computed-key-string-get-suite.stdout" \
            2> "$artifact_dir/computed-key-string-get-suite.stderr"; then
        computed_key_string_test_status=0
    else
        computed_key_string_test_status=$?
    fi
    {
        printf '\n===== String ComputedKeyGet L3 captured stdout =====\n'
        cat "$artifact_dir/computed-key-string-get-suite.stdout"
        printf '\n===== String ComputedKeyGet L3 captured stderr =====\n'
        cat "$artifact_dir/computed-key-string-get-suite.stderr"
    } >> "$artifact_log"
    cat "$artifact_dir/computed-key-string-get-suite.stdout"
    cat "$artifact_dir/computed-key-string-get-suite.stderr" >&2
    if [ "$log_file" != "$artifact_log" ]; then
        {
            printf '\n===== String ComputedKeyGet L3 captured stdout =====\n'
            cat "$artifact_dir/computed-key-string-get-suite.stdout"
            printf '\n===== String ComputedKeyGet L3 captured stderr =====\n'
            cat "$artifact_dir/computed-key-string-get-suite.stderr"
        } >> "$log_file"
    fi
    printf '%s\n' "$computed_key_string_test_status" \
        > "$artifact_dir/computed-key-string-get-acceptance-test.exit"
else
    run_plain_single_cargo_test "$computed_key_string_test_name" computed-key-string-get true computed_key_string_cargo_arguments
    computed_key_string_test_status="$test_status"
fi
if [[ -n "$artifact_dir" ]]; then
    record_single_test_result_validation \
        "$artifact_dir/computed-key-string-get-suite.stdout" "$computed_key_string_test_name" computed-key-string-get \
        "$artifact_dir/computed-key-string-get-test-result-validation.txt"
fi
finish_stage_context computed-key-string-get "$computed_key_string_test_status"

begin_stage_context keys-snapshot MIMI_MAP_V2_L3_ARTIFACT_DIR "$artifact_dir/keys-snapshot"
keys_snapshot_test_status=0
if [ -n "$artifact_dir" ]; then
    {
        printf 'LLVM_SYS_181_PREFIX=%q MIMI_REQUIRE_VALGRIND=1 ' "$LLVM_SYS_181_PREFIX"
        printf 'MIMI_MAP_V2_L3_ARTIFACT_DIR=%q ' "$artifact_dir/keys-snapshot"
        printf '%q ' "$CARGO_COMMAND" "${keys_snapshot_cargo_arguments[@]}"
        printf '\n'
    } > "$artifact_dir/keys-snapshot-suite.command.txt"
    printf '%s\0' "$CARGO_COMMAND" "${keys_snapshot_cargo_arguments[@]}" \
        > "$artifact_dir/keys-snapshot-suite.argv.nul"
    if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        MIMI_REQUIRE_VALGRIND=1 \
        MIMI_MAP_V2_L3_ARTIFACT_DIR="$artifact_dir/keys-snapshot" \
        "$CARGO_COMMAND" "${keys_snapshot_cargo_arguments[@]}" \
            > "$artifact_dir/keys-snapshot-suite.stdout" \
            2> "$artifact_dir/keys-snapshot-suite.stderr"; then
        keys_snapshot_test_status=0
    else
        keys_snapshot_test_status=$?
    fi
    {
        printf '\n===== KeysSnapshot L3 captured stdout =====\n'
        cat "$artifact_dir/keys-snapshot-suite.stdout"
        printf '\n===== KeysSnapshot L3 captured stderr =====\n'
        cat "$artifact_dir/keys-snapshot-suite.stderr"
    } >> "$artifact_log"
    cat "$artifact_dir/keys-snapshot-suite.stdout"
    cat "$artifact_dir/keys-snapshot-suite.stderr" >&2
    if [ "$log_file" != "$artifact_log" ]; then
        {
            printf '\n===== KeysSnapshot L3 captured stdout =====\n'
            cat "$artifact_dir/keys-snapshot-suite.stdout"
            printf '\n===== KeysSnapshot L3 captured stderr =====\n'
            cat "$artifact_dir/keys-snapshot-suite.stderr"
        } >> "$log_file"
    fi
    printf '%s\n' "$keys_snapshot_test_status" \
        > "$artifact_dir/keys-snapshot-acceptance-test.exit"
else
    run_plain_single_cargo_test "$keys_snapshot_test_name" keys-snapshot true keys_snapshot_cargo_arguments
    keys_snapshot_test_status="$test_status"
fi
if [[ -n "$artifact_dir" ]]; then
    record_single_test_result_validation \
        "$artifact_dir/keys-snapshot-suite.stdout" "$keys_snapshot_test_name" keys-snapshot \
        "$artifact_dir/keys-snapshot-test-result-validation.txt"
fi
finish_stage_context keys-snapshot "$keys_snapshot_test_status"

begin_stage_context values-heterogeneous MIMI_MAP_V2_L3_ARTIFACT_DIR "$artifact_dir/values-heterogeneous"
heterogeneous_values_test_status=0
if [ -n "$artifact_dir" ]; then
    {
        printf 'LLVM_SYS_181_PREFIX=%q MIMI_REQUIRE_VALGRIND=1 ' "$LLVM_SYS_181_PREFIX"
        printf 'MIMI_MAP_V2_L3_ARTIFACT_DIR=%q ' "$artifact_dir/values-heterogeneous"
        printf '%q ' "$CARGO_COMMAND" "${heterogeneous_values_cargo_arguments[@]}"
        printf '\n'
    } > "$artifact_dir/values-heterogeneous-suite.command.txt"
    printf '%s\0' "$CARGO_COMMAND" "${heterogeneous_values_cargo_arguments[@]}" \
        > "$artifact_dir/values-heterogeneous-suite.argv.nul"
    if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        MIMI_REQUIRE_VALGRIND=1 \
        MIMI_MAP_V2_L3_ARTIFACT_DIR="$artifact_dir/values-heterogeneous" \
        "$CARGO_COMMAND" "${heterogeneous_values_cargo_arguments[@]}" \
            > "$artifact_dir/values-heterogeneous-suite.stdout" \
            2> "$artifact_dir/values-heterogeneous-suite.stderr"; then
        heterogeneous_values_test_status=0
    else
        heterogeneous_values_test_status=$?
    fi
    {
        printf '\n===== heterogeneous Values L3 captured stdout =====\n'
        cat "$artifact_dir/values-heterogeneous-suite.stdout"
        printf '\n===== heterogeneous Values L3 captured stderr =====\n'
        cat "$artifact_dir/values-heterogeneous-suite.stderr"
    } >> "$artifact_log"
    cat "$artifact_dir/values-heterogeneous-suite.stdout"
    cat "$artifact_dir/values-heterogeneous-suite.stderr" >&2
    if [ "$log_file" != "$artifact_log" ]; then
        {
            printf '\n===== heterogeneous Values L3 captured stdout =====\n'
            cat "$artifact_dir/values-heterogeneous-suite.stdout"
            printf '\n===== heterogeneous Values L3 captured stderr =====\n'
            cat "$artifact_dir/values-heterogeneous-suite.stderr"
        } >> "$log_file"
    fi
    printf '%s\n' "$heterogeneous_values_test_status" \
        > "$artifact_dir/values-heterogeneous-acceptance-test.exit"
else
    run_plain_single_cargo_test "$heterogeneous_values_test_name" values-heterogeneous true heterogeneous_values_cargo_arguments
    heterogeneous_values_test_status="$test_status"
fi
if [[ -n "$artifact_dir" ]]; then
    record_single_test_result_validation \
        "$artifact_dir/values-heterogeneous-suite.stdout" "$heterogeneous_values_test_name" values-heterogeneous \
        "$artifact_dir/values-heterogeneous-test-result-validation.txt"
fi
finish_stage_context values-heterogeneous "$heterogeneous_values_test_status"

begin_stage_context unmatched-fixtures MIMI_MAP_V2_UNMATCHED_FIXTURES_L3_ARTIFACT_DIR "$artifact_dir/unmatched-fixtures"
unmatched_fixtures_test_status=0
if [ -n "$artifact_dir" ]; then
    {
        printf 'LLVM_SYS_181_PREFIX=%q MIMI_REQUIRE_VALGRIND=1 ' "$LLVM_SYS_181_PREFIX"
        printf 'MIMI_MAP_V2_UNMATCHED_FIXTURES_L3_ARTIFACT_DIR=%q ' "$artifact_dir/unmatched-fixtures"
        printf '%q ' "$CARGO_COMMAND" "${unmatched_fixtures_cargo_arguments[@]}"
        printf '\n'
    } > "$artifact_dir/unmatched-fixtures-suite.command.txt"
    printf '%s\0' "$CARGO_COMMAND" "${unmatched_fixtures_cargo_arguments[@]}" \
        > "$artifact_dir/unmatched-fixtures-suite.argv.nul"
    if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        MIMI_REQUIRE_VALGRIND=1 \
        MIMI_MAP_V2_UNMATCHED_FIXTURES_L3_ARTIFACT_DIR="$artifact_dir/unmatched-fixtures" \
        "$CARGO_COMMAND" "${unmatched_fixtures_cargo_arguments[@]}" \
            > "$artifact_dir/unmatched-fixtures-suite.stdout" \
            2> "$artifact_dir/unmatched-fixtures-suite.stderr"; then
        unmatched_fixtures_test_status=0
    else
        unmatched_fixtures_test_status=$?
    fi
    {
        printf '\n===== unmatched Map/Any fixtures L3 captured stdout =====\n'
        cat "$artifact_dir/unmatched-fixtures-suite.stdout"
        printf '\n===== unmatched Map/Any fixtures L3 captured stderr =====\n'
        cat "$artifact_dir/unmatched-fixtures-suite.stderr"
    } >> "$artifact_log"
    cat "$artifact_dir/unmatched-fixtures-suite.stdout"
    cat "$artifact_dir/unmatched-fixtures-suite.stderr" >&2
    if [ "$log_file" != "$artifact_log" ]; then
        {
            printf '\n===== unmatched Map/Any fixtures L3 captured stdout =====\n'
            cat "$artifact_dir/unmatched-fixtures-suite.stdout"
            printf '\n===== unmatched Map/Any fixtures L3 captured stderr =====\n'
            cat "$artifact_dir/unmatched-fixtures-suite.stderr"
        } >> "$log_file"
    fi
    printf '%s\n' "$unmatched_fixtures_test_status" \
        > "$artifact_dir/unmatched-fixtures-acceptance-test.exit"
    record_unmatched_test_result_validation \
        "$artifact_dir/unmatched-fixtures-suite.stdout" artifact-mode \
        "$artifact_dir/unmatched-fixtures-test-result-validation.txt"
else
    unmatched_fixtures_captured_output="$(mktemp "${TMPDIR:-/tmp}/mimi-map-v2-l3-unmatched-fixtures-$$.XXXXXX")"
    if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        MIMI_REQUIRE_VALGRIND=1 \
        "$CARGO_COMMAND" "${unmatched_fixtures_cargo_arguments[@]}" 2>&1 \
        | tee -a "$log_file" "$unmatched_fixtures_captured_output"; then
        unmatched_fixtures_test_status=0
    else
        unmatched_fixtures_test_status=$?
    fi
    if ! validate_unmatched_cargo_test_results "$unmatched_fixtures_captured_output" no-artifact-mode; then
        validation_status=1
    fi
    rm -f -- "$unmatched_fixtures_captured_output"
fi
finish_stage_context unmatched-fixtures "$unmatched_fixtures_test_status"

if [ -n "$artifact_dir" ]; then
    for test_result_receipt in \
        "$artifact_dir/test-result-validation.txt" \
        "$artifact_dir/direct-tuple-get-l1-test-result-validation.txt" \
        "$artifact_dir/direct-print-result-test-result-validation.txt" \
        "$artifact_dir/computed-key-string-get-test-result-validation.txt" \
        "$artifact_dir/keys-snapshot-test-result-validation.txt" \
        "$artifact_dir/values-heterogeneous-test-result-validation.txt" \
        "$artifact_dir/unmatched-fixtures-test-result-validation.txt"; do
        if ! grep -Fxq 'validation=PASS' "$test_result_receipt" 2>/dev/null; then
            printf 'required Cargo test-result receipt is missing or failed: %s\n' \
                "$test_result_receipt" >&2
            validation_status=1
        fi
    done
    source_snapshot_manifest > "$artifact_dir/source-snapshot.after.manifest.sha256"
    snapshot_after="$(sha256sum -- "$artifact_dir/source-snapshot.after.manifest.sha256" | awk '{print $1}')"
    printf '%s\n' "$snapshot_after" > "$artifact_dir/source-snapshot.after.sha256"
else
    snapshot_after="$(source_snapshot_manifest | sha256sum | awk '{print $1}')"
fi
printf 'source_snapshot_sha256_after=%s\n' "$snapshot_after"
if [ "$snapshot_before" != "$snapshot_after" ]; then
    printf 'source snapshot changed during the strict Map v2 L3 run\n' >&2
    snapshot_status=98
    if [ -n "$artifact_dir" ]; then
        printf 'FAIL source snapshot changed\n' > "$artifact_dir/artifact-validation.txt"
    fi
fi

if [ -n "$artifact_dir" ]; then
    if [ "$test_status" -eq 0 ]; then
        summary="$artifact_dir/native/run-summary.txt"
        if [ ! -s "$summary" ]; then
            printf 'successful L3 test left no native run summary\n' >&2
            validation_status=1
        else
            case_count="$(sed -n 's/^case_count=//p' "$summary")"
            build_count="$(sed -n 's/^native_build_count=//p' "$summary")"
            memcheck_count="$(sed -n 's/^memcheck_run_count=//p' "$summary")"
            default_receipt_count="$(sed -n 's/^default_route_receipt_count=//p' "$summary")"
            explicit_census_count="$(sed -n 's/^explicit_mir_census_count=//p' "$summary")"
            fallback_count="$(sed -n 's/^legacy_fallback_count=//p' "$summary")"
            grep -Fq 'route_profile=map-any-owner-v2' "$summary" || validation_status=1
            if [[ "$case_count" =~ ^[1-9][0-9]*$ ]]; then
                case_total=$((10#$case_count))
                expected_build_count="$((2 * case_total))"
                expected_invocation_count="$((2 + 5 * case_total))"
            else
                case_total=0
                expected_build_count=-1
                expected_invocation_count=-1
                validation_status=1
            fi
            [[ "$build_count" == "$expected_build_count" ]] || validation_status=1
            [[ "$memcheck_count" == "$build_count" ]] || validation_status=1
            [[ "$default_receipt_count" == "$case_count" ]] || validation_status=1
            [[ "$explicit_census_count" == "$case_count" ]] || validation_status=1
            [[ "$fallback_count" == 0 ]] || validation_status=1
            program_count="$(find "$artifact_dir/native" -type f -name program | wc -l)"
            log_count="$(find "$artifact_dir/native" -type f -name valgrind.log | wc -l)"
            summary_count="$(find "$artifact_dir/native" -type f -name memcheck-summary.txt | wc -l)"
            invocation_count="$(find "$artifact_dir/native" -type f -name command.argv.json | wc -l)"
            status_count="$(find "$artifact_dir/native" -type f -name process.status | wc -l)"
            output_count="$(find "$artifact_dir/native" -type f -name stdout.bin | wc -l)"
            error_output_count="$(find "$artifact_dir/native" -type f -name stderr.bin | wc -l)"
            [[ "$program_count" == "$build_count" ]] || validation_status=1
            [[ "$log_count" == "$memcheck_count" ]] || validation_status=1
            [[ "$summary_count" == "$memcheck_count" ]] || validation_status=1
            [[ "$invocation_count" == "$expected_invocation_count" ]] || validation_status=1
            [[ "$status_count" == "$expected_invocation_count" ]] || validation_status=1
            [[ "$output_count" == "$expected_invocation_count" ]] || validation_status=1
            [[ "$error_output_count" == "$expected_invocation_count" ]] || validation_status=1
            non_executable_count="$(find "$artifact_dir/native" -type f -name program ! -perm -111 | wc -l)"
            [[ "$non_executable_count" == 0 ]] || validation_status=1
            mimi_identity="$artifact_dir/native/mimi-binary-identity.txt"
            mimi_hash_directory="$artifact_dir/native/mimi-binary-sha256"
            mimi_canonical_path="$(sed -n 's/^canonical_path=//p' "$mimi_identity" 2>/dev/null || true)"
            mimi_sha256="$(sed -n 's/^sha256=//p' "$mimi_identity" 2>/dev/null || true)"
            if [ ! -s "$mimi_identity" ] \
                || [[ ! "$mimi_canonical_path" = /* ]] \
                || [[ ! "$mimi_sha256" =~ ^[0-9a-fA-F]{64}$ ]] \
                || ! grep -Fxq 'exit_code=0' "$mimi_hash_directory/process.status" 2>/dev/null \
                || ! grep -Fq 'sha256sum' "$mimi_hash_directory/command.argv.json" 2>/dev/null \
                || ! grep -Fq "$mimi_sha256  $mimi_canonical_path" "$mimi_hash_directory/stdout.bin" 2>/dev/null; then
                printf 'actual Cargo mimi binary path/hash evidence is missing or inconsistent\n' >&2
                validation_status=1
            fi
            while IFS= read -r summary_file; do
                for expected in \
                    'definitely lost: 0 bytes in 0 blocks' \
                    'indirectly lost: 0 bytes in 0 blocks' \
                    'possibly lost: 0 bytes in 0 blocks' \
                    'ERROR SUMMARY: 0 errors'; do
                    if ! grep -Fq "$expected" "$summary_file"; then
                        printf 'strict Memcheck summary lacks "%s": %s\n' "$expected" "$summary_file" >&2
                        validation_status=1
                    fi
                done
            done < <(find "$artifact_dir/native" -type f -name memcheck-summary.txt -print | sort)
            case_count_seen=0
            while IFS= read -r case_directory; do
                case_count_seen=$((case_count_seen + 1))
                receipt_digest="$(sed -n 's/^mir_digest=//p' "$case_directory/receipt/stdout.bin" || true)"
                default_stderr="$case_directory/default-build/stderr.bin"
                explicit_stderr="$case_directory/explicit-build/stderr.bin"
                for command_status in \
                    "$case_directory/receipt/process.status" \
                    "$case_directory/default-build/process.status" \
                    "$case_directory/explicit-build/process.status"; do
                    if ! grep -Fxq 'exit_code=0' "$command_status"; then
                        printf 'required MIR/build command did not exit successfully: %s\n' "$command_status" >&2
                        validation_status=1
                    fi
                done
                for mode in default explicit; do
                    valgrind_command="$case_directory/${mode}-memcheck/command.argv.json"
                    for required_argument in \
                        '"--leak-check=full"' \
                        '"--show-leak-kinds=all"' \
                        '"--errors-for-leak-kinds=definite,indirect,possible"' \
                        '"--error-exitcode=97"'; do
                        if ! grep -Fq "$required_argument" "$valgrind_command"; then
                            printf 'strict Valgrind command omits %s: %s\n' "$required_argument" "$valgrind_command" >&2
                            validation_status=1
                        fi
                    done
                done
                if [ -z "$receipt_digest" ] \
                    || [ "$(grep -Fc "canonical route disposition: canonical (map-any-owner-v2) mir_digest=$receipt_digest" "$default_stderr" || true)" -ne 1 ] \
                    || grep -Fq 'canonical route disposition: legacy' "$default_stderr"; then
                    printf 'default native build lacks its own canonical MapAnyOwner v2 receipt: %s\n' "$default_stderr" >&2
                    validation_status=1
                fi
                if [ "$(grep -Fc 'info: canonical MIR native compiled ' "$explicit_stderr" || true)" -ne 1 ] \
                    || ! grep -Fq ' function(s), 0 fell back; mir_digest=' "$explicit_stderr" \
                    || ! grep -Fq "mir_digest=$receipt_digest" "$explicit_stderr" \
                    || grep -Fq 'canonical route disposition: legacy' "$explicit_stderr"; then
                    printf 'explicit native build lacks its own complete no-fallback MIR census: %s\n' "$explicit_stderr" >&2
                    validation_status=1
                fi
                for mode in default explicit; do
                    memcheck_directory="$case_directory/${mode}-memcheck"
                    expected_status="$(sed -n 's/^expected_exit_code=//p' "$memcheck_directory/expected-native-result.txt" || true)"
                    observed_status="$(sed -n 's/^exit_code=//p' "$memcheck_directory/process.status" || true)"
                    if [ "$expected_status" != "$observed_status" ] \
                        || ! cmp -s "$memcheck_directory/expected-native-stdout.bin" "$memcheck_directory/stdout.bin"; then
                        printf 'native runtime result differs from its expected status/stdout: %s\n' "$memcheck_directory" >&2
                        validation_status=1
                    fi
                done
            done < <(find "$artifact_dir/native" -mindepth 1 -maxdepth 1 -type d -name 'case-*' -print | sort)
            [[ "$case_count_seen" == "$case_total" ]] || validation_status=1
        fi
    fi

    if [ "$direct_tuple_get_l1_test_status" -eq 0 ]; then
        direct_tuple_summary="$artifact_dir/direct-tuple-get-l1/native/run-summary.txt"
        if [ ! -s "$direct_tuple_summary" ]; then
            printf 'successful direct tuple Get L1 test left no native run summary\n' >&2
            validation_status=1
        else
            direct_tuple_native="$artifact_dir/direct-tuple-get-l1/native"
            direct_tuple_mir_digest="$(sed -n 's/^mir_digest=//p' "$direct_tuple_summary")"
            direct_tuple_receipt_identity="$(sed -n 's/^receipt_identity_digest=//p' "$direct_tuple_summary")"
            for expected in \
                'case_count=1' \
                'native_build_count=2' \
                'memcheck_run_count=2' \
                'default_route_receipt_count=1' \
                'explicit_route_receipt_count=1' \
                'legacy_fallback_count=0'; do
                if ! grep -Fxq "$expected" "$direct_tuple_summary"; then
                    printf 'direct tuple Get L1 summary lacks %s: %s\n' "$expected" "$direct_tuple_summary" >&2
                    validation_status=1
                fi
            done
            grep -Fxq 'route_profile=ordinary-map-owned-any-v2-direct-tuple-get-unpack-l1-core-v1' \
                "$direct_tuple_summary" || validation_status=1
            [[ "$direct_tuple_mir_digest" =~ ^[0-9a-fA-F]{64}$ ]] || validation_status=1
            [[ "$direct_tuple_receipt_identity" =~ ^[0-9a-fA-F]{64}$ ]] || validation_status=1
            [[ "$(find "$direct_tuple_native" -type f -name program | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$direct_tuple_native" -type f -name valgrind.log | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$direct_tuple_native" -type f -name memcheck-summary.txt | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$direct_tuple_native" -type f -name command.argv.json | wc -l)" == 4 ]] || validation_status=1
            [[ "$(find "$direct_tuple_native" -type f -name process.status | wc -l)" == 4 ]] || validation_status=1
            [[ "$(find "$direct_tuple_native" -type f -name stdout.bin | wc -l)" == 4 ]] || validation_status=1
            [[ "$(find "$direct_tuple_native" -type f -name stderr.bin | wc -l)" == 4 ]] || validation_status=1
            while IFS= read -r summary_file; do
                for expected in \
                    'definitely lost: 0 bytes in 0 blocks' \
                    'indirectly lost: 0 bytes in 0 blocks' \
                    'possibly lost: 0 bytes in 0 blocks' \
                    'ERROR SUMMARY: 0 errors'; do
                    if ! grep -Fq "$expected" "$summary_file"; then
                        printf 'direct tuple Get L1 strict Memcheck summary lacks %s: %s\n' "$expected" "$summary_file" >&2
                        validation_status=1
                    fi
                done
                [[ "$(grep -Fc 'still reachable:' "$summary_file" || true)" == 1 ]] || validation_status=1
            done < <(find "$direct_tuple_native" -type f -name memcheck-summary.txt -print | sort)
            for mode in default explicit; do
                case_directory="$direct_tuple_native/$mode"
                build_stderr="$case_directory/stderr.bin"
                if ! grep -Fq 'ordinary-map-owned-any-v2-direct-tuple-get-unpack-l1-core-v1' "$build_stderr" \
                    || [ "$(grep -Fc 'MIMI_ROUTE_CENSUS route-admission ' "$build_stderr" || true)" -ne 1 ] \
                    || [ "$(grep -Fc 'MIMI_ROUTE_CENSUS canonical-native-build ' "$build_stderr" || true)" -ne 1 ] \
                    || grep -Fq 'canonical route disposition: legacy' "$build_stderr" \
                    || grep -Fq 'VerifierOnly' "$build_stderr"; then
                    printf 'direct tuple Get L1 %s build lacks its exact no-fallback execution route receipt\n' "$mode" >&2
                    validation_status=1
                fi
                route_identity="$(sed -n 's/.*receipt_identity_digest=\([^ ]*\).*/\1/p' "$build_stderr")"
                [[ "$route_identity" == "$direct_tuple_receipt_identity" ]] || validation_status=1
                [[ "$(sed -n 's/^exit_code=//p' "$case_directory/memcheck/process.status")" == 0 ]] || validation_status=1
                cmp -s "$case_directory/memcheck/expected-native-stdout.bin" "$case_directory/memcheck/stdout.bin" || validation_status=1
                valgrind_command="$case_directory/memcheck/command.argv.json"
                for required_argument in \
                    '"--leak-check=full"' \
                    '"--show-leak-kinds=all"' \
                    '"--errors-for-leak-kinds=definite,indirect,possible"' \
                    '"--error-exitcode=97"'; do
                    grep -Fq "$required_argument" "$valgrind_command" || validation_status=1
                done
            done
            [[ "$(find "$direct_tuple_native" -type f -name program ! -perm -111 | wc -l)" == 0 ]] || validation_status=1
        fi
    fi

    if [ "$direct_print_result_test_status" -eq 0 ]; then
        direct_summary="$artifact_dir/direct-print-result/native/run-summary.txt"
        if [ ! -s "$direct_summary" ]; then
            printf 'successful DirectPrintResult L3 test left no native run summary\n' >&2
            validation_status=1
        else
            direct_case_count="$(sed -n 's/^case_count=//p' "$direct_summary")"
            direct_build_count="$(sed -n 's/^native_build_count=//p' "$direct_summary")"
            direct_memcheck_count="$(sed -n 's/^memcheck_run_count=//p' "$direct_summary")"
            direct_receipt_count="$(sed -n 's/^default_route_receipt_count=//p' "$direct_summary")"
            direct_explicit_receipt_count="$(sed -n 's/^explicit_route_receipt_count=//p' "$direct_summary")"
            direct_census_count="$(sed -n 's/^explicit_mir_census_count=//p' "$direct_summary")"
            direct_identity_match_count="$(sed -n 's/^checker_mir_build_identity_match_count=//p' "$direct_summary")"
            direct_fallback_count="$(sed -n 's/^legacy_fallback_count=//p' "$direct_summary")"
            grep -Fxq 'route_profile=map-any-owner-v2-direct-print-result-v1' "$direct_summary" || validation_status=1
            grep -Fxq 'global_map_any_v2_production_ready=false' "$direct_summary" || validation_status=1
            if [[ "$direct_case_count" =~ ^[1-9][0-9]*$ ]]; then
                direct_case_total=$((10#$direct_case_count))
                [[ "$direct_case_total" == 4 ]] || validation_status=1
                expected_direct_build_count="$((2 * direct_case_total))"
                expected_direct_invocation_count="$((2 + 4 * direct_case_total))"
            else
                direct_case_total=0
                expected_direct_build_count=-1
                expected_direct_invocation_count=-1
                validation_status=1
            fi
            [[ "$direct_build_count" == "$expected_direct_build_count" ]] || validation_status=1
            [[ "$direct_memcheck_count" == "$direct_build_count" ]] || validation_status=1
            [[ "$direct_receipt_count" == "$direct_case_count" ]] || validation_status=1
            [[ "$direct_explicit_receipt_count" == "$direct_case_count" ]] || validation_status=1
            [[ "$direct_census_count" == "$direct_case_count" ]] || validation_status=1
            [[ "$direct_identity_match_count" == "$direct_case_count" ]] || validation_status=1
            [[ "$direct_fallback_count" == 0 ]] || validation_status=1
            direct_native="$artifact_dir/direct-print-result/native"
            direct_program_count="$(find "$direct_native" -type f -name program | wc -l)"
            direct_log_count="$(find "$direct_native" -type f -name valgrind.log | wc -l)"
            direct_memcheck_summary_count="$(find "$direct_native" -type f -name memcheck-summary.txt | wc -l)"
            direct_invocation_count="$(find "$direct_native" -type f -name command.argv.json | wc -l)"
            direct_status_count="$(find "$direct_native" -type f -name process.status | wc -l)"
            direct_output_count="$(find "$direct_native" -type f -name stdout.bin | wc -l)"
            direct_error_output_count="$(find "$direct_native" -type f -name stderr.bin | wc -l)"
            [[ "$direct_program_count" == "$direct_build_count" ]] || validation_status=1
            [[ "$direct_log_count" == "$direct_memcheck_count" ]] || validation_status=1
            [[ "$direct_memcheck_summary_count" == "$direct_memcheck_count" ]] || validation_status=1
            [[ "$direct_invocation_count" == "$expected_direct_invocation_count" ]] || validation_status=1
            [[ "$direct_status_count" == "$expected_direct_invocation_count" ]] || validation_status=1
            [[ "$direct_output_count" == "$expected_direct_invocation_count" ]] || validation_status=1
            [[ "$direct_error_output_count" == "$expected_direct_invocation_count" ]] || validation_status=1
            while IFS= read -r summary_file; do
                for expected in \
                    'definitely lost: 0 bytes in 0 blocks' \
                    'indirectly lost: 0 bytes in 0 blocks' \
                    'possibly lost: 0 bytes in 0 blocks' \
                    'ERROR SUMMARY: 0 errors'; do
                    if ! grep -Fq "$expected" "$summary_file"; then
                        printf 'DirectPrintResult Memcheck summary lacks "%s": %s\n' "$expected" "$summary_file" >&2
                        validation_status=1
                    fi
                done
                if [ "$(grep -Fc 'still reachable:' "$summary_file" || true)" -ne 1 ]; then
                    printf 'DirectPrintResult Memcheck summary omits or repeats still reachable: %s\n' "$summary_file" >&2
                    validation_status=1
                fi
            done < <(find "$direct_native" -type f -name memcheck-summary.txt -print | sort)
            direct_case_count_seen=0
            while IFS= read -r case_directory; do
                direct_case_count_seen=$((direct_case_count_seen + 1))
                default_stderr="$case_directory/default-build/stderr.bin"
                explicit_stderr="$case_directory/explicit-build/stderr.bin"
                for command_status in \
                    "$case_directory/default-build/process.status" \
                    "$case_directory/explicit-build/process.status"; do
                    if ! grep -Fxq 'exit_code=0' "$command_status"; then
                        printf 'DirectPrintResult production build did not exit successfully: %s\n' "$command_status" >&2
                        validation_status=1
                    fi
                done
                for mode in default explicit; do
                    valgrind_command="$case_directory/${mode}-memcheck/command.argv.json"
                    for required_argument in \
                        '"--leak-check=full"' \
                        '"--show-leak-kinds=all"' \
                        '"--errors-for-leak-kinds=definite,indirect,possible"' \
                        '"--error-exitcode=97"' \
                        '"--log-file='; do
                        if ! grep -Fq "$required_argument" "$valgrind_command"; then
                            printf 'DirectPrintResult strict Valgrind command omits %s: %s\n' "$required_argument" "$valgrind_command" >&2
                            validation_status=1
                        fi
                    done
                done
                route_profile='map-any-owner-v2-direct-print-result-v1'
                default_admission_count="$(grep -Fc 'MIMI_ROUTE_CENSUS route-admission consumer=build mode=default ' "$default_stderr" || true)"
                explicit_admission_count="$(grep -Fc 'MIMI_ROUTE_CENSUS route-admission consumer=build mode=explicit-mir ' "$explicit_stderr" || true)"
                default_native_receipt_count="$(grep -Fc 'MIMI_ROUTE_CENSUS canonical-native-build consumer=' "$default_stderr" || true)"
                explicit_native_receipt_count="$(grep -Fc 'MIMI_ROUTE_CENSUS canonical-native-build consumer=' "$explicit_stderr" || true)"
                receipt_digest="$(sed -n 's/.*MIMI_ROUTE_CENSUS route-admission .*mir_digest=\([^ ]*\).*/\1/p' "$default_stderr")"
                default_route_identity="$(sed -n 's/.*MIMI_ROUTE_CENSUS route-admission .*receipt_identity_digest=\([^ ]*\).*/\1/p' "$default_stderr")"
                explicit_route_identity="$(sed -n 's/.*MIMI_ROUTE_CENSUS route-admission .*receipt_identity_digest=\([^ ]*\).*/\1/p' "$explicit_stderr")"
                explicit_receipt_digest="$(sed -n 's/.*MIMI_ROUTE_CENSUS route-admission .*mir_digest=\([^ ]*\).*/\1/p' "$explicit_stderr")"
                if [ -z "$receipt_digest" ] \
                    || [[ ! "$receipt_digest" =~ ^[0-9a-fA-F]{64}$ ]] \
                    || [ "$default_admission_count" -ne 1 ] \
                    || [ "$explicit_admission_count" -ne 1 ] \
                    || [ "$default_native_receipt_count" -ne 1 ] \
                    || [ "$explicit_native_receipt_count" -ne 1 ] \
                    || [ "$explicit_receipt_digest" != "$receipt_digest" ] \
                    || [ "$default_route_identity" != "$explicit_route_identity" ] \
                    || [[ ! "$default_route_identity" =~ ^[0-9a-fA-F]{64}$ ]] \
                    || ! grep -Fq "checker_profile=$route_profile build_route_profile=$route_profile mir_digest=$receipt_digest" "$default_stderr" \
                    || ! grep -Fq "checker_profile=$route_profile build_route_profile=$route_profile mir_digest=$receipt_digest" "$explicit_stderr" \
                    || ! grep -Fq "canonical-native-build consumer=" "$default_stderr" \
                    || ! grep -Fq "route_id=$route_profile build_route_profile=$route_profile" "$default_stderr" \
                    || ! grep -Fq "route_id=$route_profile build_route_profile=$route_profile" "$explicit_stderr" \
                    || ! grep -Fq " profile=$route_profile " "$default_stderr" \
                    || ! grep -Fq " profile=$route_profile " "$explicit_stderr" \
                    || ! grep -Fq "route_identity_digest=$default_route_identity" "$default_stderr" \
                    || ! grep -Fq "route_identity_digest=$explicit_route_identity" "$explicit_stderr" \
                    || ! grep -Fq "receipt_identity_digest=$default_route_identity" "$default_stderr" \
                    || ! grep -Fq "receipt_identity_digest=$explicit_route_identity" "$explicit_stderr" \
                    || ! grep -Fq "canonical route disposition: canonical ($route_profile) mir_digest=$receipt_digest" "$default_stderr" \
                    || grep -Fq 'canonical route disposition: legacy' "$default_stderr"; then
                    printf 'DirectPrintResult default/explicit builds lack the same exact Checker-bound production receipt: %s\n' "$case_directory" >&2
                    validation_status=1
                fi
                if [ "$(grep -Fc 'info: canonical MIR native compiled ' "$default_stderr" || true)" -ne 1 ] \
                    || [ "$(grep -Fc 'info: canonical MIR native compiled ' "$explicit_stderr" || true)" -ne 1 ] \
                    || ! grep -Fq ' function(s), 0 fell back; mir_digest=' "$default_stderr" \
                    || ! grep -Fq ' function(s), 0 fell back; mir_digest=' "$explicit_stderr" \
                    || ! grep -Fq "mir_digest=$receipt_digest" "$default_stderr" \
                    || ! grep -Fq "mir_digest=$receipt_digest" "$explicit_stderr" \
                    || ! grep -Fq ' function(s), 0 fell back; mir_digest=' "$explicit_stderr" \
                    || grep -Fq 'canonical route disposition: legacy' "$default_stderr" \
                    || grep -Fq 'canonical route disposition: legacy' "$explicit_stderr"; then
                    printf 'DirectPrintResult default/explicit builds lack a complete no-fallback native MIR census: %s\n' "$case_directory" >&2
                    validation_status=1
                fi
                for mode in default explicit; do
                    memcheck_directory="$case_directory/${mode}-memcheck"
                    expected_status="$(sed -n 's/^expected_exit_code=//p' "$memcheck_directory/expected-native-result.txt" || true)"
                    observed_status="$(sed -n 's/^exit_code=//p' "$memcheck_directory/process.status" || true)"
                    if [ "$expected_status" != "$observed_status" ] \
                        || ! cmp -s "$memcheck_directory/expected-native-stdout.bin" "$memcheck_directory/stdout.bin"; then
                        printf 'DirectPrintResult native result differs from expected status/stdout: %s\n' "$memcheck_directory" >&2
                        validation_status=1
                    fi
                done
            done < <(find "$direct_native" -mindepth 1 -maxdepth 1 -type d -name 'case-*' -print | sort)
            [[ "$direct_case_count_seen" == "$direct_case_total" ]] || validation_status=1
        fi
    fi

    if [ "$computed_key_string_test_status" -eq 0 ]; then
        computed_string_native="$artifact_dir/computed-key-string-get/native"
        computed_string_summary="$computed_string_native/run-summary.txt"
        if [ ! -s "$computed_string_summary" ]; then
            printf 'successful String ComputedKeyGet L3 test left no native run summary\n' >&2
            validation_status=1
        else
            grep -Fxq 'route_profile=map-any-owner-v2-computed-key-get-v1' "$computed_string_summary" || validation_status=1
            grep -Fxq 'global_map_any_v2_production_ready=false' "$computed_string_summary" || validation_status=1
            grep -Fxq 'case_count=1' "$computed_string_summary" || validation_status=1
            grep -Fxq 'native_build_count=2' "$computed_string_summary" || validation_status=1
            grep -Fxq 'control_native_build_count=1' "$computed_string_summary" || validation_status=1
            grep -Fxq 'total_native_build_count=3' "$computed_string_summary" || validation_status=1
            grep -Fxq 'memcheck_run_count=3' "$computed_string_summary" || validation_status=1
            grep -Fxq 'target_memcheck_run_count=2' "$computed_string_summary" || validation_status=1
            grep -Fxq 'control_memcheck_run_count=1' "$computed_string_summary" || validation_status=1
            grep -Fxq 'default_route_receipt_count=1' "$computed_string_summary" || validation_status=1
            grep -Fxq 'control_route_receipt_count=1' "$computed_string_summary" || validation_status=1
            grep -Fxq 'explicit_mir_census_count=1' "$computed_string_summary" || validation_status=1
            grep -Fxq 'legacy_fallback_count=0' "$computed_string_summary" || validation_status=1
            vg_launcher_path="$(sed -n 's/^valgrind_launcher_path=//p' "$computed_string_summary")"
            vg_engine_path="$(sed -n 's/^valgrind_engine_path=//p' "$computed_string_summary")"
            vg_version="$(sed -n 's/^valgrind_version=//p' "$computed_string_summary")"
            vg_identity="$computed_string_native/valgrind-identity/valgrind-identity.txt"
            vg_identity_launcher_path="$(sed -n 's/^launcher_path=//p' "$vg_identity" 2>/dev/null || true)"
            vg_identity_launcher_hash="$(sed -n 's/^launcher_sha256=//p' "$vg_identity" 2>/dev/null || true)"
            vg_identity_engine_path="$(sed -n 's/^engine_path=//p' "$vg_identity" 2>/dev/null || true)"
            vg_identity_engine_hash="$(sed -n 's/^engine_sha256=//p' "$vg_identity" 2>/dev/null || true)"
            vg_identity_version="$(sed -n 's/^version=//p' "$vg_identity" 2>/dev/null || true)"
            vg_version_stdout="$(cat "$computed_string_native/valgrind-version/stdout.bin" 2>/dev/null || true)"
            if [[ ! "$vg_launcher_path" = /* ]] \
                || [[ ! "$vg_engine_path" = /* ]] \
                || [[ ! "$vg_identity_launcher_path" = /* ]] \
                || [[ ! "$vg_identity_engine_path" = /* ]] \
                || [[ ! "$vg_identity_launcher_hash" =~ ^[0-9a-fA-F]{64}$ ]] \
                || [[ ! "$vg_identity_engine_hash" =~ ^[0-9a-fA-F]{64}$ ]] \
                || [[ "$vg_launcher_path" != "$vg_identity_launcher_path" ]] \
                || [[ "$vg_engine_path" != "$vg_identity_engine_path" ]] \
                || [[ "$vg_version" != "$vg_identity_version" ]] \
                || [[ "$vg_version" != "$(printf '%s' "$vg_version_stdout" | tr -d '\r\n')" ]] \
                || [[ ! "$vg_version" = valgrind-* ]]; then
                printf 'String ComputedKeyGet strict Valgrind path/hash/version identity is missing or inconsistent\n' >&2
                validation_status=1
            fi
            if ! grep -Fxq 'exit_code=0' "$computed_string_native/valgrind-version/process.status" 2>/dev/null \
                || ! grep -Fq "\"$vg_launcher_path\", \"--version\"" "$computed_string_native/valgrind-version/command.argv.json" 2>/dev/null; then
                printf 'canonical Valgrind launcher version command evidence is missing or inconsistent\n' >&2
                validation_status=1
            fi
            launcher_hash_directory="$computed_string_native/valgrind-identity/launcher-sha256"
            if ! grep -Fxq 'exit_code=0' "$launcher_hash_directory/process.status" 2>/dev/null \
                || ! grep -Fq 'sha256sum' "$launcher_hash_directory/command.argv.json" 2>/dev/null \
                || ! grep -Fq "$vg_identity_launcher_hash  $vg_identity_launcher_path" "$launcher_hash_directory/stdout.bin" 2>/dev/null; then
                printf 'canonical Valgrind launcher SHA-256 command evidence is missing or inconsistent\n' >&2
                validation_status=1
            fi
            if [[ "$vg_launcher_path" != "$vg_engine_path" ]]; then
                engine_hash_directory="$computed_string_native/valgrind-identity/engine-sha256"
                if ! grep -Fxq 'exit_code=0' "$engine_hash_directory/process.status" 2>/dev/null \
                    || ! grep -Fq 'sha256sum' "$engine_hash_directory/command.argv.json" 2>/dev/null \
                    || ! grep -Fq "$vg_identity_engine_hash  $vg_identity_engine_path" "$engine_hash_directory/stdout.bin" 2>/dev/null; then
                    printf 'canonical Valgrind ELF engine SHA-256 command evidence is missing or inconsistent\n' >&2
                    validation_status=1
                fi
                if ! grep -Fxq 'exit_code=0' "$computed_string_native/valgrind-identity/engine-version/process.status" 2>/dev/null \
                    || [[ "$(tr -d '\r\n' < "$computed_string_native/valgrind-identity/engine-version/stdout.bin" 2>/dev/null || true)" != "$vg_identity_version" ]]; then
                    printf 'canonical Valgrind ELF engine version command evidence is missing or inconsistent\n' >&2
                    validation_status=1
                fi
            fi
            if [[ "$vg_launcher_path" == "$vg_engine_path" ]]; then
                expected_invocations=11
            else
                expected_invocations=13
            fi
            control_case="$computed_string_native/control-i32-computed-key-get"
            computed_string_case="$computed_string_native/case-00-owned-string"
            computed_string_receipt_digest="$(sed -n 's/^mir_digest=//p' "$computed_string_case/receipt/stdout.bin" || true)"
            control_receipt_digest="$(sed -n 's/^mir_digest=//p' "$control_case/receipt/stdout.bin" || true)"
            for command_status in \
                "$control_case/receipt/process.status" \
                "$control_case/default-build/process.status" \
                "$control_case/default-memcheck/process.status" \
                "$computed_string_case/receipt/process.status" \
                "$computed_string_case/default-build/process.status" \
                "$computed_string_case/explicit-build/process.status"; do
                if ! grep -Fxq 'exit_code=0' "$command_status"; then
                    printf 'String ComputedKeyGet MIR/build command failed: %s\n' "$command_status" >&2
                    validation_status=1
                fi
            done
            if [ -z "$control_receipt_digest" ] \
                || [ "$(grep -Fc "canonical route disposition: canonical (map-any-owner-v2-computed-key-get-v1) mir_digest=$control_receipt_digest" "$control_case/default-build/stderr.bin" || true)" -ne 1 ] \
                || grep -Fq 'canonical route disposition: legacy' "$control_case/default-build/stderr.bin"; then
                printf 'i32 still-reachable control build lacks its own no-fallback ComputedKeyGet receipt\n' >&2
                validation_status=1
            fi
            control_memcheck_command="$control_case/default-memcheck/command.argv.json"
            if ! grep -Fq "\"$vg_launcher_path\"" "$control_memcheck_command"; then
                printf 'i32 control did not execute through the recorded canonical Valgrind path\n' >&2
                validation_status=1
            fi
            for required_argument in \
                '"--leak-check=full"' \
                '"--show-leak-kinds=all"' \
                '"--errors-for-leak-kinds=definite,indirect,possible"' \
                '"--error-exitcode=97"' \
                '"--log-file='; do
                if ! grep -Fq "$required_argument" "$control_memcheck_command"; then
                    printf 'i32 control strict Valgrind command omits %s\n' "$required_argument" >&2
                    validation_status=1
                fi
            done
            if [ -z "$computed_string_receipt_digest" ] \
                || [ "$(grep -Fc "canonical route disposition: canonical (map-any-owner-v2-computed-key-get-v1) mir_digest=$computed_string_receipt_digest" "$computed_string_case/default-build/stderr.bin" || true)" -ne 1 ] \
                || grep -Fq 'canonical route disposition: legacy' "$computed_string_case/default-build/stderr.bin"; then
                printf 'String ComputedKeyGet default build lacks its exact route receipt\n' >&2
                validation_status=1
            fi
            if [ "$(grep -Fc 'info: canonical MIR native compiled ' "$computed_string_case/explicit-build/stderr.bin" || true)" -ne 1 ] \
                || ! grep -Fq ' function(s), 0 fell back; mir_digest=' "$computed_string_case/explicit-build/stderr.bin" \
                || ! grep -Fq "mir_digest=$computed_string_receipt_digest" "$computed_string_case/explicit-build/stderr.bin" \
                || grep -Fq 'canonical route disposition: legacy' "$computed_string_case/explicit-build/stderr.bin"; then
                printf 'String ComputedKeyGet explicit build lacks its no-fallback MIR census\n' >&2
                validation_status=1
            fi
            for mode in default explicit; do
                memcheck_directory="$computed_string_case/${mode}-memcheck"
                valgrind_command="$memcheck_directory/command.argv.json"
                if ! grep -Fq "\"$vg_launcher_path\"" "$valgrind_command"; then
                    printf 'String ComputedKeyGet %s did not execute through the recorded canonical Valgrind path\n' "$mode" >&2
                    validation_status=1
                fi
                for required_argument in \
                    '"--leak-check=full"' \
                    '"--show-leak-kinds=all"' \
                    '"--errors-for-leak-kinds=definite,indirect,possible"' \
                    '"--error-exitcode=97"' \
                    '"--log-file='; do
                    if ! grep -Fq "$required_argument" "$valgrind_command"; then
                        printf 'String ComputedKeyGet strict Valgrind command omits %s\n' "$required_argument" >&2
                        validation_status=1
                    fi
                done
                for expected in \
                    'definitely lost: 0 bytes in 0 blocks' \
                    'indirectly lost: 0 bytes in 0 blocks' \
                    'possibly lost: 0 bytes in 0 blocks' \
                    'ERROR SUMMARY: 0 errors'; do
                    if ! grep -Fq "$expected" "$memcheck_directory/memcheck-summary.txt"; then
                        printf 'String ComputedKeyGet Memcheck summary lacks %s\n' "$expected" >&2
                        validation_status=1
                    fi
                done
                if [ "$(grep -Ec '^still reachable: [0-9,]+ bytes in [0-9,]+ blocks' "$memcheck_directory/memcheck-summary.txt" || true)" -ne 1 ]; then
                    printf 'String ComputedKeyGet summary lacks exactly one parsed still-reachable bytes/blocks row: %s\n' "$memcheck_directory" >&2
                    validation_status=1
                fi
                expected_status="$(sed -n 's/^expected_exit_code=//p' "$memcheck_directory/expected-native-result.txt" || true)"
                observed_status="$(sed -n 's/^exit_code=//p' "$memcheck_directory/process.status" || true)"
                if [ "$expected_status" != "$observed_status" ] \
                    || ! cmp -s "$memcheck_directory/expected-native-stdout.bin" "$memcheck_directory/stdout.bin"; then
                    printf 'String ComputedKeyGet native result differs from expectation: %s\n' "$memcheck_directory" >&2
                    validation_status=1
                fi
            done
            control_memcheck_summary="$control_case/default-memcheck/memcheck-summary.txt"
            if [ "$(grep -Ec '^still reachable: [0-9,]+ bytes in [0-9,]+ blocks' "$control_memcheck_summary" || true)" -ne 1 ]; then
                printf 'i32 control summary lacks exactly one parsed still-reachable bytes/blocks row\n' >&2
                validation_status=1
            fi
            for expected in \
                'definitely lost: 0 bytes in 0 blocks' \
                'indirectly lost: 0 bytes in 0 blocks' \
                'possibly lost: 0 bytes in 0 blocks' \
                'ERROR SUMMARY: 0 errors'; do
                if ! grep -Fq "$expected" "$control_memcheck_summary"; then
                    printf 'i32 control Memcheck summary lacks %s\n' "$expected" >&2
                    validation_status=1
                fi
            done
            if [ "$(sed -n 's/^expected_exit_code=//p' "$control_case/default-memcheck/expected-native-result.txt" || true)" != \
                    "$(sed -n 's/^exit_code=//p' "$control_case/default-memcheck/process.status" || true)" ] \
                || ! cmp -s "$control_case/default-memcheck/expected-native-stdout.bin" "$control_case/default-memcheck/stdout.bin"; then
                printf 'i32 control native result differs from its recorded expectation\n' >&2
                validation_status=1
            fi
            comparison="$computed_string_native/still-reachable-comparison.txt"
            for required_comparison in \
                'baseline_still_reachable_bytes=' \
                'baseline_still_reachable_blocks=' \
                'default_still_reachable_bytes=' \
                'default_still_reachable_blocks=' \
                'explicit_still_reachable_bytes=' \
                'explicit_still_reachable_blocks='; do
                if ! grep -Eq "^${required_comparison}[0-9]+$" "$comparison" 2>/dev/null; then
                    printf 'still-reachable baseline comparison lacks %s\n' "$required_comparison" >&2
                    validation_status=1
                fi
            done
            baseline_counts="$(still_reachable_counts "$control_memcheck_summary" || true)"
            read -r baseline_actual_bytes baseline_actual_blocks <<< "$baseline_counts"
            baseline_recorded_bytes="$(sed -n 's/^baseline_still_reachable_bytes=//p' "$comparison" 2>/dev/null || true)"
            baseline_recorded_blocks="$(sed -n 's/^baseline_still_reachable_blocks=//p' "$comparison" 2>/dev/null || true)"
            summary_baseline_bytes="$(sed -n 's/^still_reachable_baseline_bytes=//p' "$computed_string_summary")"
            summary_baseline_blocks="$(sed -n 's/^still_reachable_baseline_blocks=//p' "$computed_string_summary")"
            if [[ "$baseline_actual_bytes" != "$baseline_recorded_bytes" ]] \
                || [[ "$baseline_actual_blocks" != "$baseline_recorded_blocks" ]] \
                || [[ "$summary_baseline_bytes" != "$baseline_actual_bytes" ]] \
                || [[ "$summary_baseline_blocks" != "$baseline_actual_blocks" ]]; then
                printf 'still-reachable baseline comparison does not match the control Memcheck report\n' >&2
                validation_status=1
            fi
            for mode in default explicit; do
                target_counts="$(still_reachable_counts "$computed_string_case/${mode}-memcheck/memcheck-summary.txt" || true)"
                read -r target_actual_bytes target_actual_blocks <<< "$target_counts"
                target_recorded_bytes="$(sed -n "s/^${mode}_still_reachable_bytes=//p" "$comparison" 2>/dev/null || true)"
                target_recorded_blocks="$(sed -n "s/^${mode}_still_reachable_blocks=//p" "$comparison" 2>/dev/null || true)"
                added_bytes="$(sed -n "s/^${mode}_added_retained_bytes=//p" "$comparison" 2>/dev/null || true)"
                added_blocks="$(sed -n "s/^${mode}_added_retained_blocks=//p" "$comparison" 2>/dev/null || true)"
                if [[ "$target_actual_bytes" != "$target_recorded_bytes" ]] \
                    || [[ "$target_actual_blocks" != "$target_recorded_blocks" ]] \
                    || [[ ! "$baseline_actual_bytes" =~ ^[0-9]+$ ]] \
                    || [[ ! "$baseline_actual_blocks" =~ ^[0-9]+$ ]] \
                    || [[ ! "$target_actual_bytes" =~ ^[0-9]+$ ]] \
                    || [[ ! "$target_actual_blocks" =~ ^[0-9]+$ ]] \
                    || (( 10#$target_actual_bytes > 10#$baseline_actual_bytes )) \
                    || (( 10#$target_actual_blocks > 10#$baseline_actual_blocks )) \
                    || [[ "$added_bytes" != 0 ]] \
                    || [[ "$added_blocks" != 0 ]]; then
                    printf 'String ComputedKeyGet %s retained still-reachable memory beyond its i32 baseline\n' "$mode" >&2
                    validation_status=1
                fi
            done
            for required_zero in \
                'default_added_retained_bytes=0' \
                'default_added_retained_blocks=0' \
                'explicit_added_retained_bytes=0' \
                'explicit_added_retained_blocks=0'; do
                if ! grep -Fxq "$required_zero" "$comparison" 2>/dev/null; then
                    printf 'still-reachable baseline comparison lacks %s\n' "$required_zero" >&2
                    validation_status=1
                fi
            done
            [[ "$(find "$computed_string_native" -type f -name program | wc -l)" == 3 ]] || validation_status=1
            [[ "$(find "$computed_string_native" -type f -name valgrind.log | wc -l)" == 3 ]] || validation_status=1
            [[ "$(find "$computed_string_native" -type f -name memcheck-summary.txt | wc -l)" == 3 ]] || validation_status=1
            [[ "$(find "$computed_string_native" -type f -name command.argv.json | wc -l)" == "$expected_invocations" ]] || validation_status=1
            [[ "$(find "$computed_string_native" -type f -name process.status | wc -l)" == "$expected_invocations" ]] || validation_status=1
            [[ "$(find "$computed_string_native" -type f -name stdout.bin | wc -l)" == "$expected_invocations" ]] || validation_status=1
            [[ "$(find "$computed_string_native" -type f -name stderr.bin | wc -l)" == "$expected_invocations" ]] || validation_status=1
        fi
    fi

    if [ "$keys_snapshot_test_status" -eq 0 ]; then
        keys_snapshot_root="$artifact_dir/keys-snapshot"
        keys_snapshot_native="$keys_snapshot_root/native"
        keys_snapshot_summary="$keys_snapshot_native/run-summary.txt"
        if [ ! -s "$keys_snapshot_summary" ]; then
            printf 'successful KeysSnapshot L3 test left no native run summary\n' >&2
            validation_status=1
        else
            grep -Fxq 'route_profile=map-any-owner-v2-keys-snapshot-v1' "$keys_snapshot_summary" || validation_status=1
            grep -Fxq 'global_map_any_v2_production_ready=false' "$keys_snapshot_summary" || validation_status=1
            grep -Fxq 'case_count=1' "$keys_snapshot_summary" || validation_status=1
            grep -Fxq 'native_build_count=2' "$keys_snapshot_summary" || validation_status=1
            grep -Fxq 'memcheck_run_count=2' "$keys_snapshot_summary" || validation_status=1
            grep -Fxq 'default_route_receipt_count=1' "$keys_snapshot_summary" || validation_status=1
            grep -Fxq 'explicit_mir_census_count=1' "$keys_snapshot_summary" || validation_status=1
            grep -Fxq 'legacy_fallback_count=0' "$keys_snapshot_summary" || validation_status=1

            vg_launcher_path="$(sed -n 's/^valgrind_launcher_path=//p' "$keys_snapshot_summary")"
            vg_engine_path="$(sed -n 's/^valgrind_engine_path=//p' "$keys_snapshot_summary")"
            vg_version="$(sed -n 's/^valgrind_version=//p' "$keys_snapshot_summary")"
            vg_identity="$keys_snapshot_native/valgrind-identity/valgrind-identity.txt"
            vg_identity_launcher_path="$(sed -n 's/^launcher_path=//p' "$vg_identity" 2>/dev/null || true)"
            vg_identity_launcher_hash="$(sed -n 's/^launcher_sha256=//p' "$vg_identity" 2>/dev/null || true)"
            vg_identity_engine_path="$(sed -n 's/^engine_path=//p' "$vg_identity" 2>/dev/null || true)"
            vg_identity_engine_hash="$(sed -n 's/^engine_sha256=//p' "$vg_identity" 2>/dev/null || true)"
            vg_identity_version="$(sed -n 's/^version=//p' "$vg_identity" 2>/dev/null || true)"
            vg_version_stdout="$(cat "$keys_snapshot_native/valgrind-version/stdout.bin" 2>/dev/null || true)"
            if [[ ! "$vg_launcher_path" = /* ]] \
                || [[ ! "$vg_engine_path" = /* ]] \
                || [[ ! "$vg_identity_launcher_path" = /* ]] \
                || [[ ! "$vg_identity_engine_path" = /* ]] \
                || [[ ! "$vg_identity_launcher_hash" =~ ^[0-9a-fA-F]{64}$ ]] \
                || [[ ! "$vg_identity_engine_hash" =~ ^[0-9a-fA-F]{64}$ ]] \
                || [[ "$vg_launcher_path" != "$vg_identity_launcher_path" ]] \
                || [[ "$vg_engine_path" != "$vg_identity_engine_path" ]] \
                || [[ "$vg_version" != "$vg_identity_version" ]] \
                || [[ "$vg_version" != "$(printf '%s' "$vg_version_stdout" | tr -d '\r\n')" ]] \
                || [[ ! "$vg_version" = valgrind-* ]]; then
                printf 'KeysSnapshot strict Valgrind path/hash/version identity is missing or inconsistent\n' >&2
                validation_status=1
            fi
            if ! grep -Fxq 'exit_code=0' "$keys_snapshot_native/valgrind-version/process.status" 2>/dev/null \
                || ! grep -Fq "\"$vg_launcher_path\", \"--version\"" "$keys_snapshot_native/valgrind-version/command.argv.json" 2>/dev/null; then
                printf 'KeysSnapshot Valgrind launcher version command evidence is missing or inconsistent\n' >&2
                validation_status=1
            fi
            launcher_hash_directory="$keys_snapshot_native/valgrind-identity/launcher-sha256"
            if ! grep -Fxq 'exit_code=0' "$launcher_hash_directory/process.status" 2>/dev/null \
                || ! grep -Fq 'sha256sum' "$launcher_hash_directory/command.argv.json" 2>/dev/null \
                || ! grep -Fq "$vg_identity_launcher_hash  $vg_identity_launcher_path" "$launcher_hash_directory/stdout.bin" 2>/dev/null; then
                printf 'KeysSnapshot Valgrind launcher SHA-256 evidence is missing or inconsistent\n' >&2
                validation_status=1
            fi
            if [[ "$vg_launcher_path" != "$vg_engine_path" ]]; then
                engine_hash_directory="$keys_snapshot_native/valgrind-identity/engine-sha256"
                if ! grep -Fxq 'exit_code=0' "$engine_hash_directory/process.status" 2>/dev/null \
                    || ! grep -Fq 'sha256sum' "$engine_hash_directory/command.argv.json" 2>/dev/null \
                    || ! grep -Fq "$vg_identity_engine_hash  $vg_identity_engine_path" "$engine_hash_directory/stdout.bin" 2>/dev/null; then
                    printf 'KeysSnapshot Valgrind engine SHA-256 evidence is missing or inconsistent\n' >&2
                    validation_status=1
                fi
                if ! grep -Fxq 'exit_code=0' "$keys_snapshot_native/valgrind-identity/engine-version/process.status" 2>/dev/null \
                    || [[ "$(tr -d '\r\n' < "$keys_snapshot_native/valgrind-identity/engine-version/stdout.bin" 2>/dev/null || true)" != "$vg_identity_version" ]]; then
                    printf 'KeysSnapshot Valgrind engine version evidence is missing or inconsistent\n' >&2
                    validation_status=1
                fi
            fi

            keys_snapshot_case="$keys_snapshot_native/case-00-keys-snapshot"
            keys_snapshot_source="$ROOT_DIR/tests/fixtures/mir_ordinary_map_any_owner_v2_keys_snapshot.mimi"
            target_source_sha256="$(sed -n 's/^target_source_sha256=//p' "$keys_snapshot_summary")"
            target_mir_digest="$(sed -n 's/^target_mir_digest=//p' "$keys_snapshot_summary")"
            source_hash_expected="$(sed -n 's/^captured_source_sha256=//p' "$keys_snapshot_case/source-hash.expected.txt" 2>/dev/null || true)"
            receipt_digest="$(sed -n 's/^mir_digest=//p' "$keys_snapshot_case/receipt/stdout.bin" 2>/dev/null || true)"
            source_hash_directory="$keys_snapshot_root/source-hashes/keys-snapshot-fixture-source-sha256"
            if [[ ! "$target_source_sha256" =~ ^[0-9a-fA-F]{64}$ ]] \
                || [[ "$target_source_sha256" != "$source_hash_expected" ]] \
                || [[ ! "$target_mir_digest" =~ ^[0-9a-fA-F]{64}$ ]] \
                || [[ "$target_mir_digest" != "$receipt_digest" ]] \
                || ! cmp -s "$keys_snapshot_source" "$keys_snapshot_case/source.fixture.mimi" \
                || ! grep -Fxq "source_path=$keys_snapshot_source" "$keys_snapshot_case/source-hash.expected.txt" \
                || ! grep -Fxq 'exit_code=0' "$source_hash_directory/process.status" 2>/dev/null \
                || ! grep -Fq 'sha256sum' "$source_hash_directory/command.argv.json" 2>/dev/null \
                || ! grep -Fq "$target_source_sha256  $keys_snapshot_case/source.fixture.mimi" "$source_hash_directory/stdout.bin" 2>/dev/null; then
                printf 'KeysSnapshot fixture source/hash evidence is missing, stale, or inconsistent\n' >&2
                validation_status=1
            fi
            for command_status in \
                "$keys_snapshot_case/receipt/process.status" \
                "$keys_snapshot_case/default-build/process.status" \
                "$keys_snapshot_case/explicit-build/process.status"; do
                if ! grep -Fxq 'exit_code=0' "$command_status" 2>/dev/null; then
                    printf 'KeysSnapshot MIR/build command did not exit successfully: %s\n' "$command_status" >&2
                    validation_status=1
                fi
            done
            for mode in default explicit; do
                memcheck_directory="$keys_snapshot_case/${mode}-memcheck"
                valgrind_command="$memcheck_directory/command.argv.json"
                if ! grep -Fq "\"$vg_launcher_path\"" "$valgrind_command" 2>/dev/null; then
                    printf 'KeysSnapshot %s did not execute through the recorded Valgrind path\n' "$mode" >&2
                    validation_status=1
                fi
                for required_argument in \
                    '"--leak-check=full"' \
                    '"--show-leak-kinds=all"' \
                    '"--errors-for-leak-kinds=definite,indirect,possible"' \
                    '"--error-exitcode=97"' \
                    '"--log-file='; do
                    if ! grep -Fq "$required_argument" "$valgrind_command" 2>/dev/null; then
                        printf 'KeysSnapshot strict Valgrind command omits %s: %s\n' "$required_argument" "$valgrind_command" >&2
                        validation_status=1
                    fi
                done
                for expected in \
                    'definitely lost: 0 bytes in 0 blocks' \
                    'indirectly lost: 0 bytes in 0 blocks' \
                    'possibly lost: 0 bytes in 0 blocks' \
                    'ERROR SUMMARY: 0 errors'; do
                    if ! grep -Fq "$expected" "$memcheck_directory/memcheck-summary.txt" 2>/dev/null; then
                        printf 'KeysSnapshot Memcheck summary lacks %s: %s\n' "$expected" "$memcheck_directory" >&2
                        validation_status=1
                    fi
                done
                if [ "$(grep -Ec '^still reachable: [0-9,]+ bytes in [0-9,]+ blocks' "$memcheck_directory/memcheck-summary.txt" 2>/dev/null || true)" -ne 1 ]; then
                    printf 'KeysSnapshot Memcheck summary lacks exactly one still-reachable row: %s\n' "$memcheck_directory" >&2
                    validation_status=1
                fi
                read -r observed_reachable_bytes observed_reachable_blocks <<< "$(still_reachable_counts "$memcheck_directory/memcheck-summary.txt" || true)"
                recorded_reachable_bytes="$(sed -n "s/^${mode}_still_reachable_bytes=//p" "$keys_snapshot_summary")"
                recorded_reachable_blocks="$(sed -n "s/^${mode}_still_reachable_blocks=//p" "$keys_snapshot_summary")"
                if [[ ! "$observed_reachable_bytes" =~ ^[0-9]+$ ]] \
                    || [[ ! "$observed_reachable_blocks" =~ ^[0-9]+$ ]] \
                    || [[ "$recorded_reachable_bytes" != "$observed_reachable_bytes" ]] \
                    || [[ "$recorded_reachable_blocks" != "$observed_reachable_blocks" ]]; then
                    printf 'KeysSnapshot %s run summary retained-byte counts differ from its Memcheck report\n' "$mode" >&2
                    validation_status=1
                fi
                if ! grep -Fxq 'expected_exit_code=0' "$memcheck_directory/expected-native-result.txt" 2>/dev/null \
                    || ! grep -Fxq 'exit_code=0' "$memcheck_directory/process.status" 2>/dev/null \
                    || ! printf 'answer\n' | cmp -s - "$memcheck_directory/expected-native-stdout.bin" \
                    || ! cmp -s "$memcheck_directory/expected-native-stdout.bin" "$memcheck_directory/stdout.bin"; then
                    printf 'KeysSnapshot %s native status/stdout differs from the exact expected result\n' "$mode" >&2
                    validation_status=1
                fi
            done
            default_stderr="$keys_snapshot_case/default-build/stderr.bin"
            explicit_stderr="$keys_snapshot_case/explicit-build/stderr.bin"
            if [ -z "$receipt_digest" ] \
                || [ "$(grep -Fc "canonical route disposition: canonical (map-any-owner-v2-keys-snapshot-v1) mir_digest=$receipt_digest" "$default_stderr" 2>/dev/null || true)" -ne 1 ] \
                || grep -Fq 'canonical route disposition: legacy' "$default_stderr" 2>/dev/null; then
                printf 'KeysSnapshot default build lacks its exact source-bound route receipt\n' >&2
                validation_status=1
            fi
            if [ "$(grep -Fc 'info: canonical MIR native compiled ' "$explicit_stderr" 2>/dev/null || true)" -ne 1 ] \
                || ! grep -Fq ' function(s), 0 fell back; mir_digest=' "$explicit_stderr" 2>/dev/null \
                || ! grep -Fq "mir_digest=$receipt_digest" "$explicit_stderr" 2>/dev/null \
                || grep -Fq 'canonical route disposition: legacy' "$explicit_stderr" 2>/dev/null; then
                printf 'KeysSnapshot explicit build lacks its complete no-fallback MIR census\n' >&2
                validation_status=1
            fi

            if [[ "$vg_launcher_path" == "$vg_engine_path" ]]; then
                expected_invocations=9
            else
                expected_invocations=11
            fi
            [[ "$(find "$keys_snapshot_root" -type f -name program | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$keys_snapshot_root" -type f -name valgrind.log | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$keys_snapshot_root" -type f -name memcheck-summary.txt | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$keys_snapshot_root" -type f -name command.argv.json | wc -l)" == "$expected_invocations" ]] || validation_status=1
            [[ "$(find "$keys_snapshot_root" -type f -name process.status | wc -l)" == "$expected_invocations" ]] || validation_status=1
            [[ "$(find "$keys_snapshot_root" -type f -name stdout.bin | wc -l)" == "$expected_invocations" ]] || validation_status=1
            [[ "$(find "$keys_snapshot_root" -type f -name stderr.bin | wc -l)" == "$expected_invocations" ]] || validation_status=1
        fi
    fi

    if [ "$heterogeneous_values_test_status" -eq 0 ]; then
        heterogeneous_values_root="$artifact_dir/values-heterogeneous"
        heterogeneous_values_native="$heterogeneous_values_root/native"
        heterogeneous_values_summary="$heterogeneous_values_native/run-summary.txt"
        heterogeneous_values_case="$heterogeneous_values_native/case-00-heterogeneous-values"
        if [ ! -s "$heterogeneous_values_summary" ]; then
            printf 'successful heterogeneous Values L3 test left no native run summary\n' >&2
            validation_status=1
        else
            grep -Fxq 'route_profile=map-any-owner-v2-values-heterogeneous-string-index1-v1' "$heterogeneous_values_summary" || validation_status=1
            grep -Fxq 'global_map_any_v2_production_ready=false' "$heterogeneous_values_summary" || validation_status=1
            grep -Fxq 'case_count=1' "$heterogeneous_values_summary" || validation_status=1
            grep -Fxq 'native_build_count=2' "$heterogeneous_values_summary" || validation_status=1
            grep -Fxq 'memcheck_run_count=2' "$heterogeneous_values_summary" || validation_status=1
            grep -Fxq 'default_route_receipt_count=1' "$heterogeneous_values_summary" || validation_status=1
            grep -Fxq 'explicit_mir_census_count=1' "$heterogeneous_values_summary" || validation_status=1
            grep -Fxq 'legacy_fallback_count=0' "$heterogeneous_values_summary" || validation_status=1

            vg_launcher_path="$(sed -n 's/^valgrind_launcher_path=//p' "$heterogeneous_values_summary")"
            vg_engine_path="$(sed -n 's/^valgrind_engine_path=//p' "$heterogeneous_values_summary")"
            vg_version="$(sed -n 's/^valgrind_version=//p' "$heterogeneous_values_summary")"
            vg_identity="$heterogeneous_values_native/valgrind-identity/valgrind-identity.txt"
            vg_identity_launcher_path="$(sed -n 's/^launcher_path=//p' "$vg_identity" 2>/dev/null || true)"
            vg_identity_engine_path="$(sed -n 's/^engine_path=//p' "$vg_identity" 2>/dev/null || true)"
            vg_identity_version="$(sed -n 's/^version=//p' "$vg_identity" 2>/dev/null || true)"
            if [[ ! "$vg_launcher_path" = /* ]] \
                || [[ ! "$vg_engine_path" = /* ]] \
                || [[ "$vg_launcher_path" != "$vg_identity_launcher_path" ]] \
                || [[ "$vg_engine_path" != "$vg_identity_engine_path" ]] \
                || [[ "$vg_version" != "$vg_identity_version" ]] \
                || [[ ! "$vg_version" = valgrind-* ]]; then
                printf 'heterogeneous Values strict Valgrind path/version identity is missing or inconsistent\n' >&2
                validation_status=1
            fi

            target_source_sha256="$(sed -n 's/^target_source_sha256=//p' "$heterogeneous_values_summary")"
            target_mir_digest="$(sed -n 's/^target_mir_digest=//p' "$heterogeneous_values_summary")"
            captured_source_sha256="$(sed -n 's/^captured_source_sha256=//p' "$heterogeneous_values_case/source-hash.expected.txt" 2>/dev/null || true)"
            receipt_digest="$(sed -n 's/^mir_digest=//p' "$heterogeneous_values_case/receipt/stdout.bin" 2>/dev/null || true)"
            source_hash_directory="$heterogeneous_values_root/source-hashes/heterogeneous-values-fixture-source-sha256"
            source_path="$ROOT_DIR/tests/fixtures/mir_ordinary_map_any_owner_v2_values_heterogeneous_string_index1.mimi"
            if [[ ! "$target_source_sha256" =~ ^[0-9a-fA-F]{64}$ ]] \
                || [[ "$captured_source_sha256" != "$target_source_sha256" ]] \
                || [[ ! "$target_mir_digest" =~ ^[0-9a-fA-F]{64}$ ]] \
                || [[ "$receipt_digest" != "$target_mir_digest" ]] \
                || ! cmp -s "$source_path" "$heterogeneous_values_case/source.fixture.mimi" \
                || ! grep -Fxq "source_path=$source_path" "$heterogeneous_values_case/source-hash.expected.txt" \
                || ! grep -Fxq 'exit_code=0' "$source_hash_directory/process.status" 2>/dev/null \
                || ! grep -Fq 'sha256sum' "$source_hash_directory/command.argv.json" 2>/dev/null \
                || ! grep -Fq "$target_source_sha256  $heterogeneous_values_case/source.fixture.mimi" "$source_hash_directory/stdout.bin" 2>/dev/null; then
                printf 'heterogeneous Values fixture source/hash evidence is missing, stale, or inconsistent\n' >&2
                validation_status=1
            fi
            for command_status in \
                "$heterogeneous_values_case/receipt/process.status" \
                "$heterogeneous_values_case/default-build/process.status" \
                "$heterogeneous_values_case/explicit-build/process.status"; do
                if ! grep -Fxq 'exit_code=0' "$command_status" 2>/dev/null; then
                    printf 'heterogeneous Values MIR/build command did not exit successfully: %s\n' "$command_status" >&2
                    validation_status=1
                fi
            done
            for mode in default explicit; do
                memcheck_directory="$heterogeneous_values_case/${mode}-memcheck"
                valgrind_command="$memcheck_directory/command.argv.json"
                if ! grep -Fq "\"$vg_launcher_path\"" "$valgrind_command" 2>/dev/null; then
                    printf 'heterogeneous Values %s did not execute through the recorded Valgrind path\n' "$mode" >&2
                    validation_status=1
                fi
                for required_argument in \
                    '"--leak-check=full"' \
                    '"--show-leak-kinds=all"' \
                    '"--errors-for-leak-kinds=definite,indirect,possible"' \
                    '"--error-exitcode=97"' \
                    '"--log-file='; do
                    if ! grep -Fq "$required_argument" "$valgrind_command" 2>/dev/null; then
                        printf 'heterogeneous Values strict Valgrind command omits %s: %s\n' "$required_argument" "$valgrind_command" >&2
                        validation_status=1
                    fi
                done
                for expected in \
                    'definitely lost: 0 bytes in 0 blocks' \
                    'indirectly lost: 0 bytes in 0 blocks' \
                    'possibly lost: 0 bytes in 0 blocks' \
                    'ERROR SUMMARY: 0 errors'; do
                    if ! grep -Fq "$expected" "$memcheck_directory/memcheck-summary.txt" 2>/dev/null; then
                        printf 'heterogeneous Values Memcheck summary lacks %s: %s\n' "$expected" "$memcheck_directory" >&2
                        validation_status=1
                    fi
                done
                if [ "$(grep -Ec '^still reachable: [0-9,]+ bytes in [0-9,]+ blocks' "$memcheck_directory/memcheck-summary.txt" 2>/dev/null || true)" -ne 1 ] \
                    || ! grep -Fxq 'expected_exit_code=0' "$memcheck_directory/expected-native-result.txt" 2>/dev/null \
                    || ! grep -Fxq 'exit_code=0' "$memcheck_directory/process.status" 2>/dev/null \
                    || ! printf 'true\n' | cmp -s - "$memcheck_directory/expected-native-stdout.bin" \
                    || ! cmp -s "$memcheck_directory/expected-native-stdout.bin" "$memcheck_directory/stdout.bin"; then
                    printf 'heterogeneous Values %s native status/stdout or retained-byte summary is incomplete\n' "$mode" >&2
                    validation_status=1
                fi
            done
            default_stderr="$heterogeneous_values_case/default-build/stderr.bin"
            explicit_stderr="$heterogeneous_values_case/explicit-build/stderr.bin"
            if [ -z "$receipt_digest" ] \
                || [ "$(grep -Fc "canonical route disposition: canonical (map-any-owner-v2-values-heterogeneous-string-index1-v1) mir_digest=$receipt_digest" "$default_stderr" 2>/dev/null || true)" -ne 1 ] \
                || grep -Fq 'canonical route disposition: legacy' "$default_stderr" 2>/dev/null; then
                printf 'heterogeneous Values default build lacks its exact source-bound route receipt\n' >&2
                validation_status=1
            fi
            if [ "$(grep -Fc 'info: canonical MIR native compiled ' "$explicit_stderr" 2>/dev/null || true)" -ne 1 ] \
                || ! grep -Fq ' function(s), 0 fell back; mir_digest=' "$explicit_stderr" 2>/dev/null \
                || ! grep -Fq "mir_digest=$receipt_digest" "$explicit_stderr" 2>/dev/null \
                || grep -Fq 'canonical route disposition: legacy' "$explicit_stderr" 2>/dev/null; then
                printf 'heterogeneous Values explicit build lacks its complete no-fallback MIR census\n' >&2
                validation_status=1
            fi

            if [[ "$vg_launcher_path" == "$vg_engine_path" ]]; then
                expected_invocations=9
            else
                expected_invocations=11
            fi
            [[ "$(find "$heterogeneous_values_root" -type f -name program | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$heterogeneous_values_root" -type f -name valgrind.log | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$heterogeneous_values_root" -type f -name memcheck-summary.txt | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$heterogeneous_values_root" -type f -name command.argv.json | wc -l)" == "$expected_invocations" ]] || validation_status=1
            [[ "$(find "$heterogeneous_values_root" -type f -name process.status | wc -l)" == "$expected_invocations" ]] || validation_status=1
            [[ "$(find "$heterogeneous_values_root" -type f -name stdout.bin | wc -l)" == "$expected_invocations" ]] || validation_status=1
            [[ "$(find "$heterogeneous_values_root" -type f -name stderr.bin | wc -l)" == "$expected_invocations" ]] || validation_status=1
        fi
    fi

    if [ "$unmatched_fixtures_test_status" -eq 0 ]; then
        unmatched_root="$artifact_dir/unmatched-fixtures"
        for case_name in dynamic-string-keys record-unit is-empty-control clone; do
            case_native="$unmatched_root/$case_name/native"
            case_summary="$case_native/run-summary.txt"
            for expected in \
                "fixture=mir_ordinary_map_" \
                'bytecode_run=true' \
                'reference_run=true' \
                'bytecode_reference_equal=true' \
                'default_native_build=true' \
                'explicit_native_build=true' \
                'memcheck_run_count=2' \
                'default_route_receipt=true' \
                'explicit_route_receipt=true' \
                'legacy_fallback_count=0'; do
                if ! grep -Fq "$expected" "$case_summary" 2>/dev/null; then
                    printf 'unmatched fixture summary lacks %s: %s\n' "$expected" "$case_summary" >&2
                    validation_status=1
                fi
            done
            if [[ "$case_name" == "dynamic-string-keys" ]]; then
                for expected in \
                    'route_profile=ordinary-map-owned-any-v2-computed-string-key-owner-chain-v1' \
                    'global_map_any_v2_production_ready=false' \
                    'zero_reachable_required=true' \
                    'expected_stdout=true\ntrue\nfalse\ntrue\n'; do
                    if ! grep -Fxq "$expected" "$case_summary" 2>/dev/null; then
                        printf 'dynamic String-key owner-chain summary lacks %s: %s\n' \
                            "$expected" "$case_summary" >&2
                        validation_status=1
                    fi
                done
            fi
            for mode in default explicit; do
                memcheck_summary="$case_native/${mode}-memcheck/memcheck-summary.txt"
                for expected in \
                    'definitely lost: 0 bytes in 0 blocks' \
                    'indirectly lost: 0 bytes in 0 blocks' \
                    'possibly lost: 0 bytes in 0 blocks' \
                    'ERROR SUMMARY: 0 errors'; do
                    if ! grep -Fq "$expected" "$memcheck_summary" 2>/dev/null; then
                        printf 'unmatched fixture Memcheck summary lacks %s: %s\n' "$expected" "$memcheck_summary" >&2
                        validation_status=1
                    fi
                done
            done
        done
        [[ "$(find "$unmatched_root" -type f -name run-summary.txt | wc -l)" == 4 ]] || validation_status=1
        [[ "$(find "$unmatched_root" -type f -name program | wc -l)" == 8 ]] || validation_status=1
        [[ "$(find "$unmatched_root" -type f -name valgrind.log | wc -l)" == 8 ]] || validation_status=1
        [[ "$(find "$unmatched_root" -type f -name memcheck-summary.txt | wc -l)" == 8 ]] || validation_status=1
        for evidence_file in command.argv.json process.status stdout.bin stderr.bin; do
            [[ "$(find "$unmatched_root" -type f -name "$evidence_file" | wc -l)" == 20 ]] || validation_status=1
        done
    fi

    acceptance_status="$test_status"
    if [ "$acceptance_status" -eq 0 ] && [ "$direct_tuple_get_l1_test_status" -ne 0 ]; then
        acceptance_status="$direct_tuple_get_l1_test_status"
    fi
    if [ "$acceptance_status" -eq 0 ] && [ "$direct_print_result_test_status" -ne 0 ]; then
        acceptance_status="$direct_print_result_test_status"
    fi
    if [ "$acceptance_status" -eq 0 ] && [ "$computed_key_string_test_status" -ne 0 ]; then
        acceptance_status="$computed_key_string_test_status"
    fi
    if [ "$acceptance_status" -eq 0 ] && [ "$keys_snapshot_test_status" -ne 0 ]; then
        acceptance_status="$keys_snapshot_test_status"
    fi
    if [ "$acceptance_status" -eq 0 ] && [ "$heterogeneous_values_test_status" -ne 0 ]; then
        acceptance_status="$heterogeneous_values_test_status"
    fi
    if [ "$acceptance_status" -eq 0 ] && [ "$unmatched_fixtures_test_status" -ne 0 ]; then
        acceptance_status="$unmatched_fixtures_test_status"
    fi
    if [ "$acceptance_status" -eq 0 ] && [ "$snapshot_status" -eq 0 ] && [ "$validation_status" -eq 0 ]; then
        printf 'PASS native binaries, route/test results, raw Memcheck logs, zero-loss summaries, and run counts\n' \
            > "$artifact_dir/artifact-validation.txt"
    elif [ "$snapshot_status" -ne 0 ]; then
        printf 'FAIL source snapshot changed during the strict Map v2 L3 run\n' \
            > "$artifact_dir/artifact-validation.txt"
    elif [ "$acceptance_status" -ne 0 ]; then
        printf 'FAIL acceptance test exit=%s; available evidence retained\n' "$acceptance_status" \
            > "$artifact_dir/artifact-validation.txt"
    else
        printf 'FAIL one or more required L3 artifacts or strict Memcheck summaries are missing\n' \
            > "$artifact_dir/artifact-validation.txt"
        acceptance_status=97
    fi

fi

acceptance_status="$test_status"
if [ "$acceptance_status" -eq 0 ] && [ "$direct_tuple_get_l1_test_status" -ne 0 ]; then
    acceptance_status="$direct_tuple_get_l1_test_status"
fi
if [ "$acceptance_status" -eq 0 ] && [ "$direct_print_result_test_status" -ne 0 ]; then
    acceptance_status="$direct_print_result_test_status"
fi
if [ "$acceptance_status" -eq 0 ] && [ "$computed_key_string_test_status" -ne 0 ]; then
    acceptance_status="$computed_key_string_test_status"
fi
if [ "$acceptance_status" -eq 0 ] && [ "$keys_snapshot_test_status" -ne 0 ]; then
    acceptance_status="$keys_snapshot_test_status"
fi
if [ "$acceptance_status" -eq 0 ] && [ "$heterogeneous_values_test_status" -ne 0 ]; then
    acceptance_status="$heterogeneous_values_test_status"
fi
if [ "$acceptance_status" -eq 0 ] && [ "$unmatched_fixtures_test_status" -ne 0 ]; then
    acceptance_status="$unmatched_fixtures_test_status"
fi
if [ "$validation_status" -ne 0 ] && [ "$acceptance_status" -eq 0 ]; then
    acceptance_status=97
fi
final_exit_status="$acceptance_status"
if [ "$snapshot_status" -ne 0 ]; then
    final_exit_status="$snapshot_status"
fi
if [ -n "$artifact_dir" ]; then
    if [ "$final_exit_status" -eq 0 ]; then
        final_result=PASS
    else
        final_result=FAIL
    fi
    {
        printf 'result=%s\n' "$final_result"
        printf 'completion=complete\n'
        printf 'exit_code=%s\n' "$final_exit_status"
        printf 'acceptance_test_exit=%s\n' "$acceptance_status"
        printf 'snapshot_exit=%s\n' "$snapshot_status"
        printf 'validation_exit=%s\n' "$validation_status"
        printf 'source_snapshot_before=%s\n' "$snapshot_before"
        printf 'source_snapshot_after=%s\n' "$snapshot_after"
        printf 'stage.map-owner.cargo_exit=%s\n' "$test_status"
        printf 'stage.map-owner.test_result_validation=%s\n' "$(sed -n 's/^validation=//p' "$artifact_dir/test-result-validation.txt" 2>/dev/null || printf missing)"
        printf 'stage.direct-tuple-get-l1.cargo_exit=%s\n' "$direct_tuple_get_l1_test_status"
        printf 'stage.direct-tuple-get-l1.test_result_validation=%s\n' "$(sed -n 's/^validation=//p' "$artifact_dir/direct-tuple-get-l1-test-result-validation.txt" 2>/dev/null || printf missing)"
        printf 'stage.direct-print-result.cargo_exit=%s\n' "$direct_print_result_test_status"
        printf 'stage.direct-print-result.test_result_validation=%s\n' "$(sed -n 's/^validation=//p' "$artifact_dir/direct-print-result-test-result-validation.txt" 2>/dev/null || printf missing)"
        printf 'stage.computed-key-string-get.cargo_exit=%s\n' "$computed_key_string_test_status"
        printf 'stage.computed-key-string-get.test_result_validation=%s\n' "$(sed -n 's/^validation=//p' "$artifact_dir/computed-key-string-get-test-result-validation.txt" 2>/dev/null || printf missing)"
        printf 'stage.keys-snapshot.cargo_exit=%s\n' "$keys_snapshot_test_status"
        printf 'stage.keys-snapshot.test_result_validation=%s\n' "$(sed -n 's/^validation=//p' "$artifact_dir/keys-snapshot-test-result-validation.txt" 2>/dev/null || printf missing)"
        printf 'stage.values-heterogeneous.cargo_exit=%s\n' "$heterogeneous_values_test_status"
        printf 'stage.values-heterogeneous.test_result_validation=%s\n' "$(sed -n 's/^validation=//p' "$artifact_dir/values-heterogeneous-test-result-validation.txt" 2>/dev/null || printf missing)"
        printf 'stage.unmatched-fixtures.cargo_exit=%s\n' "$unmatched_fixtures_test_status"
        printf 'stage.unmatched-fixtures.test_result_validation=%s\n' "$(sed -n 's/^validation=//p' "$artifact_dir/unmatched-fixtures-test-result-validation.txt" 2>/dev/null || printf missing)"
    } > "$artifact_dir/suite-result.txt"
    write_artifact_manifest
    FINAL_RESULT_WRITTEN=1
fi
printf 'acceptance_test_exit=%s\n' "$acceptance_status"
exit "$final_exit_status"
