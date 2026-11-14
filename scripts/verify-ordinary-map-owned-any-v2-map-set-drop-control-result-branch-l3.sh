#!/usr/bin/env bash
# External, source-bound native/Memcheck gate for MapSetDropControl Result branches.
set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
test_name=ordinary_map_owned_any_v2_map_set_drop_control_default_and_explicit_native_memcheck_clean
test_target=canonical_mir_ordinary_map_any_v2_map_set_drop_control_memcheck_l3

: "${CARGO_TARGET_DIR:?Set CARGO_TARGET_DIR to an explicit external Cargo target directory}"
: "${MIMI_MAP_SET_DROP_CONTROL_L3_EVIDENCE_DIR:?Set MIMI_MAP_SET_DROP_CONTROL_L3_EVIDENCE_DIR to a new external evidence directory}"

[[ "$CARGO_TARGET_DIR" == /* ]] || {
    printf 'CARGO_TARGET_DIR must be an absolute external path: %s\n' "$CARGO_TARGET_DIR" >&2
    exit 2
}
[[ "$MIMI_MAP_SET_DROP_CONTROL_L3_EVIDENCE_DIR" == /* ]] || {
    printf 'MIMI_MAP_SET_DROP_CONTROL_L3_EVIDENCE_DIR must be an absolute external path: %s\n' \
        "$MIMI_MAP_SET_DROP_CONTROL_L3_EVIDENCE_DIR" >&2
    exit 2
}

requested_target_dir="$CARGO_TARGET_DIR"
requested_evidence_dir="$MIMI_MAP_SET_DROP_CONTROL_L3_EVIDENCE_DIR"
[[ ! -e "$requested_evidence_dir" && ! -L "$requested_evidence_dir" ]] || {
    printf 'Refusing to overwrite existing L3 evidence: %s\n' "$requested_evidence_dir" >&2
    exit 2
}
target_dir="$(realpath -m -- "$requested_target_dir")"
evidence_dir="$(realpath -m -- "$requested_evidence_dir")"
case "$target_dir/" in
    "$repo_root/"*)
        printf 'CARGO_TARGET_DIR must stay outside the repository: %s\n' "$target_dir" >&2
        exit 2
        ;;
esac
case "$evidence_dir/" in
    "$repo_root/"*)
        printf 'L3 evidence must stay outside the repository: %s\n' "$evidence_dir" >&2
        exit 2
        ;;
esac

llvm_prefix="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
[[ -d "$llvm_prefix" ]] || {
    printf 'LLVM_SYS_181_PREFIX is unavailable: %s\n' "$llvm_prefix" >&2
    exit 2
}
llvm_prefix="$(realpath -- "$llvm_prefix")"
for tool in cargo rustc cc valgrind sha256sum realpath find sort cmp xargs git; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'Strict MapSetDropControl L3 requires %s\n' "$tool" >&2
        exit 2
    }
done

mkdir -p -- "$target_dir" "$(dirname -- "$evidence_dir")"
mkdir -- "$evidence_dir"
evidence_dir="$(realpath -- "$evidence_dir")"
target_dir="$(realpath -- "$target_dir")"
export CARGO_TARGET_DIR="$target_dir"
export MIMI_MAP_SET_DROP_CONTROL_L3_EVIDENCE_DIR="$evidence_dir"

source_snapshot() {
    (
        cd -- "$repo_root"
        {
            for directory in src tests scripts devdocs .cargo; do
                [[ ! -d "$directory" ]] || find "$directory" -type f -print0
            done
            find . -maxdepth 1 -type f \
                \( -name 'Cargo*' -o -name '.gitignore' -o -name 'build.rs' \
                    -o -name 'rust-toolchain*' -o -name 'rustfmt.toml' -o -name '.rustfmt.toml' \) \
                -print0
        } | LC_ALL=C sort -zu
    )
}

hash_snapshot() {
    local source_list="$1"
    xargs -0 -r sha256sum -- < "$source_list"
}

status=0
test_status=not_run
source_stable=false
finalize() {
    local shell_status=$?
    trap - EXIT
    [[ "$shell_status" == 0 ]] || status="$shell_status"

    if [[ -f "$evidence_dir/source-files.before.nul" ]]; then
        source_snapshot > "$evidence_dir/source-files.after.nul" \
            2> "$evidence_dir/source-snapshot.after.stderr" || status=98
        tr '\0' '\n' < "$evidence_dir/source-files.after.nul" > "$evidence_dir/source-files.after.txt"
        if cmp -s "$evidence_dir/source-files.before.nul" "$evidence_dir/source-files.after.nul"; then
            if hash_snapshot "$evidence_dir/source-files.after.nul" > "$evidence_dir/source.after.sha256" \
                && cmp -s "$evidence_dir/source.before.sha256" "$evidence_dir/source.after.sha256"; then
                source_stable=true
            else
                status=98
            fi
        else
            diff -u "$evidence_dir/source-files.before.txt" "$evidence_dir/source-files.after.txt" \
                > "$evidence_dir/source-files.diff" || true
            status=98
        fi
    else
        status=98
    fi

    local passed=false
    if [[ "$status" == 0 && "$test_status" == 0 && "$source_stable" == true ]]; then
        passed=true
    fi
    {
        printf 'route_profile=map-any-owner-v2-map-set-drop-control-result-branch-v1\n'
        printf 'test_status=%s\nrunner_exit_code=%s\nsource_tree_unchanged=%s\n' \
            "$test_status" "$status" "$source_stable"
        printf 'strict_memcheck_required=true\nstrict_memcheck_runs_required=4\nnative_error_runs_required=2\nl3_passed=%s\n' "$passed"
    } > "$evidence_dir/gate.status"
    (
        cd -- "$evidence_dir"
        find . -type f ! -name SHA256SUMS -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum
    ) > "$evidence_dir/SHA256SUMS"
    if [[ "$passed" == true ]]; then
        printf 'MapSetDropControl strict native/Memcheck L3 passed; evidence: %s\n' "$evidence_dir"
    else
        printf 'MapSetDropControl strict native/Memcheck L3 failed (runner exit %s, test %s); evidence: %s\n' \
            "$status" "$test_status" "$evidence_dir" >&2
    fi
    exit "$status"
}
trap finalize EXIT

source_snapshot > "$evidence_dir/source-files.before.nul"
tr '\0' '\n' < "$evidence_dir/source-files.before.nul" > "$evidence_dir/source-files.before.txt"
hash_snapshot "$evidence_dir/source-files.before.nul" > "$evidence_dir/source.before.sha256"

fixture="$repo_root/tests/fixtures/mir_ordinary_map_any_v2_map_set_drop_control.mimi"
test_source="$repo_root/tests/$test_target.rs"
c_harness="$repo_root/tests/fixtures/canonical_mir_map_set_drop_control_result_l3_harness.c"
[[ -f "$fixture" && -f "$test_source" && -f "$c_harness" ]] || {
    printf 'MapSetDropControl fixture, ignored L3 integration test, or native C harness is missing\n' >&2
    exit 2
}
sha256sum -- "$fixture" "$test_source" "$c_harness" "$script_dir/$(basename -- "${BASH_SOURCE[0]}")" \
    > "$evidence_dir/gate-source-identities.sha256"
{
    printf 'repo_root=%s\nCARGO_TARGET_DIR=%s\nrequested_CARGO_TARGET_DIR=%s\n' \
        "$repo_root" "$target_dir" "$requested_target_dir"
    printf 'evidence_dir=%s\nrequested_evidence_dir=%s\n' "$evidence_dir" "$requested_evidence_dir"
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$llvm_prefix"
    printf 'cargo='; cargo --version
    printf 'rustc='; rustc --version --verbose
    printf 'cc='; cc --version
    printf 'cc_target='; cc -dumpmachine
    printf 'valgrind='; valgrind --version
    printf 'llvm_config='; "$llvm_prefix/bin/llvm-config" --version
    printf 'git_head='; git -C "$repo_root" rev-parse HEAD
    printf 'fixture_expected_sha256=29fcad6d4e2ec8c5e45be48ccc96a2a9d62567b9dd8c82bb2febdf8ff6c5a12b\n'
    printf 'strict_memcheck_runs_required=4\nnative_error_runs_required=2\n'
    printf 'ignored_test=%s\n' "$test_name"
    printf 'source_tree_change_during_gate=fail\n'
} > "$evidence_dir/environment.txt" 2>&1

args=(test --locked --manifest-path "$repo_root/Cargo.toml" --target-dir "$target_dir" --test "$test_target" -- --ignored --exact "$test_name" --nocapture)
{
    printf 'LLVM_SYS_181_PREFIX=%q CARGO_TARGET_DIR=%q CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=1 ' \
        "$llvm_prefix" "$target_dir"
    printf '%q ' cargo "${args[@]}"
    printf '\n'
} > "$evidence_dir/cargo-test.command.txt"
printf '%s\0' cargo "${args[@]}" > "$evidence_dir/cargo-test.argv.nul"
test_status=0
if LLVM_SYS_181_PREFIX="$llvm_prefix" CARGO_TARGET_DIR="$target_dir" CARGO_BUILD_JOBS=1 \
    CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=1 \
    cargo "${args[@]}" > "$evidence_dir/cargo-test.stdout" 2> "$evidence_dir/cargo-test.stderr"; then
    test_status=0
else
    test_status=$?
    status="$test_status"
fi
printf 'exit_code=%s\n' "$test_status" > "$evidence_dir/cargo-test.status"
[[ "$test_status" == 0 ]] || exit "$test_status"
if [[ "$(grep -Fxc "test $test_name ... ok" "$evidence_dir/cargo-test.stdout" || true)" != 1 ]] \
    || ! grep -Eq 'test result: ok\. 1 passed; 0 failed;' "$evidence_dir/cargo-test.stdout"; then
    printf 'Ignored L3 test did not execute exactly once and pass; inspect %s/cargo-test.stdout\n' \
        "$evidence_dir" >&2
    test_status=97
    status=97
    exit 97
fi
