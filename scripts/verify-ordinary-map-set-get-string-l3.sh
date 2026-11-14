#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
evidence_base="/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/gates"
target_dir="${CARGO_TARGET_DIR:-/media/ontos-hpc/P7000Z2T/.codex-target-cache/mimi-ordinary-map-set-get-string-l3}"
llvm_prefix="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
artifact_dir="${MIMI_ORDINARY_MAP_SET_GET_STRING_L3_EVIDENCE:-$evidence_base/ordinary-map-set-get-string-native-l3-$timestamp-$$}"

case "$artifact_dir" in
    /*) ;;
    *) printf 'String L3 evidence path must be absolute: %s\n' "$artifact_dir" >&2; exit 2 ;;
esac
case "$target_dir" in
    /*) ;;
    *) printf 'String L3 Cargo target path must be absolute: %s\n' "$target_dir" >&2; exit 2 ;;
esac
artifact_dir="$(realpath -m -- "$artifact_dir")"
target_dir="$(realpath -m -- "$target_dir")"
case "$artifact_dir/" in
    "$repo_root/"*) printf 'String L3 evidence must stay outside the repository: %s\n' "$artifact_dir" >&2; exit 2 ;;
esac
case "$target_dir/" in
    "$repo_root/"*) printf 'String L3 Cargo target must stay outside the repository: %s\n' "$target_dir" >&2; exit 2 ;;
esac
if [[ -e "$artifact_dir" || -L "$artifact_dir" ]]; then
    printf 'refusing to overwrite String L3 evidence: %s\n' "$artifact_dir" >&2
    exit 2
fi
if [[ ! -d "$llvm_prefix" ]]; then
    printf 'LLVM wrapper directory is unavailable: %s\n' "$llvm_prefix" >&2
    exit 1
fi
if [[ ! -d /media/ontos-hpc/P7000Z2T ]]; then
    printf 'external evidence volume is unavailable: /media/ontos-hpc/P7000Z2T\n' >&2
    exit 1
fi
if ! command -v valgrind >/dev/null 2>&1; then
    printf 'ordinary Map Set/Get-String L3 requires Valgrind\n' >&2
    exit 1
fi

mkdir -p -- "$(dirname -- "$artifact_dir")" "$target_dir"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
target_dir="$(realpath -- "$target_dir")"
cd -- "$repo_root"

test_arguments=(
    test
    --manifest-path "$repo_root/Cargo.toml"
    --test ordinary-map-set-get-string-l3
    --features ordinary-map-set-result-l3
)
{
    printf 'LLVM_SYS_181_PREFIX=%q CARGO_TARGET_DIR=%q MIMI_ORDINARY_MAP_SET_GET_STRING_L3_EVIDENCE=%q ' \
        "$llvm_prefix" "$target_dir" "$artifact_dir"
    printf '%q ' cargo "${test_arguments[@]}"
    printf '\n'
} > "$artifact_dir/command.txt"
printf '%s\0' cargo "${test_arguments[@]}" > "$artifact_dir/command.argv.nul"
{
    printf 'repository=%s\n' "$repo_root"
    printf 'evidence_root=%s\n' "$artifact_dir"
    printf 'target_dir=%s\n' "$target_dir"
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$llvm_prefix"
    printf 'cargo_path=%s\n' "$(command -v cargo)"
    printf 'cargo_version='; cargo --version
    printf 'rustc_path=%s\n' "$(command -v rustc)"
    printf 'rustc_version='; rustc --version
    printf 'cc_path=%s\n' "$(command -v cc)"
    printf 'cc_version='; cc -dumpfullversion -dumpversion
    printf 'valgrind_path=%s\n' "$(command -v valgrind)"
    printf 'valgrind_version='; valgrind --version
    printf 'sha256sum_path=%s\n' "$(command -v sha256sum)"
} > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"

set +e
LLVM_SYS_181_PREFIX="$llvm_prefix" \
CARGO_TARGET_DIR="$target_dir" \
MIMI_ORDINARY_MAP_SET_GET_STRING_L3_EVIDENCE="$artifact_dir" \
cargo "${test_arguments[@]}" > "$artifact_dir/cargo.stdout.log" 2> "$artifact_dir/cargo.stderr.log"
status=$?
set -e
printf '%s\n' "$status" > "$artifact_dir/cargo.exit-status.txt"
find "$artifact_dir" -type f ! -name evidence-files.sha256 -print0 |
    sort -z |
    xargs -0 sha256sum > "$artifact_dir/evidence-files.sha256"
exit "$status"
