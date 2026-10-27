#!/usr/bin/env bash
# Serial, source-bound L1/L2 gate for the Map-root producer/caller profile.
set -uo pipefail

if [[ $# -gt 1 || ${1:-} == --help ]]; then
    printf 'Usage: %s [current-YYYYMMDD-rN]\n' "$0"
    exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
run_id="${1:-current-$(date -u +%Y%m%d-%H%M%S)-$$}"
[[ "$run_id" =~ ^[a-z0-9][a-z0-9-]{0,80}$ ]] || { printf 'Invalid run id\n' >&2; exit 2; }
artifact_dir="$repo_root/evidence/0.1.11-dev/gates/map-root-producer-caller-l1l2-$run_id"
[[ ! -e "$artifact_dir" && ! -L "$artifact_dir" ]] || { printf 'Refusing to overwrite %s\n' "$artifact_dir" >&2; exit 2; }
mkdir -p -- "$artifact_dir"
cd -- "$repo_root"

source_snapshot() {
    {
        for directory in src tests scripts std docs .cargo; do
            [[ ! -d "$directory" ]] || rg --files --hidden "$directory"
        done
        for path in .gitignore Cargo.toml Cargo.lock build.rs rust-toolchain.toml rust-toolchain .rustfmt.toml rustfmt.toml; do
            [[ ! -f "$path" ]] || printf '%s\n' "$path"
        done
    } | LC_ALL=C sort -u
}

hash_snapshot() {
    while IFS= read -r file; do sha256sum -- "$file"; done
}

source_snapshot > "$artifact_dir/source-files.before.txt" 2> "$artifact_dir/source-snapshot.before.stderr"
while IFS= read -r file; do sha256sum -- "$file"; done \
    < "$artifact_dir/source-files.before.txt" > "$artifact_dir/source.before.sha256"
{
    printf 'repo_root=%s\nrun_id=%s\nbranch=' "$repo_root" "$run_id"
    git branch --show-current
    printf 'head='; git rev-parse HEAD
    printf 'cargo_build_jobs=1\ntest_threads=1\nllvm_sys_181_prefix=%s\n' "${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
    printf 'cargo='; cargo --version
    printf 'rustc='; rustc --version --verbose
    printf 'llvm_config='; "${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}/bin/llvm-config" --version
} > "$artifact_dir/environment.txt" 2>&1

overall_status=0
checker_status=125
cli_status=125
run_gate() {
    local gate_name="$1"
    shift
    printf '%q ' "$@" > "$artifact_dir/$gate_name.command.txt"
    printf '\n' >> "$artifact_dir/$gate_name.command.txt"
    "$@" > "$artifact_dir/$gate_name.stdout" 2> "$artifact_dir/$gate_name.stderr"
    local gate_status=$?
    printf 'exit_code=%s\n' "$gate_status" > "$artifact_dir/$gate_name.status"
    if [[ "$gate_status" != 0 && "$overall_status" == 0 ]]; then
        overall_status="$gate_status"
    fi
    return 0
}

run_gate checker-mir-backend env CARGO_BUILD_JOBS=1 LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}" \
    cargo test --manifest-path "$repo_root/Cargo.toml" --lib map_root_producer_caller -- --nocapture --test-threads=1
checker_status="$(sed -n 's/^exit_code=//p' "$artifact_dir/checker-mir-backend.status")"
run_gate cli-routes env CARGO_BUILD_JOBS=1 LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}" \
    cargo test --manifest-path "$repo_root/Cargo.toml" --test canonical_mir_ordinary_map_owned_any_v2_map_root_producer_caller_cli -- --nocapture --test-threads=1
cli_status="$(sed -n 's/^exit_code=//p' "$artifact_dir/cli-routes.status")"

source_snapshot > "$artifact_dir/source-files.after.txt" 2> "$artifact_dir/source-snapshot.after.stderr"
while IFS= read -r file; do sha256sum -- "$file"; done \
    < "$artifact_dir/source-files.after.txt" > "$artifact_dir/source.after.sha256"
source_unchanged=false
if cmp -s "$artifact_dir/source-files.before.txt" "$artifact_dir/source-files.after.txt" \
    && cmp -s "$artifact_dir/source.before.sha256" "$artifact_dir/source.after.sha256"; then
    source_unchanged=true
else
    diff -u "$artifact_dir/source-files.before.txt" "$artifact_dir/source-files.after.txt" \
        > "$artifact_dir/source-files.diff" || true
    diff -u "$artifact_dir/source.before.sha256" "$artifact_dir/source.after.sha256" \
        > "$artifact_dir/source-hashes.diff" || true
    overall_status=98
fi

printf 'profile=ordinary-map-owned-any-v2-map-root-producer-caller-v1\nexit_code=%s\nchecker_exit_code=%s\ncli_exit_code=%s\nsource_tree_unchanged=%s\nglobal_map_any_v2_production_ready=false\n' \
    "$overall_status" "$checker_status" "$cli_status" "$source_unchanged" > "$artifact_dir/gate.status"
(cd -- "$artifact_dir" && find . -type f ! -name SHA256SUMS -print0 \
    | LC_ALL=C sort -z | xargs -0 -r sha256sum) > "$artifact_dir/SHA256SUMS"
if [[ "$overall_status" == 0 ]]; then
    printf 'Map-root producer/caller L1/L2 passed; evidence: %s\n' "$artifact_dir"
else
    printf 'Map-root producer/caller L1/L2 failed (exit %s); evidence: %s\n' "$overall_status" "$artifact_dir" >&2
fi
exit "$overall_status"
