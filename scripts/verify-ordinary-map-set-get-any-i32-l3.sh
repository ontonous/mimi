#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
llvm_prefix="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
target_dir="${CARGO_TARGET_DIR:-}"

if [[ -z "$target_dir" ]]; then
    printf 'CARGO_TARGET_DIR must name an absolute external build directory\n' >&2
    exit 2
fi
case "$target_dir" in
    /*) ;;
    *) printf 'CARGO_TARGET_DIR must be absolute: %s\n' "$target_dir" >&2; exit 2 ;;
esac
target_dir="$(realpath -m -- "$target_dir")"
case "$target_dir/" in
    "$repo_root/"*)
        printf 'CARGO_TARGET_DIR must resolve outside the repository: %s\n' "$target_dir" >&2
        exit 2
        ;;
esac
export CARGO_TARGET_DIR="$target_dir"

artifact_dir="${MIMI_ORDINARY_MAP_SET_GET_ANY_I32_L3_EVIDENCE:?set this to a fresh external evidence directory}"
case "$artifact_dir" in
    /*) ;;
    *) printf 'L3 evidence path must be absolute: %s\n' "$artifact_dir" >&2; exit 2 ;;
esac
artifact_dir="$(realpath -m -- "$artifact_dir")"
case "$artifact_dir/" in
    "$repo_root/"*)
        printf 'L3 evidence must be outside the repository: %s\n' "$artifact_dir" >&2
        exit 2
        ;;
esac
if [[ -e "$artifact_dir" || -L "$artifact_dir" ]]; then
    printf 'refusing to overwrite L3 evidence: %s\n' "$artifact_dir" >&2
    exit 2
fi
if [[ ! -d "$llvm_prefix" ]]; then
    printf 'LLVM wrapper directory is unavailable: %s\n' "$llvm_prefix" >&2
    exit 1
fi
if ! command -v valgrind >/dev/null 2>&1; then
    printf 'Set/GetAny-i32 L3 requires Valgrind\n' >&2
    exit 1
fi

mkdir -p -- "$(dirname -- "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
cd -- "$repo_root"

test_arguments=(
    test
    --manifest-path "$repo_root/Cargo.toml"
    --test ordinary-map-set-get-any-i32-l3
    --features ordinary-map-set-result-l3
)
{
    printf 'LLVM_SYS_181_PREFIX=%q CARGO_TARGET_DIR=%q MIMI_ORDINARY_MAP_SET_GET_ANY_I32_L3_EVIDENCE=%q ' \
        "$llvm_prefix" "$target_dir" "$artifact_dir"
    printf '%q ' cargo "${test_arguments[@]}"
    printf '\n'
} > "$artifact_dir/command.txt"
printf '%s\0' cargo "${test_arguments[@]}" > "$artifact_dir/command.argv.nul"
{
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$llvm_prefix"
    printf 'CARGO_TARGET_DIR=%s\n' "$target_dir"
    printf 'cargo_path=%s\n' "$(command -v cargo)"
    printf 'cargo_version='; cargo --version
    printf 'rustc_path=%s\n' "$(command -v rustc)"
    printf 'rustc_version='; rustc --version
    printf 'valgrind_path=%s\n' "$(command -v valgrind)"
    printf 'valgrind_version='; valgrind --version
} > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"

set +e
LLVM_SYS_181_PREFIX="$llvm_prefix" \
MIMI_ORDINARY_MAP_SET_GET_ANY_I32_L3_EVIDENCE="$artifact_dir" \
cargo "${test_arguments[@]}" > "$artifact_dir/cargo.stdout.log" 2> "$artifact_dir/cargo.stderr.log"
status=$?
set -e
printf '%s\n' "$status" > "$artifact_dir/cargo.exit-status.txt"
exit "$status"
