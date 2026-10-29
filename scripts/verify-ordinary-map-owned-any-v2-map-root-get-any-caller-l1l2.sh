#!/usr/bin/env bash
# Serial, source-bound L1/L2 gate for the Map-root GetAny caller profile.
set -uo pipefail

if [[ $# -gt 1 || ${1:-} == --help ]]; then
    printf 'Usage: %s [current-YYYYMMDD-rN]\n' "$0"
    exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
run_id="${1:-current-$(date -u +%Y%m%d-%H%M%S)-$$}"
[[ "$run_id" =~ ^[a-z0-9][a-z0-9-]{0,80}$ ]] || { printf 'Invalid run id\n' >&2; exit 2; }
artifact_dir="$repo_root/evidence/0.1.11-dev/gates/map-root-get-any-caller-l1l2-$run_id"
[[ ! -e "$artifact_dir" && ! -L "$artifact_dir" ]] || { printf 'Refusing to overwrite %s\n' "$artifact_dir" >&2; exit 2; }
mkdir -p -- "$artifact_dir"
cd -- "$repo_root"

source_snapshot() {
    {
        for directory in src tests scripts std docs devdocs .cargo; do
            [[ ! -d "$directory" ]] || rg --files --hidden -0 "$directory"
        done
        for path in .gitignore Cargo.toml Cargo.lock build.rs rust-toolchain.toml rust-toolchain .rustfmt.toml rustfmt.toml; do
            [[ ! -f "$path" ]] || printf '%s\0' "$path"
        done
    } | LC_ALL=C sort -zu
}

hash_snapshot() {
    local source_list="$1"
    xargs -0 -r sha256sum -- < "$source_list"
}

overall_status=0
unit_status=125
cli_status=125
environment_status=0
source_before_status=0
source_after_status=0
source_unchanged=false
program_ignored=false
evidence_visible=false
ignore_status=0

if source_snapshot > "$artifact_dir/source-files.before.nul" 2> "$artifact_dir/source-snapshot.before.stderr"; then
    tr '\0' '\n' < "$artifact_dir/source-files.before.nul" > "$artifact_dir/source-files.before.txt"
    if hash_snapshot "$artifact_dir/source-files.before.nul" > "$artifact_dir/source.before.sha256"; then
        source_before_status=0
    else
        source_before_status=$?
    fi
else
    source_before_status=$?
fi
if [[ "$source_before_status" != 0 ]]; then
    overall_status=98
fi

if {
    printf 'repo_root=%s\nrun_id=%s\nbranch=' "$repo_root" "$run_id"
    git branch --show-current
    printf 'head='; git rev-parse HEAD
    printf 'cargo_build_jobs=1\ntest_threads=1\nCARGO_PROFILE_TEST_DEBUG=0\nCARGO_PROFILE_DEV_DEBUG=0\nCARGO_INCREMENTAL=1\nllvm_sys_181_prefix=%s\n' "${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
    printf 'cargo='; cargo --version
    printf 'rustc='; rustc --version --verbose
    printf 'llvm_config='; "${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}/bin/llvm-config" --version
} > "$artifact_dir/environment.txt" 2>&1; then
    environment_status=0
else
    environment_status=$?
    [[ "$overall_status" != 0 ]] || overall_status=99
fi

finalize() {
    local shell_status=$?
    trap - EXIT
    if [[ "$shell_status" != 0 && "$overall_status" == 0 ]]; then
        overall_status="$shell_status"
    fi

    if [[ "$source_before_status" == 0 ]]; then
        if source_snapshot > "$artifact_dir/source-files.after.nul" 2> "$artifact_dir/source-snapshot.after.stderr"; then
            tr '\0' '\n' < "$artifact_dir/source-files.after.nul" > "$artifact_dir/source-files.after.txt"
            if hash_snapshot "$artifact_dir/source-files.after.nul" > "$artifact_dir/source.after.sha256"; then
                source_after_status=0
                if cmp -s "$artifact_dir/source-files.before.nul" "$artifact_dir/source-files.after.nul" \
                    && cmp -s "$artifact_dir/source.before.sha256" "$artifact_dir/source.after.sha256"; then
                    source_unchanged=true
                else
                    diff -u "$artifact_dir/source-files.before.txt" "$artifact_dir/source-files.after.txt" \
                        > "$artifact_dir/source-files.diff" || true
                    diff -u "$artifact_dir/source.before.sha256" "$artifact_dir/source.after.sha256" \
                        > "$artifact_dir/source-hashes.diff" || true
                    [[ "$overall_status" != 0 ]] || overall_status=98
                fi
            else
                source_after_status=$?
                [[ "$overall_status" != 0 ]] || overall_status=98
            fi
        else
            source_after_status=$?
            [[ "$overall_status" != 0 ]] || overall_status=98
        fi
    fi

    if [[ "$ignore_status" != 0 && "$overall_status" == 0 ]]; then
        overall_status=97
    fi
    printf 'profile=ordinary-map-owned-any-v2-map-root-get-any-caller-v1\nexit_code=%s\nunit_exit_code=%s\ncli_exit_code=%s\nenvironment_capture_exit_code=%s\nsource_before_exit_code=%s\nsource_after_exit_code=%s\nsource_tree_unchanged=%s\ngenerated_program_path_ignored=%s\ngate_evidence_visible=%s\nglobal_map_any_v2_production_ready=false\n' \
        "$overall_status" "$unit_status" "$cli_status" "$environment_status" \
        "$source_before_status" "$source_after_status" "$source_unchanged" \
        "$program_ignored" "$evidence_visible" > "$artifact_dir/gate.status"
    if (cd -- "$artifact_dir" && find . -type f ! -name SHA256SUMS -print0 \
        | LC_ALL=C sort -z | xargs -0 -r sha256sum) > "$artifact_dir/SHA256SUMS"; then
        :
    else
        [[ "$overall_status" != 0 ]] || overall_status=99
        printf 'exit_code=%s\n' "$overall_status" > "$artifact_dir/gate.status"
    fi
    if [[ "$overall_status" == 0 ]]; then
        printf 'Map-root GetAny caller L1/L2 passed; evidence: %s\n' "$artifact_dir"
    else
        printf 'Map-root GetAny caller L1/L2 failed (exit %s); evidence: %s\n' "$overall_status" "$artifact_dir" >&2
    fi
    exit "$overall_status"
}
trap finalize EXIT

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

run_gate checker-mir-backends env CARGO_BUILD_JOBS=1 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=1 LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}" \
    cargo test --manifest-path "$repo_root/Cargo.toml" --lib map_root_get_any_caller -- --nocapture --test-threads=1
unit_status="$(sed -n 's/^exit_code=//p' "$artifact_dir/checker-mir-backends.status")"
run_gate route-cli-backends-verifier env CARGO_BUILD_JOBS=1 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=1 LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}" \
    cargo test --manifest-path "$repo_root/Cargo.toml" --test canonical_mir_ordinary_map_owned_any_v2_map_root_get_any_caller_cli -- --nocapture --test-threads=1
cli_status="$(sed -n 's/^exit_code=//p' "$artifact_dir/route-cli-backends-verifier.status")"

program_path="${artifact_dir#"$repo_root"/}/native/default/program"
if git check-ignore -v -- "$program_path" \
    > "$artifact_dir/generated-program-ignore.txt" 2>&1; then
    program_ignored=true
else
    check_ignore_status=$?
    printf 'generated program path ignore probe failed with exit %s\n' "$check_ignore_status" \
        >> "$artifact_dir/generated-program-ignore.txt"
    ignore_status="$check_ignore_status"
fi
evidence_path="${artifact_dir#"$repo_root"/}/gate.status"
if git check-ignore -- "$evidence_path" \
    > "$artifact_dir/evidence-visibility-check.txt" 2>&1; then
    evidence_visible=false
    printf 'gate evidence is unexpectedly ignored\n' >> "$artifact_dir/evidence-visibility-check.txt"
    ignore_status=1
else
    check_ignore_status=$?
    if [[ "$check_ignore_status" == 1 ]]; then
        evidence_visible=true
        printf 'gate evidence remains visible to version control\n' > "$artifact_dir/evidence-visibility-check.txt"
    else
        printf 'git check-ignore failed for gate evidence with exit %s\n' "$check_ignore_status" \
            > "$artifact_dir/evidence-visibility-check.txt"
        ignore_status="$check_ignore_status"
    fi
fi
