#!/usr/bin/env bash
# One serial, source-sealed gate for the independently admitted empty-root profile.
set -Eeuo pipefail
set -o noclobber

if [[ $# -gt 1 || ${1:-} == --help ]]; then
    printf 'Usage: %s [current-YYYYMMDD-rN]\n' "$0"
    exit 2
fi
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
configured_cargo_target_dir="${CARGO_TARGET_DIR:-<default>}"
resolved_target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
[[ "$resolved_target_dir" == /* ]] || resolved_target_dir="$repo_root/$resolved_target_dir"
resolved_target_dir="$(realpath -m -- "$resolved_target_dir")"
# Pin Cargo to the exact target path inspected above; otherwise build.target-dir
# in a user/project Cargo config could redirect linking to another filesystem.
export CARGO_TARGET_DIR="$resolved_target_dir"
run_id="${1:-current-$(date -u +%Y%m%d-%H%M%S)-$$}"
[[ "$run_id" =~ ^[a-z0-9][a-z0-9-]{0,80}$ ]] || { printf 'Invalid run id\n' >&2; exit 2; }
artifact_dir="$repo_root/evidence/0.1.11-dev/gates/map-empty-root-root-queries-v1-$run_id"
[[ ! -e "$artifact_dir" && ! -L "$artifact_dir" ]] || { printf 'Refusing to overwrite %s\n' "$artifact_dir" >&2; exit 2; }
mkdir -- "$artifact_dir"

failed=0
finished=false
source_snapshot_status=not-established
snapshot() {
    {
        for directory in src tests std scripts .cargo; do
            [[ ! -d "$directory" ]] || rg --files --hidden "$directory"
        done
        for path in Cargo.toml Cargo.lock build.rs rust-toolchain.toml rust-toolchain .rustfmt.toml rustfmt.toml; do
            [[ ! -f "$path" ]] || printf '%s\n' "$path"
        done
    } | LC_ALL=C sort -u
}
space_check() {
    local requested_path="${1:?filesystem path must be specified}"
    local required_kib="${2:?required KiB must be specified}"
    local filesystem_path="$requested_path"
    [[ "$filesystem_path" == /* ]] || filesystem_path="$repo_root/$filesystem_path"
    filesystem_path="$(realpath -m -- "$filesystem_path")"
    while [[ ! -e "$filesystem_path" && ! -L "$filesystem_path" ]]; do
        local parent_path
        parent_path="$(dirname -- "$filesystem_path")"
        [[ "$parent_path" != "$filesystem_path" ]] || break
        filesystem_path="$parent_path"
    done
    if [[ ! -e "$filesystem_path" && ! -L "$filesystem_path" ]]; then
        printf 'filesystem_path_unavailable=%s\n' "$filesystem_path"
        return 1
    fi
    local available_kib
    available_kib="$(df -Pk -- "$filesystem_path" | awk 'NR==2 {print $4}')"
    printf 'requested_path=%s\nfilesystem_path=%s\navailable_kib=%s\nrequired_kib=%s\n' \
        "$requested_path" "$filesystem_path" "$available_kib" "$required_kib"
    [[ "$available_kib" =~ ^[0-9]+$ && "$available_kib" -ge "$required_kib" ]]
}
space_preflight() {
    local required_kib="${1:-5767168}"
    local result=0
    space_check "$resolved_target_dir" "$required_kib" || result=1
    space_check "$artifact_dir" "$required_kib" || result=1
    return "$result"
}
stage() {
    local label="$1"; shift
    local directory="$artifact_dir/$label" status=0
    mkdir -- "$directory"
    if [[ -f "$artifact_dir/environment.txt" ]]; then
        cp -- "$artifact_dir/environment.txt" "$directory/environment.txt"
    fi
    printf '%q ' "$@" > "$directory/argv.txt"
    printf '\n' >> "$directory/argv.txt"
    printf '%s\0' "$@" > "$directory/argv.nul"
    date -u +%Y-%m-%dT%H:%M:%SZ > "$directory/started.txt"
    if "$@" > "$directory/stdout" 2> "$directory/stderr"; then status=0; else status=$?; fi
    printf 'exit_code=%s\n' "$status" > "$directory/status"
    date -u +%Y-%m-%dT%H:%M:%SZ > "$directory/finished.txt"
    printf '%s exit=%s\n' "$label" "$status"
    [[ "$status" == 0 ]] || failed=1
    return 0
}
finish() {
    local status=$?
    trap - EXIT
    set +e
    set +o noclobber
    cd -- "$repo_root"
    local unchanged=not-established
    local finalization_failed=0
    local source_snapshot_status=PASS
    if [[ -f "$artifact_dir/source-files.txt" ]]; then
        if ! snapshot > "$artifact_dir/source-files.after.txt" 2> "$artifact_dir/source-files.after.stderr"; then
            source_snapshot_status=FAIL
            finalization_failed=1
        fi
        while IFS= read -r file; do
            if [[ ! -f "$file" ]]; then
                printf 'missing source path: %s\n' "$file" >> "$artifact_dir/source-files.after.stderr"
                source_snapshot_status=FAIL
                finalization_failed=1
                continue
            fi
            if ! sha256sum -- "$file"; then
                source_snapshot_status=FAIL
                finalization_failed=1
            fi
        done < "$artifact_dir/source-files.after.txt" > "$artifact_dir/source.after.sha256"
        if cmp -s "$artifact_dir/source-files.txt" "$artifact_dir/source-files.after.txt" \
            && cmp -s "$artifact_dir/source.before.sha256" "$artifact_dir/source.after.sha256"; then
            unchanged=true
        else
            unchanged=false
            source_snapshot_status=FAIL
            finalization_failed=1
            [[ "$status" != 0 ]] || status=98
        fi
    else
        unchanged=false
        source_snapshot_status=FAIL
        finalization_failed=1
        [[ "$status" != 0 ]] || status=98
    fi
    if [[ -f "$artifact_dir/cargo-global-config.before.sha256" ]]; then
        if [[ -f "$cargo_global_config" ]]; then
            sha256sum -- "$cargo_global_config" > "$artifact_dir/cargo-global-config.after.sha256"
        else
            printf 'absent\n' > "$artifact_dir/cargo-global-config.after.sha256"
        fi
        if ! cmp -s "$artifact_dir/cargo-global-config.before.sha256" "$artifact_dir/cargo-global-config.after.sha256"; then
            source_snapshot_status=FAIL
            finalization_failed=1
            unchanged=false
            [[ "$status" != 0 ]] || status=98
        fi
    fi
    if [[ "$finished" != true ]]; then
        [[ "$status" != 0 ]] || status=99
        finalization_failed=1
    fi

    write_gate_status() {
        printf 'profile=map-empty-root-root-queries-v1\nexit_code=%s\nsource_tree_unchanged=%s\nsource_snapshot_status=%s\nfinished=%s\nglobal_map_any_v2_production_ready=false\n' \
            "$status" "$unchanged" "$source_snapshot_status" "$finished" > "$artifact_dir/gate.status"
    }
    native_artifact_identity_lines() {
        local path binary digest size
        local -a rows=()
        for path in \
            consumer-evidence/cli-owned-get-v1/native-false \
            consumer-evidence/cli-owned-get-v1/native-true \
            consumer-evidence/cli-positive/native-false \
            consumer-evidence/cli-positive/native-true \
            consumer-evidence/strict-l3/native-false \
            consumer-evidence/strict-l3/native-true; do
            binary="$artifact_dir/$path"
            [[ ! -L "$binary" ]] || return 1
            [[ -f "$binary" ]] || continue
            digest="$(sha256sum --binary -- "$binary")" || return 1
            digest="${digest%% *}"
            size="$(stat -c '%s' -- "$binary")" || return 1
            [[ "$digest" =~ ^[0-9a-f]{64}$ && "$size" =~ ^[0-9]+$ ]] || return 1
            rows+=("$path"$'\t'"$digest"$'\t'"$size")
        done
        if ((${#rows[@]} == 0)); then
            return 0
        fi
        printf '%s\n' "${rows[@]}" | LC_ALL=C sort
    }
    write_native_artifact_identities() {
        native_artifact_identity_lines > "$artifact_dir/native-artifact-identities.tsv"
    }
    verify_native_artifact_identities() {
        local binary_count
        native_artifact_identity_lines | cmp -s - "$artifact_dir/native-artifact-identities.tsv" || return 1
        if (( status == 0 )); then
            binary_count="$(wc -l < "$artifact_dir/native-artifact-identities.tsv")" || return 1
            [[ "$binary_count" == 6 ]]
        fi
    }
    write_checksums() {
        (cd -- "$artifact_dir" && find . -type f \
            ! -path ./SHA256SUMS \
            ! -path ./SHA256SUMS.sha256 \
            ! -path ./consumer-evidence/cli-owned-get-v1/native-false \
            ! -path ./consumer-evidence/cli-owned-get-v1/native-true \
            ! -path ./consumer-evidence/cli-positive/native-false \
            ! -path ./consumer-evidence/cli-positive/native-true \
            ! -path ./consumer-evidence/strict-l3/native-false \
            ! -path ./consumer-evidence/strict-l3/native-true \
            -print0 \
            | LC_ALL=C sort -z | xargs -0 -r sha256sum) > "$artifact_dir/SHA256SUMS" \
            && (cd -- "$artifact_dir" && sha256sum -- SHA256SUMS > SHA256SUMS.sha256)
    }
    verify_checksums() {
        (cd -- "$artifact_dir" && sha256sum --status -c SHA256SUMS) \
            && (cd -- "$artifact_dir" && sha256sum --status -c SHA256SUMS.sha256)
    }
    if ! write_native_artifact_identities; then
        finalization_failed=1
        [[ "$status" != 0 ]] || status=97
    fi
    if ! verify_native_artifact_identities; then
        finalization_failed=1
        [[ "$status" != 0 ]] || status=97
    fi
    if (( status == 0 && finalization_failed != 0 )); then status=97; fi
    if ! write_gate_status; then finalization_failed=1; status=97; fi
    if ! write_checksums; then finalization_failed=1; status=97; fi
    if ! verify_checksums; then finalization_failed=1; status=97; fi
    local artifact_symlink
    artifact_symlink="$(find "$artifact_dir" -type l -print -quit 2>/dev/null)"
    if [[ $? -ne 0 || -n "$artifact_symlink" ]]; then
        finalization_failed=1
        status=97
    fi
    if ! chmod -R a-w -- "$artifact_dir" 2>/dev/null; then
        finalization_failed=1
        status=97
    fi
    local writable_path
    writable_path="$(find "$artifact_dir" -perm /222 -print -quit 2>/dev/null)"
    if [[ $? -ne 0 || -n "$writable_path" ]]; then
        finalization_failed=1
        status=97
    fi

    # If any sealing step failed, rewrite the durable status to failure and
    # regenerate the manifest when the filesystem still permits it. The shell
    # exit code remains nonzero even if a read-only filesystem prevents repair.
    if (( finalization_failed != 0 )); then
        chmod -R u+rw -- "$artifact_dir" 2>/dev/null || true
        write_gate_status || true
        write_checksums || true
        chmod -R a-w -- "$artifact_dir" 2>/dev/null || true
        verify_checksums || true
        writable_path="$(find "$artifact_dir" -perm /222 -print -quit 2>/dev/null)"
        [[ $? -eq 0 && -z "$writable_path" ]] || status=97
    fi
    if (( status == 0 && finalization_failed == 0 )); then
        printf 'Sealed evidence: %s\n' "$artifact_dir"
    else
        printf 'Unsealed or failed evidence (exit=%s): %s\n' "$status" "$artifact_dir" >&2
    fi
    exit "$status"
}
trap finish EXIT

cd -- "$repo_root"
export LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
export CARGO_BUILD_JOBS=1
export CARGO_TERM_COLOR=never
export MIMI_EMPTY_ROOT_V1_EVIDENCE_DIR="$artifact_dir/consumer-evidence"
unset MIMI_TEST_FORCE_NO_Z3
while IFS= read -r name; do unset "$name"; done < <(compgen -e | rg '^VALGRIND_' || true)

for source_directory in src tests std scripts .cargo; do
    if [[ -d "$source_directory" ]]; then
        symlink_path="$(find "$source_directory" -type l -print -quit 2>/dev/null)"
        find_status=$?
        if [[ "$find_status" -ne 0 || -n "$symlink_path" ]]; then
            printf 'source snapshot rejects symlink or unreadable path: %s %s\n' \
                "$source_directory" "${symlink_path:-<find-failed>}" >&2
            failed=1
            exit 97
        fi
    fi
done

# The last failed integration-test link consumed about 3.5 GiB. Require a
# 5.5-GiB reserve on both the Cargo target and evidence filesystems before
# snapshotting or compiling; do not repeat an OOM run.
stage 00-space-before-snapshot space_preflight
if [[ "$failed" != 0 ]]; then exit 98; fi

snapshot > "$artifact_dir/source-files.txt"
while IFS= read -r file; do sha256sum -- "$file"; done < "$artifact_dir/source-files.txt" > "$artifact_dir/source.before.sha256"
tar -czf "$artifact_dir/source-snapshot.tar.gz" -T "$artifact_dir/source-files.txt"
{
    printf 'cwd=%s\nLLVM_SYS_181_PREFIX=%s\nCARGO_BUILD_JOBS=1\nCARGO_TERM_COLOR=never\nMIMI_EMPTY_ROOT_V1_EVIDENCE_DIR=%s\n' \
        "$repo_root" "$LLVM_SYS_181_PREFIX" "$MIMI_EMPTY_ROOT_V1_EVIDENCE_DIR"
    printf 'CARGO_HOME=%s\nCARGO_TARGET_DIR_CONFIGURED=%s\nCARGO_TARGET_DIR_RESOLVED=%s\nRUSTUP_HOME=%s\nRUSTFLAGS=%s\nCARGO_ENCODED_RUSTFLAGS=%s\nPATH=%s\n' \
        "${CARGO_HOME:-$HOME/.cargo}" "$configured_cargo_target_dir" "$resolved_target_dir" "${RUSTUP_HOME:-$HOME/.rustup}" \
        "${RUSTFLAGS:-}" "${CARGO_ENCODED_RUSTFLAGS:-}" "$PATH"
    effective_cargo_home="${CARGO_HOME:-$HOME/.cargo}"
    cargo_global_config="$effective_cargo_home/config.toml"
    if [[ -f "$cargo_global_config" ]]; then
        printf 'CARGO_GLOBAL_CONFIG=%s\n' "$cargo_global_config"
        sha256sum -- "$cargo_global_config"
    else
        printf 'CARGO_GLOBAL_CONFIG=%s (absent)\n' "$cargo_global_config"
    fi
    printf 'VALGRIND_variables=all-unset\nMIMI_TEST_FORCE_NO_Z3=unset\n'
} > "$artifact_dir/environment.txt"
if [[ -f "$cargo_global_config" ]]; then
    sha256sum -- "$cargo_global_config" > "$artifact_dir/cargo-global-config.before.sha256"
else
    printf 'absent\n' > "$artifact_dir/cargo-global-config.before.sha256"
fi
env -0 | LC_ALL=C sort -z | sha256sum > "$artifact_dir/effective-environment.sha256"
printf '%q ' "$0" "$@" > "$artifact_dir/runner.argv.txt"
printf '\n' >> "$artifact_dir/runner.argv.txt"
git rev-parse HEAD > "$artifact_dir/head.txt"
git status --short --untracked-files=no > "$artifact_dir/status.before.txt"
if [[ -n ${MIMI_EMPTY_ROOT_V1_PRECHECK_DIR:-} ]]; then cp -a -- "$MIMI_EMPTY_ROOT_V1_PRECHECK_DIR" "$artifact_dir/prechecks"; fi

stage 01-tools bash -c 'cargo --version --verbose && rustc --version --verbose && valgrind --version && "$LLVM_SYS_181_PREFIX/bin/llvm-config" --version'
if [[ "$failed" != 0 ]]; then exit 1; fi
stage 02-format cargo fmt --all -- --check
if [[ "$failed" != 0 ]]; then exit 1; fi
stage 03-build cargo test --test canonical_mir_map_empty_root_root_queries_v1_cli --no-run
if [[ "$(cat "$artifact_dir/03-build/status")" != exit_code=0 ]]; then exit 1; fi
successful_single_test_result='^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out; finished in [0-9]+([.][0-9]+)?s$'
stage test-v2-list-registry-idle-capacity cargo test --lib finished_v2_list_receipt_releases_idle_registry_capacity -- --test-threads=1 --nocapture
unit_result_lines="$(rg -c "$successful_single_test_result" "$artifact_dir/test-v2-list-registry-idle-capacity/stdout" 2>/dev/null || true)"
unit_summary_lines="$(rg -c '^test result:' "$artifact_dir/test-v2-list-registry-idle-capacity/stdout" 2>/dev/null || true)"
unit_test_lines="$(rg -Fxc 'test runtime::map_any_snapshot::tests::finished_v2_list_receipt_releases_idle_registry_capacity ... ok' "$artifact_dir/test-v2-list-registry-idle-capacity/stdout" 2>/dev/null || true)"
if [[ "$unit_result_lines" != 1 || "$unit_summary_lines" != 1 || "$unit_test_lines" != 1 ]]; then failed=1; fi
for test_name in \
    empty_root_v1_checker_reference_bytecode_native_verifier_receipt \
    empty_root_v1_source_near_misses_have_no_profile_authority \
    empty_root_v1_cli_default_and_explicit_share_all_consumer_receipts \
    empty_root_v1_cli_near_misses_fail_closed_without_legacy \
    empty_root_v1_populated_values_near_miss_uses_owned_get_v1_route; do
    stage "test-$test_name" cargo test --test canonical_mir_map_empty_root_root_queries_v1_cli "$test_name" -- --exact --test-threads=1 --nocapture
    result_lines="$(rg -c "$successful_single_test_result" "$artifact_dir/test-$test_name/stdout" 2>/dev/null || true)"
    summary_lines="$(rg -c '^test result:' "$artifact_dir/test-$test_name/stdout" 2>/dev/null || true)"
    test_lines="$(rg -Fxc "test $test_name ... ok" "$artifact_dir/test-$test_name/stdout" 2>/dev/null || true)"
    if [[ "$result_lines" != 1 || "$summary_lines" != 1 || "$test_lines" != 1 ]]; then failed=1; fi
done
stage 04-space-before-l3 space_preflight 1572864
if [[ "$(cat "$artifact_dir/04-space-before-l3/status")" == exit_code=0 ]]; then
    stage 05-strict-l3 cargo test --test canonical_mir_map_empty_root_root_queries_v1_cli empty_root_v1_strict_memcheck_l3 -- --exact --ignored --test-threads=1 --nocapture
    result_lines="$(rg -c "$successful_single_test_result" "$artifact_dir/05-strict-l3/stdout" 2>/dev/null || true)"
    summary_lines="$(rg -c '^test result:' "$artifact_dir/05-strict-l3/stdout" 2>/dev/null || true)"
    test_lines="$(rg -Fxc 'test empty_root_v1_strict_memcheck_l3 ... ok' "$artifact_dir/05-strict-l3/stdout" 2>/dev/null || true)"
    if [[ "$result_lines" != 1 || "$summary_lines" != 1 || "$test_lines" != 1 ]]; then failed=1; fi
else
    failed=1
fi
sha256sum -- "$resolved_target_dir/debug/mimi" > "$artifact_dir/mimi.sha256"
finished=true
exit "$failed"
