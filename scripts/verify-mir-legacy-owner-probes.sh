#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_ENV=MIMI_LEGACY_OWNER_PROBES_ARTIFACT_DIR
TARGET_ENV=MIMI_LEGACY_OWNER_PROBES_TARGET_DIR
if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper
fi

for tool in bash cargo rustc sha256sum realpath git find sort wc od tr awk grep cmp \
    dirname date head readlink; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'legacy-owner probe capture requires %s\n' "$tool" >&2
        exit 1
    }
done
if [[ -v "$ARTIFACT_ENV" && -n "${!ARTIFACT_ENV}" ]]; then
    requested="${!ARTIFACT_ENV}"
else
    requested="${TMPDIR:-/tmp}/mimi-legacy-owner-probes-$(date -u +%Y%m%dT%H%M%SZ)-$$"
fi
case "$requested" in
    /*) ;;
    *) requested="$ROOT_DIR/$requested" ;;
esac
if [ -e "$requested" ] || [ -L "$requested" ]; then
    printf 'refusing to overwrite existing legacy-owner probe artifact path: %s\n' "$requested" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/target/"*)
        printf 'legacy-owner probe artifacts must be outside source/build trees: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"

if [[ -v "$TARGET_ENV" && -n "${!TARGET_ENV}" ]]; then
    requested_target="${!TARGET_ENV}"
    case "$requested_target" in
        /*) ;;
        *)
            printf '%s must be an absolute build directory: %s\n' "$TARGET_ENV" "$requested_target" >&2
            exit 2 ;;
    esac
    target_dir="$(realpath -m -- "$requested_target")"
    case "$target_dir/" in
        "$ROOT_DIR/"*|"$artifact_dir/"*)
            printf 'legacy-owner probe build directory must be outside the workspace and evidence tree: %s\n' "$target_dir" >&2
            exit 2 ;;
    esac
    mkdir -p -- "$target_dir"
    target_dir="$(realpath -e -- "$target_dir")"
else
    target_dir="$ROOT_DIR/target"
fi

scratch="$(mktemp -d "${TMPDIR:-/tmp}/mimi-legacy-owner-probes.XXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT
preflight_status=0
overall_status=0
: > "$artifact_dir/preflight-rejections.txt"

native_elf() {
    local path="$1"
    local magic
    [ -f "$path" ] && [ -x "$path" ] || return 1
    magic="$(od -An -tx1 -N4 -- "$path" | tr -d '[:space:]')" || return 1
    [[ "$magic" == 7f454c46 ]]
}

resolve_path_command() {
    local name="$1"
    local path_var="$2"
    local real_var="$3"
    local path
    path="$(type -P "$name" 2>/dev/null)" || return 1
    if [[ "$path" != /* ]]; then
        path="$(realpath -e -- "$path")" || return 1
    fi
    local invocation_dir invocation canonical
    invocation_dir="$(realpath -e -- "$(dirname -- "$path")")" || return 1
    invocation="$invocation_dir/${path##*/}"
    canonical="$(realpath -e -- "$invocation")" || return 1
    printf -v "$path_var" '%s' "$invocation"
    printf -v "$real_var" '%s' "$canonical"
}

record_preflight_failure() {
    printf 'rejected=%s\n' "$1" >> "$artifact_dir/preflight-rejections.txt"
    preflight_status=1
}

is_forbidden_inherited_env() {
    case "$1" in
        RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|RUSTDOCFLAGS|CARGO_ENCODED_RUSTDOCFLAGS|\
        RUSTC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|RUSTC_BOOTSTRAP|RUSTDOC|\
        RUSTUP_TOOLCHAIN|RUSTUP_HOME|\
        CARGO_BUILD_TARGET|CARGO_BUILD_RUSTC|CARGO_BUILD_RUSTC_WRAPPER|\
        CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER|CARGO_BUILD_RUSTDOC|CARGO_BUILD_RUSTFLAGS|\
        CARGO_TARGET_*|CARGO_PROFILE_*|CARGO_ALIAS_*|CARGO_INCREMENTAL|\
        CC|CXX|CPPFLAGS|CFLAGS|CXXFLAGS|LDFLAGS|AR|RANLIB|LD|LIBCLANG_PATH|\
        LD_LIBRARY_PATH|DYLD_LIBRARY_PATH|DYLD_FALLBACK_LIBRARY_PATH|\
        RUSTC_LINKER|LLVM_CONFIG|LLVM_CONFIG_PATH|PKG_CONFIG|PKG_CONFIG_PATH|\
        PKG_CONFIG_LIBDIR|PKG_CONFIG_SYSROOT_DIR|BINDGEN_EXTRA_CLANG_ARGS)
            return 0 ;;
        CARGO_BUILD_*)
            [[ "$1" != CARGO_BUILD_JOBS ]] && return 0 ;;
    esac
    return 1
}

collect_cargo_config() {
    local directory="$1"
    local candidate canonical existing
    for candidate in "$directory/.cargo/config" "$directory/.cargo/config.toml"; do
        [ -f "$candidate" ] || continue
        canonical="$(realpath -e -- "$candidate")" || return 1
        for existing in "${cargo_config_files[@]}"; do
            [[ "$existing" == "$canonical" ]] && continue 2
        done
        cargo_config_files+=("$canonical")
    done
}

config_has_semantic_override() {
    local path="$1"
    # Be conservative: any target table, target/runner/rustc/rustflags key,
    # rustc wrapper key, or test alias can change the command being measured.
    awk '
        {
            line = $0
            sub(/[[:space:]]*#.*/, "", line)
            if (line ~ /^[[:space:]]*\[[[:space:]]*target([.]|[[:space:]]*\])/) bad = 1
            if (line ~ /(^|[{,[:space:]])(target|runner|rustc|rustdoc|rustflags|rustc-wrapper|rustc-workspace-wrapper|test)[[:space:]]*=/) bad = 1
            if (line ~ /^[[:space:]]*build[.](target|rustc|rustdoc|rustflags|rustc-wrapper|rustc-workspace-wrapper)[[:space:]]*=/) bad = 1
            if (line ~ /^[[:space:]]*target[.]/) bad = 1
            if (line ~ /^[[:space:]]*alias[.]test[[:space:]]*=/) bad = 1
            if (line ~ /^[[:space:]]*test[[:space:]]*=/ && section ~ /alias/) bad = 1
            if (line ~ /^[[:space:]]*\[/) {
                section = line
                if (section ~ /^[[:space:]]*\[[[:space:]]*alias[[:space:]]*\]/) section = "alias"
            }
        }
        END { exit bad ? 0 : 1 }
    ' "$path"
}

config_manifest() {
    local destination="$1"
    local path
    : > "$destination"
    for path in "${cargo_config_files[@]}"; do
        sha256sum -- "$path" >> "$destination" || return 1
    done
    if [[ -n "${rustup_settings_file:-}" && -f "$rustup_settings_file" ]]; then
        sha256sum -- "$rustup_settings_file" >> "$destination" || return 1
    fi
}

identity_hash() {
    local path="$1"
    if [[ -f "$path" ]] && sha256sum -- "$path"; then
        return 0
    fi
    printf 'sha256=UNAVAILABLE path=%s\n' "$path"
    preflight_status=1
}

toolchain_snapshot() {
    local label="$1"
    local directory="$artifact_dir/toolchain-$label"
    local cargo_target rustc_target cargo_link rustc_link
    mkdir -p -- "$directory"
    cargo_target="$(realpath -e -- "$CARGO_BIN")" || cargo_target='<unresolved>'
    rustc_target="$(realpath -e -- "$RUSTC_INVOKE_PATH")" || rustc_target='<unresolved>'
    cargo_link='<not-a-symlink>'
    rustc_link='<not-a-symlink>'
    [[ -L "$CARGO_BIN" ]] && cargo_link="$(readlink -- "$CARGO_BIN")"
    [[ -L "$RUSTC_INVOKE_PATH" ]] && rustc_link="$(readlink -- "$RUSTC_INVOKE_PATH")"
    {
        printf 'cargo_invocation_path=%s\ncargo_invocation_name=%s\ncargo_link_target=%s\n' \
            "$CARGO_BIN" "${CARGO_BIN##*/}" "$cargo_link"
        printf 'cargo_canonical_elf_target=%s\n' "$cargo_target"
        printf 'cargo_invocation_target_native_elf=%s\n' \
            "$([[ "$cargo_target" != '<unresolved>' ]] && native_elf "$cargo_target" && printf PASS || printf FAIL)"
        identity_hash "$cargo_target"
        printf 'cargo_selected_binary=%s\n' "$CARGO_REAL_BIN"
        printf 'cargo_selected_binary_native_elf=%s\n' \
            "$(native_elf "$CARGO_REAL_BIN" && printf PASS || printf FAIL)"
        identity_hash "$CARGO_REAL_BIN"
        printf 'rustc_invocation_path=%s\nrustc_invocation_name=%s\nrustc_link_target=%s\n' \
            "$RUSTC_INVOKE_PATH" "${RUSTC_INVOKE_PATH##*/}" "$rustc_link"
        printf 'rustc_canonical_elf_target=%s\n' "$rustc_target"
        printf 'rustc_invocation_target_native_elf=%s\n' \
            "$([[ "$rustc_target" != '<unresolved>' ]] && native_elf "$rustc_target" && printf PASS || printf FAIL)"
        identity_hash "$rustc_target"
        printf 'rustc_selected_binary=%s\n' "$RUSTC_BIN"
        printf 'rustc_selected_binary_native_elf=%s\n' \
            "$(native_elf "$RUSTC_BIN" && printf PASS || printf FAIL)"
        identity_hash "$RUSTC_BIN"
    } > "$directory/identity.txt" 2> "$directory/identity.stderr"
    if [[ "$cargo_target" != '<unresolved>' ]] && native_elf "$cargo_target" && \
        "$CARGO_BIN" --version --verbose > "$directory/cargo-version.txt" 2>&1 && \
        "$CARGO_REAL_BIN" --version --verbose > "$directory/cargo-selected-version.txt" 2>&1 && \
        grep -Eq '^cargo [0-9]+\.' "$directory/cargo-version.txt" && \
        cmp -s "$directory/cargo-version.txt" "$directory/cargo-selected-version.txt"; then
        printf 'cargo_version_status=PASS\n' >> "$directory/identity.txt"
    else
        printf 'cargo_version_status=FAIL\n' >> "$directory/identity.txt"
        preflight_status=1
    fi
    if [[ "$rustc_target" != '<unresolved>' ]] && native_elf "$rustc_target" && \
        "$RUSTC_INVOKE_PATH" --version --verbose > "$directory/rustc-invocation-version.txt" 2>&1 && \
        "$RUSTC_BIN" --version --verbose > "$directory/rustc-version.txt" 2>&1 && \
        grep -Eq '^rustc [0-9]+\.' "$directory/rustc-version.txt" && \
        cmp -s "$directory/rustc-invocation-version.txt" "$directory/rustc-version.txt"; then
        printf 'rustc_version_status=PASS\n' >> "$directory/identity.txt"
    else
        printf 'rustc_version_status=FAIL\n' >> "$directory/identity.txt"
        preflight_status=1
    fi
}

source_manifest() {
    local destination="$1"
    local paths="$scratch/source-paths.nul"
    local sorted="$scratch/source-paths.sorted.nul"
    local errors="$artifact_dir/source-manifest.errors.txt"
    local -a inputs=()
    local item
    : > "$destination"
    : > "$errors"
    : > "$paths"
    for source_dir in src tests std; do
        if [ ! -d "$ROOT_DIR/$source_dir" ]; then
            printf 'missing_source_directory=%s\n' "$source_dir" >> "$errors"
            return 1
        fi
        if ! find "$ROOT_DIR/$source_dir" \( -type f -o -type l \) -print0 >> "$paths" 2>> "$errors"; then
            printf 'source_enumeration_failed=%s\n' "$source_dir" >> "$errors"
            return 1
        fi
    done
    for item in Cargo.toml Cargo.lock build.rs rust-toolchain rust-toolchain.toml \
        .cargo/config .cargo/config.toml scripts/audit-mir-legacy-owners.sh \
        scripts/verify-mir-legacy-owner-probes.sh; do
        if [ -f "$ROOT_DIR/$item" ]; then
            printf '%s\0' "$ROOT_DIR/$item" >> "$paths"
        elif [[ "$item" == Cargo.toml || "$item" == scripts/* ]]; then
            printf 'required_source_missing=%s\n' "$item" >> "$errors"
            return 1
        elif [ -e "$ROOT_DIR/$item" ]; then
            printf 'optional_source_not_regular_file=%s\n' "$item" >> "$errors"
            return 1
        fi
    done
    if ! sort -zu "$paths" -o "$sorted" 2>> "$errors"; then
        printf 'source_path_sort_failed=true\n' >> "$errors"
        return 1
    fi
    if ! mapfile -d '' -t inputs < "$sorted"; then
        printf 'source_manifest_read_failed=true\n' >> "$errors"
        return 1
    fi
    for item in "${inputs[@]}"; do
        if [ ! -f "$item" ]; then
            printf 'source_disappeared_before_hash=%s\n' "${item#"$ROOT_DIR"/}" >> "$errors"
            return 1
        fi
        if ! sha256sum -- "$item" >> "$destination" 2>> "$errors"; then
            printf 'source_hash_failed=%s\n' "${item#"$ROOT_DIR"/}" >> "$errors"
            return 1
        fi
    done
}

source_snapshot_check() {
    local label="$1"
    local snapshot="$scratch/source-$label.sha256"
    local status_file="$artifact_dir/source-$label.status"
    if ! source_manifest "$snapshot"; then
        cp "$artifact_dir/source-manifest.errors.txt" \
            "$artifact_dir/source-$label.errors.txt" 2>/dev/null || true
        printf 'source_snapshot=UNAVAILABLE\n' > "$status_file"
        return 1
    fi
    cp "$snapshot" "$artifact_dir/source-$label.sha256"
    if cmp -s "$scratch/source-before.sha256" "$snapshot"; then
        printf 'source_snapshot=UNCHANGED\n' > "$status_file"
        return 0
    fi
    printf 'source_snapshot=CHANGED\n' > "$status_file"
    return 1
}

cd "$ROOT_DIR"
if [[ -d "$LLVM_SYS_181_PREFIX" ]]; then
    LLVM_SYS_181_PREFIX="$(realpath -e -- "$LLVM_SYS_181_PREFIX")"
else
    record_preflight_failure "LLVM_wrapper_directory_unavailable:${LLVM_SYS_181_PREFIX}"
fi
export LLVM_SYS_181_PREFIX
if source_manifest "$scratch/source-before.sha256"; then
    cp "$scratch/source-before.sha256" "$artifact_dir/source-before.sha256"
    printf 'source_snapshot=AVAILABLE\n' > "$artifact_dir/source-before.status"
else
    cp "$artifact_dir/source-manifest.errors.txt" \
        "$artifact_dir/source-before.errors.txt" 2>/dev/null || true
    : > "$scratch/source-before.sha256"
    : > "$artifact_dir/source-before.sha256"
    printf 'source_snapshot=UNAVAILABLE\n' > "$artifact_dir/source-before.status"
    record_preflight_failure 'source_before_manifest_failed'
fi

resolve_path_command cargo CARGO_LOOKUP_PATH CARGO_LOOKUP_REAL || \
    record_preflight_failure 'cargo_not_resolvable_from_PATH'
resolve_path_command rustc RUSTC_LOOKUP_PATH RUSTC_LOOKUP_REAL || \
    record_preflight_failure 'rustc_not_resolvable_from_PATH'

for lookup_name in CARGO_LOOKUP_REAL RUSTC_LOOKUP_REAL; do
    lookup_real="${!lookup_name:-}"
    if [[ -n "$lookup_real" ]] && ! native_elf "$lookup_real"; then
        binary_role="${lookup_name%_LOOKUP_REAL}"
        binary_role="${binary_role,,}"
        record_preflight_failure "${binary_role}_is_not_a_native_ELF_executable:${lookup_real}"
    fi
done

CARGO_BIN="${CARGO_LOOKUP_PATH:-}"
RUSTC_INVOKE_PATH="${RUSTC_LOOKUP_PATH:-}"
CARGO_REAL_BIN=''
RUSTC_BIN=''
cargo_is_rustup=false
rustc_is_rustup=false
if [[ -n "${CARGO_LOOKUP_REAL:-}" && "${CARGO_LOOKUP_REAL##*/}" == rustup ]]; then
    cargo_is_rustup=true
fi
if [[ -n "${RUSTC_LOOKUP_REAL:-}" && "${RUSTC_LOOKUP_REAL##*/}" == rustup ]]; then
    rustc_is_rustup=true
fi
if [[ "$cargo_is_rustup" == true || "$rustc_is_rustup" == true ]]; then
    if [[ "$cargo_is_rustup" != "$rustc_is_rustup" ]]; then
        record_preflight_failure 'cargo_and_rustc_do_not_use_the_same_rustup_proxy_mode'
    fi
    RUSTUP_HOME_EFFECTIVE="${RUSTUP_HOME:-$HOME/.rustup}"
    rustup_settings_file="$RUSTUP_HOME_EFFECTIVE/settings.toml"
    if [[ ! -f "$rustup_settings_file" ]]; then
        record_preflight_failure "rustup_settings_missing:${rustup_settings_file}"
    else
        mapfile -t default_toolchains < <(awk -F '\"' \
            '/^[[:space:]]*default_toolchain[[:space:]]*=/ { print $2 }' \
            "$rustup_settings_file")
        if [[ "${#default_toolchains[@]}" -ne 1 || \
            ! "${default_toolchains[0]:-}" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]]; then
            record_preflight_failure 'rustup_default_toolchain_is_missing_or_ambiguous'
        else
            default_toolchain="${default_toolchains[0]}"
            if awk '
                /^[[:space:]]*\[overrides\][[:space:]]*$/ { inside = 1; next }
                /^[[:space:]]*\[/ { inside = 0 }
                inside {
                    line = $0
                    sub(/[[:space:]]*#.*/, "", line)
                    if (line ~ /[^[:space:]]/) found = 1
                }
                END { exit found ? 0 : 1 }
            ' "$rustup_settings_file"; then
                record_preflight_failure 'rustup_per_directory_toolchain_overrides_present'
            fi
            marker_dir="$ROOT_DIR"
            while :; do
                for marker in "$marker_dir/rust-toolchain" "$marker_dir/rust-toolchain.toml"; do
                    if [[ -f "$marker" ]]; then
                        record_preflight_failure "rustup_project_toolchain_override:${marker}"
                    fi
                done
                [[ "$marker_dir" == / ]] && break
                marker_dir="$(dirname -- "$marker_dir")"
            done
            toolchain_bin="$RUSTUP_HOME_EFFECTIVE/toolchains/$default_toolchain/bin"
            if [[ -x "$toolchain_bin/cargo" && -x "$toolchain_bin/rustc" ]]; then
                CARGO_REAL_BIN="$(realpath -e -- "$toolchain_bin/cargo")"
                RUSTC_BIN="$(realpath -e -- "$toolchain_bin/rustc")"
            else
                record_preflight_failure "rustup_default_toolchain_binaries_missing:${toolchain_bin}"
            fi
        fi
    fi
else
    CARGO_REAL_BIN="${CARGO_LOOKUP_REAL:-}"
    RUSTC_BIN="${RUSTC_LOOKUP_REAL:-}"
fi

if [[ -n "$CARGO_BIN" ]] && ! native_elf "$CARGO_BIN"; then
    record_preflight_failure "cargo_is_not_a_native_ELF_executable:${CARGO_BIN}"
fi
if [[ -n "$RUSTC_INVOKE_PATH" ]] && ! native_elf "$RUSTC_INVOKE_PATH"; then
    record_preflight_failure "rustc_invocation_is_not_a_native_ELF_executable:${RUSTC_INVOKE_PATH}"
fi
if [[ -n "$CARGO_REAL_BIN" ]] && ! native_elf "$CARGO_REAL_BIN"; then
    record_preflight_failure "cargo_selected_binary_is_not_a_native_ELF_executable:${CARGO_REAL_BIN}"
fi
if [[ -n "$RUSTC_BIN" ]] && ! native_elf "$RUSTC_BIN"; then
    record_preflight_failure "rustc_is_not_a_native_ELF_executable:${RUSTC_BIN}"
fi
if [[ -n "$CARGO_BIN" && ( "$CARGO_BIN" != /* || "${CARGO_BIN##*/}" != cargo ) ]]; then
    record_preflight_failure 'cargo_invocation_path_is_not_absolute_canonical_cargo_name'
fi
if [[ -n "$RUSTC_INVOKE_PATH" && ( "$RUSTC_INVOKE_PATH" != /* || "${RUSTC_INVOKE_PATH##*/}" != rustc ) ]]; then
    record_preflight_failure 'rustc_invocation_path_is_not_absolute_canonical_rustc_name'
fi
if [[ -n "$CARGO_BIN" && "$(dirname -- "$CARGO_BIN")" != \
    "$(realpath -e -- "$(dirname -- "$CARGO_BIN")" 2>/dev/null || true)" ]]; then
    record_preflight_failure 'cargo_invocation_directory_is_not_canonical'
fi
if [[ -n "$RUSTC_INVOKE_PATH" && "$(dirname -- "$RUSTC_INVOKE_PATH")" != \
    "$(realpath -e -- "$(dirname -- "$RUSTC_INVOKE_PATH")" 2>/dev/null || true)" ]]; then
    record_preflight_failure 'rustc_invocation_directory_is_not_canonical'
fi
if [[ -n "$CARGO_BIN" && -n "${CARGO_LOOKUP_REAL:-}" && \
    "$(realpath -e -- "$CARGO_BIN" 2>/dev/null || true)" != "$CARGO_LOOKUP_REAL" ]]; then
    record_preflight_failure 'cargo_invocation_link_target_changed_during_preflight'
fi
if [[ -n "$RUSTC_INVOKE_PATH" && -n "${RUSTC_LOOKUP_REAL:-}" && \
    "$(realpath -e -- "$RUSTC_INVOKE_PATH" 2>/dev/null || true)" != "$RUSTC_LOOKUP_REAL" ]]; then
    record_preflight_failure 'rustc_invocation_link_target_changed_during_preflight'
fi

cargo_config_files=()
config_dir="$ROOT_DIR"
while :; do
    collect_cargo_config "$config_dir" || record_preflight_failure "cargo_config_scan_failed:${config_dir}"
    [[ "$config_dir" == / ]] && break
    config_dir="$(dirname -- "$config_dir")"
done
if [[ -v CARGO_HOME ]]; then
    if [[ -z "$CARGO_HOME" || "$CARGO_HOME" != /* ]]; then
        record_preflight_failure 'CARGO_HOME_must_be_nonempty_absolute_when_set'
        cargo_home_effective="$ROOT_DIR/${CARGO_HOME:-}"
    else
        cargo_home_effective="$(realpath -m -- "$CARGO_HOME")"
    fi
else
    cargo_home_effective="$HOME/.cargo"
fi
for config_file in "$cargo_home_effective/config" "$cargo_home_effective/config.toml"; do
    if [[ -f "$config_file" ]]; then
        canonical_config="$(realpath -e -- "$config_file")"
        already_seen=false
        for existing_config in "${cargo_config_files[@]}"; do
            [[ "$existing_config" == "$canonical_config" ]] && already_seen=true
        done
        if [[ "$already_seen" == false ]]; then
            cargo_config_files+=("$canonical_config")
        fi
    fi
done
for config_file in "${cargo_config_files[@]}"; do
    if config_has_semantic_override "$config_file"; then
        record_preflight_failure "cargo_config_contains_target_runner_compiler_wrapper_or_test_alias_override:${config_file}"
    fi
done

mapfile -t inherited_variables < <(compgen -e | sort -u)
for variable in "${inherited_variables[@]}"; do
    if is_forbidden_inherited_env "$variable"; then
        record_preflight_failure "inherited_environment_override:${variable}"
    fi
done

config_manifest "$artifact_dir/cargo-config-before.sha256" || \
    record_preflight_failure 'cargo_config_before_hash_failed'
if [[ -n "$CARGO_BIN" && -n "$CARGO_REAL_BIN" && \
    -n "$RUSTC_INVOKE_PATH" && -n "$RUSTC_BIN" ]] && \
    native_elf "$CARGO_BIN" && native_elf "$CARGO_REAL_BIN" && \
    native_elf "$RUSTC_INVOKE_PATH" && native_elf "$RUSTC_BIN"; then
    toolchain_snapshot before
else
    printf 'toolchain_snapshot=SKIPPED\nreason=native_cargo_or_rustc_unavailable\n' \
        > "$artifact_dir/toolchain-before.status"
    preflight_status=1
fi

{
    printf 'branch=%s\nhead=%s\n' "$(git branch --show-current)" "$(git rev-parse HEAD)"
    printf 'cwd=%s\nLLVM_SYS_181_PREFIX=%s\n' "$ROOT_DIR" "$LLVM_SYS_181_PREFIX"
    printf 'cargo_target_dir=%s\n' "$target_dir"
    printf 'bash=%s\n' "$(bash --version | head -n 1)"
    printf 'cargo_invocation_path=%s\ncargo_canonical_elf_target=%s\ncargo_selected_binary=%s\n' \
        "${CARGO_BIN:-<unresolved>}" "${CARGO_LOOKUP_REAL:-<unresolved>}" \
        "${CARGO_REAL_BIN:-<unresolved>}"
    printf 'rustc_invocation_path=%s\nrustc_canonical_elf_target=%s\nrustc_selected_binary=%s\n' \
        "${RUSTC_INVOKE_PATH:-<unresolved>}" "${RUSTC_LOOKUP_REAL:-<unresolved>}" \
        "${RUSTC_BIN:-<unresolved>}"
    printf 'cargo_home_effective=%s\n' "$cargo_home_effective"
    printf 'cargo_config_count=%s\n' "${#cargo_config_files[@]}"
    for config_file in "${cargo_config_files[@]}"; do
        printf 'cargo_config=%s\n' "$config_file"
    done
    for variable in PATH CARGO_HOME CARGO_TARGET_DIR CARGO_BUILD_JOBS RUSTUP_TOOLCHAIN \
        RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTC RUSTC_WRAPPER \
        RUSTC_WORKSPACE_WRAPPER RUSTC_BOOTSTRAP RUSTDOC RUSTC_LINKER CC CXX \
        CFLAGS CXXFLAGS CPPFLAGS \
        LDFLAGS AR RANLIB LD LIBCLANG_PATH LD_LIBRARY_PATH DYLD_LIBRARY_PATH \
        DYLD_FALLBACK_LIBRARY_PATH LLVM_CONFIG LLVM_CONFIG_PATH PKG_CONFIG \
        PKG_CONFIG_PATH PKG_CONFIG_LIBDIR PKG_CONFIG_SYSROOT_DIR \
        BINDGEN_EXTRA_CLANG_ARGS CARGO_INCREMENTAL TMPDIR \
        MIMI_LEGACY_OWNER_PROBES_TARGET_DIR; do
        if [[ -v "$variable" ]]; then
            if is_forbidden_inherited_env "$variable"; then
                printf '%s=<set-and-rejected>\n' "$variable"
            else
                printf '%s=%q\n' "$variable" "${!variable}"
            fi
        else
            printf '%s=<unset>\n' "$variable"
        fi
    done
    printf 'controlled_RUSTC=%s\ncontrolled_CARGO_BUILD_JOBS=1\n' \
        "${RUSTC_BIN:-<unavailable>}"
} > "$artifact_dir/environment.txt"
printf 'preflight=%s\n' "$([[ "$preflight_status" -eq 0 ]] && printf PASS || printf FAIL)" \
    > "$artifact_dir/preflight.status"

tests=(
    codegen::tests::compile_checked_routes_generic_ignored_bool_binding_projection_without_legacy_access
    verifier::tests::public_checked_verifier_routes_recoverable_cross_state_flow_to_mir
    verifier::tests::typed_ffi_symbolic_string_contexts_defer_without_legacy_access
    verifier::tests::canonical_mir_bool_result_postconditions_keep_flow_verdicts_through_dual
    tests::audit_fix_scripts::legacy_owner_audit_rejects_map_any_route_regressions
)

strict_test_oracle() {
    local test_name="$1"
    local output="$2"
    local running_count exact_count summary_count testcase_count
    running_count="$(grep -Ec '^running 1 test$' "$output" || true)"
    exact_count="$(grep -Fxc -- "test $test_name ... ok" "$output" || true)"
    summary_count="$(grep -Ec '^test result:' "$output" || true)"
    testcase_count="$(grep -Ec '^test .* \.\.\. (ok|FAILED|ignored)$' "$output" || true)"
    [[ "$running_count" -eq 1 && "$exact_count" -eq 1 && \
        "$summary_count" -eq 1 && "$testcase_count" -eq 1 ]] && \
        grep -Eq '^test result: ok\. 1 passed; 0 failed; [0-9]+ ignored; [0-9]+ measured; [0-9]+ filtered out; finished in [0-9]+([.][0-9]+)?s$' \
            "$output"
}

source_stable=1
for index in "${!tests[@]}"; do
    test_name="${tests[$index]}"
    label="probe-$((index + 1))"
    directory="$artifact_dir/$label"
    mkdir -- "$directory"
    cargo_args=(
        test --manifest-path "$ROOT_DIR/Cargo.toml"
        --target-dir "$target_dir"
        --features llvm18-host-dynamic --lib "$test_name" --
        --exact --test-threads=1
    )
    {
        printf '%q ' "$CARGO_BIN" "${cargo_args[@]}"
        printf '\n'
    } > "$directory/command.txt"
    printf '%s\0' "$CARGO_BIN" "${cargo_args[@]}" > "$directory/command.argv.nul"
    {
        printf 'cwd=%s\n' "$ROOT_DIR"
        printf 'LLVM_SYS_181_PREFIX=%s\nCARGO_HOME=%s\nCARGO_BUILD_JOBS=1\n' \
            "$LLVM_SYS_181_PREFIX" "$cargo_home_effective"
        printf 'cargo_target_dir=%s\n' "$target_dir"
        printf 'RUSTC=%s\n' "$RUSTC_BIN"
        printf 'test_name=%s\n' "$test_name"
    } > "$directory/command.environment.txt"
    if ! source_snapshot_check "${label}-before"; then
        source_stable=0
        overall_status=1
    fi
    if [[ "$preflight_status" -ne 0 || "$source_stable" -ne 1 ]]; then
        status='NOT_RUN'
        if [[ "$preflight_status" -ne 0 ]]; then
            printf 'not_run=preflight_failed\n' > "$directory/stdout.txt"
        else
            printf 'not_run=source_snapshot_changed\n' > "$directory/stdout.txt"
        fi
        : > "$directory/stderr.txt"
    else
        if env CARGO_HOME="$cargo_home_effective" LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
            CARGO_BUILD_JOBS=1 RUSTC="$RUSTC_BIN" \
            "$CARGO_BIN" "${cargo_args[@]}" \
            > "$directory/stdout.txt" 2> "$directory/stderr.txt"; then
            status=0
        else
            status=$?
            overall_status=1
        fi
    fi
    if ! source_snapshot_check "${label}-after"; then
        source_stable=0
        overall_status=1
    fi
    if [[ "$status" == 0 ]]; then
        printf 'exit_code=0\nsuccess=true\n' > "$directory/process.status"
        if [[ "$source_stable" -ne 1 ]]; then
            printf 'validation=FAIL\nreason=source_snapshot_changed\ntest=%s\n' \
                "$test_name" > "$directory/validation.txt"
            overall_status=1
        elif ! strict_test_oracle "$test_name" "$directory/stdout.txt"; then
            printf 'validation=FAIL\nreason=exact_test_oracle_mismatch\ntest=%s\n' \
                "$test_name" > "$directory/validation.txt"
            overall_status=1
        else
            printf 'validation=PASS\ntest=%s\n' "$test_name" > "$directory/validation.txt"
        fi
    elif [[ "$status" == NOT_RUN ]]; then
        printf 'exit_code=NOT_RUN\nsuccess=false\n' > "$directory/process.status"
        if [[ "$preflight_status" -ne 0 ]]; then
            printf 'validation=FAIL\nreason=preflight_failed\ntest=%s\n' \
                "$test_name" > "$directory/validation.txt"
        else
            printf 'validation=FAIL\nreason=source_snapshot_changed\ntest=%s\n' \
                "$test_name" > "$directory/validation.txt"
        fi
        overall_status=1
    else
        printf 'exit_code=%s\nsuccess=false\n' "$status" > "$directory/process.status"
        printf 'validation=FAIL\ntest=%s\nexit_code=%s\n' "$test_name" "$status" \
            > "$directory/validation.txt"
    fi
done

if bash scripts/audit-mir-legacy-owners.sh > "$artifact_dir/static-audit.log" \
    2> "$artifact_dir/static-audit.stderr"; then
    audit_status=0
else
    audit_status=$?
    overall_status=1
fi
printf 'exit_code=%s\nsuccess=%s\n' "$audit_status" \
    "$([[ "$audit_status" -eq 0 ]] && printf true || printf false)" \
    > "$artifact_dir/static-audit.status"
if ! grep -Fxq 'audit_status=ok' "$artifact_dir/static-audit.log"; then
    overall_status=1
fi

if [[ -n "$CARGO_BIN" && -n "$CARGO_REAL_BIN" && \
    -n "$RUSTC_INVOKE_PATH" && -n "$RUSTC_BIN" ]] && \
    native_elf "$CARGO_BIN" && native_elf "$CARGO_REAL_BIN" && \
    native_elf "$RUSTC_INVOKE_PATH" && native_elf "$RUSTC_BIN"; then
    toolchain_snapshot after
else
    printf 'toolchain_snapshot=SKIPPED\nreason=native_cargo_or_rustc_unavailable\n' \
        > "$artifact_dir/toolchain-after.status"
    preflight_status=1
fi
if [[ -d "$artifact_dir/toolchain-before" && -d "$artifact_dir/toolchain-after" ]] && \
    cmp -s "$artifact_dir/toolchain-before/identity.txt" \
        "$artifact_dir/toolchain-after/identity.txt" && \
    cmp -s "$artifact_dir/toolchain-before/cargo-version.txt" \
        "$artifact_dir/toolchain-after/cargo-version.txt" && \
    cmp -s "$artifact_dir/toolchain-before/rustc-version.txt" \
        "$artifact_dir/toolchain-after/rustc-version.txt"; then
    printf 'toolchain_identity=UNCHANGED\n' > "$artifact_dir/toolchain-stability.status"
else
    printf 'toolchain_identity=CHANGED_OR_UNAVAILABLE\n' \
        > "$artifact_dir/toolchain-stability.status"
    overall_status=1
fi
if config_manifest "$artifact_dir/cargo-config-after.sha256"; then
    if cmp -s "$artifact_dir/cargo-config-before.sha256" \
        "$artifact_dir/cargo-config-after.sha256"; then
        printf 'cargo_config=UNCHANGED\n' > "$artifact_dir/cargo-config-stability.status"
    else
        printf 'cargo_config=CHANGED\n' > "$artifact_dir/cargo-config-stability.status"
        overall_status=1
    fi
else
    printf 'cargo_config=AFTER_HASH_FAILED\n' > "$artifact_dir/cargo-config-stability.status"
    overall_status=1
fi
if [[ "$preflight_status" -ne 0 ]]; then
    overall_status=1
fi

if source_manifest "$scratch/source-after.sha256"; then
    cp "$scratch/source-after.sha256" "$artifact_dir/source-after.sha256"
    if ! cmp -s "$scratch/source-before.sha256" "$scratch/source-after.sha256"; then
        printf 'source_snapshot=CHANGED\n' > "$artifact_dir/source-snapshot.status"
        overall_status=1
    else
        printf 'source_snapshot=UNCHANGED\n' > "$artifact_dir/source-snapshot.status"
    fi
else
    cp "$artifact_dir/source-manifest.errors.txt" \
        "$artifact_dir/source-after.errors.txt" 2>/dev/null || true
    : > "$artifact_dir/source-after.sha256"
    printf 'source_snapshot=UNAVAILABLE\n' > "$artifact_dir/source-snapshot.status"
    overall_status=1
fi

if [ "$overall_status" -eq 0 ]; then
    result='PASS'
else
    result='FAIL'
fi
{
    printf 'validation=%s\nprobe_count=%s\n' "$result" "${#tests[@]}"
    for index in "${!tests[@]}"; do
        printf '%s=%s\n' "probe_$((index + 1))" "${tests[$index]}"
    done
    printf 'static_audit_exit=%s\n' "$audit_status"
    printf 'preflight=%s\n' "$([[ "$preflight_status" -eq 0 ]] && printf PASS || printf FAIL)"
    cat "$artifact_dir/toolchain-stability.status" 2>/dev/null || true
    cat "$artifact_dir/cargo-config-stability.status" 2>/dev/null || true
    cat "$artifact_dir/source-snapshot.status"
} > "$scratch/artifact-validation.txt"

hash_status=0
if (
    cd "$artifact_dir" || exit 1
    find . -type f ! -name files.sha256 ! -name files.sha256.sha256 \
        ! -name artifact-validation.txt ! -name files.sha256.errors.txt \
        -print0 2> files.sha256.errors.txt \
        | sort -z \
        | xargs -0 -r sha256sum > files.sha256
    sha256sum --check files.sha256 > /dev/null 2>> files.sha256.errors.txt
    sha256sum files.sha256 > files.sha256.sha256 2>> files.sha256.errors.txt
    sha256sum --check files.sha256.sha256 > /dev/null 2>> files.sha256.errors.txt
); then
    hash_status=0
else
    hash_status=$?
    overall_status=1
fi
if [ "$hash_status" -ne 0 ]; then
    result='FAIL'
fi
cp "$scratch/artifact-validation.txt" "$artifact_dir/artifact-validation.txt"
if [ "$hash_status" -eq 0 ]; then
    printf 'artifact_hash_manifest=PASS\n' >> "$artifact_dir/artifact-validation.txt"
else
    printf 'artifact_hash_manifest=FAIL\n' >> "$artifact_dir/artifact-validation.txt"
fi
printf 'validation=%s\nartifact_dir=%s\n' "$result" "$artifact_dir"
exit "$overall_status"
