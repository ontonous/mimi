#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
llvm_prefix="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
default_artifact_dir="$repo_root/evidence/0.1.11-dev/gates/set-get-i32-native-l3-$timestamp-$$"
artifact_dir="${MIMI_ORDINARY_MAP_SET_GET_I32_L3_EVIDENCE:-$default_artifact_dir}"

case "$artifact_dir" in
    /*) ;;
    *) artifact_dir="$repo_root/$artifact_dir" ;;
esac
artifact_dir="$(realpath -m -- "$artifact_dir")"
case "$artifact_dir/" in
    "$repo_root/src/"*|"$repo_root/tests/"*|"$repo_root/std/"*|"$repo_root/scripts/"*|"$repo_root/target/"*)
        printf 'Set/Get-i32 L3 evidence must stay outside source and build trees: %s\n' "$artifact_dir" >&2
        exit 2
        ;;
esac
if [[ -e "$artifact_dir" || -L "$artifact_dir" ]]; then
    printf 'refusing to overwrite Set/Get-i32 L3 evidence: %s\n' "$artifact_dir" >&2
    exit 2
fi
if [[ ! -d "$llvm_prefix" ]]; then
    printf 'LLVM wrapper directory is unavailable: %s\n' "$llvm_prefix" >&2
    exit 1
fi
if ! command -v valgrind >/dev/null 2>&1; then
    printf 'Set/Get-i32 L3 requires Valgrind\n' >&2
    exit 1
fi

mkdir -p -- "$(dirname -- "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
cd -- "$repo_root"

test_arguments=(
    test
    --manifest-path "$repo_root/Cargo.toml"
    --test ordinary-map-set-get-i32-l3
    --features ordinary-map-set-result-l3
)
{
    printf 'LLVM_SYS_181_PREFIX=%q MIMI_ORDINARY_MAP_SET_GET_I32_L3_EVIDENCE=%q ' \
        "$llvm_prefix" "$artifact_dir"
    printf '%q ' cargo "${test_arguments[@]}"
    printf '\n'
} > "$artifact_dir/command.txt"
printf '%s\0' cargo "${test_arguments[@]}" > "$artifact_dir/command.argv.nul"
{
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$llvm_prefix"
    printf 'cargo_path=%s\n' "$(command -v cargo)"
    printf 'cargo_version='; cargo --version
    printf 'rustc_path=%s\n' "$(command -v rustc)"
    printf 'rustc_version='; rustc --version
    printf 'valgrind_path=%s\n' "$(command -v valgrind)"
    printf 'valgrind_version='; valgrind --version
    printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR:-<default>}"
} > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"

set +e
LLVM_SYS_181_PREFIX="$llvm_prefix" \
MIMI_ORDINARY_MAP_SET_GET_I32_L3_EVIDENCE="$artifact_dir" \
cargo "${test_arguments[@]}" > "$artifact_dir/cargo.stdout.log" 2> "$artifact_dir/cargo.stderr.log"
status=$?
set -e
printf '%s\n' "$status" > "$artifact_dir/cargo.exit-status.txt"
exit "$status"
