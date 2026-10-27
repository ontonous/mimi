#!/usr/bin/env bash
# Serial, source-bound strict native Memcheck gate for one Map-root receipt.
set -Eeuo pipefail

if [[ $# -gt 1 || ${1:-} == --help ]]; then
    printf 'Usage: %s [current-YYYYMMDD-rN]\n' "$0"
    exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
run_id="${1:-current-$(date -u +%Y%m%d-%H%M%S)-$$}"
[[ "$run_id" =~ ^[a-z0-9][a-z0-9-]{0,80}$ ]] || { printf 'Invalid run id\n' >&2; exit 2; }
artifact_dir="$repo_root/evidence/0.1.11-dev/gates/map-root-producer-caller-l3-$run_id"
[[ ! -e "$artifact_dir" && ! -L "$artifact_dir" ]] || { printf 'Refusing to overwrite %s\n' "$artifact_dir" >&2; exit 2; }
mkdir -p -- "$artifact_dir"
cd -- "$repo_root"

target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
[[ "$target_dir" == /* ]] || target_dir="$repo_root/$target_dir"
export CARGO_TARGET_DIR="$(realpath -m -- "$target_dir")"
export LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
test_artifact_dir="$artifact_dir/test-artifacts"
mkdir -- "$test_artifact_dir"
export MIMI_MAP_ROOT_PRODUCER_CALLER_L3_ARTIFACT_DIR="$test_artifact_dir"
strict_memcheck_dir="$test_artifact_dir/strict-memcheck"

source_snapshot() {
    {
        for directory in src tests scripts .cargo; do
            [[ ! -d "$directory" ]] || rg --files --hidden "$directory"
        done
        for path in .gitignore Cargo.toml Cargo.lock build.rs rust-toolchain.toml rust-toolchain .rustfmt.toml rustfmt.toml; do
            [[ ! -f "$path" ]] || printf '%s\n' "$path"
        done
    } | LC_ALL=C sort -u
}

status=0
finalize() {
    local shell_status=$?
    trap - EXIT
    [[ "$shell_status" == 0 ]] || status="$shell_status"
    local source_unchanged=false
    if [[ -f "$artifact_dir/source-files.before.txt" ]]; then
        source_snapshot > "$artifact_dir/source-files.after.txt" 2> "$artifact_dir/source-snapshot.after.stderr" || status=98
        if cmp -s "$artifact_dir/source-files.before.txt" "$artifact_dir/source-files.after.txt"; then
            while IFS= read -r file; do sha256sum -- "$file"; done \
                < "$artifact_dir/source-files.after.txt" > "$artifact_dir/source.after.sha256"
            if cmp -s "$artifact_dir/source.before.sha256" "$artifact_dir/source.after.sha256"; then
                source_unchanged=true
            else
                status=98
            fi
        else
            status=98
        fi
    else
        status=98
    fi
    printf 'profile=ordinary-map-owned-any-v2-map-root-producer-caller-v1\nexit_code=%s\nsource_tree_unchanged=%s\nglobal_map_any_v2_production_ready=false\n' \
        "$status" "$source_unchanged" > "$artifact_dir/gate.status"
    (cd -- "$artifact_dir" && find . -type f ! -name SHA256SUMS -print0 \
        | LC_ALL=C sort -z | xargs -0 -r sha256sum) > "$artifact_dir/SHA256SUMS"
    if [[ "$status" == 0 ]]; then
        printf 'Map-root producer/caller L3 passed; evidence: %s\n' "$artifact_dir"
    else
        printf 'Map-root producer/caller L3 failed (exit %s); evidence: %s\n' "$status" "$artifact_dir" >&2
    fi
    exit "$status"
}
trap finalize EXIT

source_snapshot > "$artifact_dir/source-files.before.txt"
while IFS= read -r file; do sha256sum -- "$file"; done \
    < "$artifact_dir/source-files.before.txt" > "$artifact_dir/source.before.sha256"
{
    printf 'repo_root=%s\nrun_id=%s\ncargo_target_dir=%s\nllvm_sys_181_prefix=%s\n' \
        "$repo_root" "$run_id" "$CARGO_TARGET_DIR" "$LLVM_SYS_181_PREFIX"
    printf 'cargo='; cargo --version
    printf 'rustc='; rustc --version --verbose
    printf 'valgrind='; valgrind --version
    printf 'llvm_config='; "$LLVM_SYS_181_PREFIX/bin/llvm-config" --version
} > "$artifact_dir/environment.txt" 2>&1

{
    for binary in native-default native-explicit-mir; do
        binary_path="$strict_memcheck_dir/$binary"
        if ! git check-ignore -q -- "$binary_path"; then
            printf 'expected native test binary is not ignored: %s\n' "$binary_path" >&2
            exit 97
        fi
        git check-ignore -v -- "$binary_path"
    done
    visible_record="$artifact_dir/gate.status"
    if git check-ignore -q -- "$visible_record"; then
        printf 'gate evidence record must remain visible: %s\n' "$visible_record" >&2
        exit 97
    fi
    printf 'gate_evidence_visible=true\n'
} > "$artifact_dir/ignore-preflight.txt" 2> "$artifact_dir/ignore-preflight.stderr"

test_name=map_root_producer_caller_default_and_explicit_native_bins_pass_strict_memcheck
test_target=canonical_mir_ordinary_map_owned_any_v2_map_root_producer_caller_cli
printf '%q ' cargo test --manifest-path "$repo_root/Cargo.toml" --test "$test_target" "$test_name" -- --exact --ignored --test-threads=1 --nocapture \
    > "$artifact_dir/cargo-command.txt"
printf '\n' >> "$artifact_dir/cargo-command.txt"
if cargo test --manifest-path "$repo_root/Cargo.toml" --test "$test_target" "$test_name" \
    -- --exact --ignored --test-threads=1 --nocapture \
    > "$artifact_dir/cargo.stdout" 2> "$artifact_dir/cargo.stderr"; then
    status=0
else
    status=$?
fi
printf 'exit_code=%s\n' "$status" > "$artifact_dir/cargo.status"
[[ "$status" == 0 ]] || exit "$status"

summary_dir="$test_artifact_dir/strict-memcheck"
[[ -f "$summary_dir/l3-summary.txt" ]] || { printf 'L3 test omitted final summary\n' >&2; status=97; exit "$status"; }
rg -q '^default_explicit_identity_equal=true$' "$summary_dir/l3-summary.txt"
rg -q '^memcheck_runs=2$' "$summary_dir/l3-summary.txt"
for mode in default explicit-mir; do
    for kind in 'definitely lost' 'indirectly lost' 'possibly lost'; do
        rg -q "^${kind}: 0 bytes in 0 blocks( \\(inferred from all-free summary\\))?$" \
            "$summary_dir/memcheck-summary-${mode}.txt"
    done
    rg -q '^==[0-9]+== All heap blocks were freed -- no leaks are possible$' \
        "$summary_dir/memcheck-${mode}.log"
    rg -q '^==[0-9]+== ERROR SUMMARY: 0 errors from 0 contexts \(suppressed: 0 from 0\)$' \
        "$summary_dir/memcheck-${mode}.log"
done
status=0
