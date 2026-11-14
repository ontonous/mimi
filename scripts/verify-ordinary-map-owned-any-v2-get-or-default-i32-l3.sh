#!/usr/bin/env bash
set -uo pipefail

usage() {
    printf 'Usage: %s [--help]\n' "$0"
}

if [[ ${1:-} == --help ]]; then
    usage
    exit 0
fi
if [[ $# -ne 0 ]]; then
    usage >&2
    exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)" || exit 2
run_pid="$$"
run_id="${timestamp}-${run_pid}-${RANDOM}${RANDOM}"
artifact_dir="$repo_root/evidence/0.1.11-dev/gates/map-owned-any-v2-get-or-default-i32-l3-$run_id"
cargo_target_dir="/dev/shm/mimi-get-or-default-i32-l3-target-$run_id"
cargo_tmpdir="$cargo_target_dir/tmp"
test_file="tests/canonical_mir_ordinary_map_owned_any_v2_get_or_default_i32.rs"
test_target="canonical_mir_ordinary_map_owned_any_v2_get_or_default_i32"
test_name="get_or_default_i32_native_default_and_explicit_mir_are_memcheck_clean"
route_profile="ordinary-map-owned-any-v2-get-or-default-i32-v1"
llvm_prefix="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
state=INCOMPLETE
status=2
source_before_ok=false
source_after_ok=false
source_unchanged=false
test_ok=false

if [[ -e "$artifact_dir" || -L "$artifact_dir" || -e "$cargo_target_dir" || -L "$cargo_target_dir" ]]; then
    printf 'Refusing to overwrite evidence or Cargo target path.\n' >&2
    exit 2
fi
mkdir -p -- "$(dirname -- "$artifact_dir")" || exit 2
mkdir -- "$artifact_dir" || exit 2
cd -- "$repo_root" || exit 2

source_required_paths=(
    "$test_file"
    tests/support/memcheck_summary.rs
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_hit.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_miss.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_zero_default_miss.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_reachable_helper.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_wrong_string.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_wrong_bool.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_dynamic_key.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_non_i32_default.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_non_i32_instance.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_multiple_callsites.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_missing_pair_drop.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_duplicate_pair_drop.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_early_pair_drop.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_missing_root_drop.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_duplicate_root_drop.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_extra_mutation.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_alias.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_actor_caller.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_actor_shared_helper.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_unreachable_caller.mimi
    tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_user_helper.mimi
    src/core/mir/route.rs
    src/core/mir/mod.rs
    src/core/mir/receipt.rs
    src/core/mir/reference.rs
    src/core/mir/islands.rs
    src/core/mir/lower.rs
    src/core/mir/managed_any.rs
    src/core/mir/option_island.rs
    src/interp/bytecode/mir.rs
    src/interp/bytecode/mod.rs
    src/interp/bytecode/instr.rs
    src/interp/bytecode/disasm.rs
    src/interp/bytecode/vm.rs
    src/codegen/mir/eligibility.rs
    src/codegen/mir/mod.rs
    src/codegen/mir/ordinary_map_owned_any_v2.rs
    src/codegen/compile.rs
    src/core/cfg/resource_lower.rs
    src/core/ownership.rs
    src/core/resolved/mod.rs
    src/main/canonical_dispatch.rs
    src/main/build.rs
    src/main/run.rs
    src/main/verify.rs
    src/verifier/mir_capability.rs
    src/verifier/mir.rs
    src/verifier/mod.rs
    src/lib.rs
    src/core/mod.rs
    std/maps.mimi
    .gitignore
    scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l1l2.sh
    scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l3.sh
    Cargo.toml
    Cargo.lock
    build.rs
)
for optional_source in rust-toolchain rust-toolchain.toml .cargo/config .cargo/config.toml; do
    if [[ -e "$optional_source" || -L "$optional_source" ]]; then
        source_required_paths+=("$optional_source")
    fi
done

source_snapshot() {
    local phase="$1"
    local paths="$artifact_dir/source-files.$phase.nul"
    local hashes="$artifact_dir/source-hashes.$phase.tsv"
    local source_path source_hash required_path ignore_status
    git ls-files --cached --others --exclude-standard -z -- \
        src \
        "$test_file" \
        tests/support/memcheck_summary.rs \
        'tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_*.mimi' \
        std/maps.mimi \
        .gitignore \
        scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l1l2.sh \
        scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l3.sh \
        Cargo.toml Cargo.lock build.rs rust-toolchain rust-toolchain.toml \
        .cargo/config .cargo/config.toml "${source_required_paths[@]}" \
        | LC_ALL=C sort -zu > "$paths" || return 1
    tr '\0' '\n' < "$paths" > "$artifact_dir/source-files.$phase.txt" || return 1
    for required_path in "${source_required_paths[@]}"; do
        if [[ ! -f "$required_path" && ! -L "$required_path" ]]; then
            printf 'required source input is missing: %s\n' "$required_path" >&2
            return 1
        fi
        if git check-ignore -q -- "$required_path"; then
            printf 'required source input is ignored: %s\n' "$required_path" >&2
            return 1
        else
            ignore_status=$?
            if [[ "$ignore_status" != 1 ]]; then
                printf 'could not check ignore status for required source input: %s (exit %s)\n' \
                    "$required_path" "$ignore_status" >&2
                return 1
            fi
        fi
        if ! rg -F -x -q -- "$required_path" "$artifact_dir/source-files.$phase.txt"; then
            printf 'required source input is absent from the source snapshot: %s\n' \
                "$required_path" >&2
            return 1
        fi
    done
    : > "$hashes" || return 1
    while IFS= read -r -d '' source_path; do
        if [[ -f "$source_path" || -L "$source_path" ]]; then
            source_hash="$(sha256sum -- "$source_path" | awk '{print $1}')" || return 1
        else
            source_hash=MISSING
        fi
        printf '%s\t%s\n' "$source_hash" "$source_path" >> "$hashes" || return 1
    done < "$paths"
}

path_is_ignored() {
    local path="$1"
    local check_status
    git check-ignore -q -- "$path"
    check_status=$?
    if [[ "$check_status" == 0 ]]; then
        return 0
    elif [[ "$check_status" == 1 ]]; then
        return 1
    fi
    return 2
}

verify_l3_ignore_contract() {
    local check_status path
    for path in \
        "$artifact_dir/native/hit/default/program" \
        "$artifact_dir/native/hit/default/program.tmp-codex-probe"; do
        if ! path_is_ignored "$path"; then
            return 1
        fi
    done
    for path in \
        "$artifact_dir/native/hit/default/valgrind.log" \
        "$artifact_dir/gate.status"; do
        if path_is_ignored "$path"; then
            return 1
        else
            check_status=$?
            if [[ "$check_status" != 1 ]]; then
                return 1
            fi
        fi
    done
    return 0
}

branch_before="$(git branch --show-current 2>/dev/null || printf unavailable)"
head_before="$(git rev-parse HEAD 2>/dev/null || printf unavailable)"
git status --short --branch --untracked-files=all -- \
    src \
    "$test_file" \
    tests/support/memcheck_summary.rs \
    'tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_*.mimi' \
    std/maps.mimi \
    .gitignore \
    scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l1l2.sh \
    scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l3.sh \
    Cargo.toml Cargo.lock build.rs rust-toolchain rust-toolchain.toml \
    > "$artifact_dir/git-state.before.txt" 2>&1 || true
if source_snapshot before; then source_before_ok=true; fi

cat > "$artifact_dir/run-metadata.txt" <<EOF
repo_root=$repo_root
run_id=$run_id
run_timestamp_utc=$timestamp
branch_before=$branch_before
head_before=$head_before
test_target=$test_target
test_name=$test_name
route_profile=$route_profile
level=L3
llvm_sys_181_prefix=$llvm_prefix
cargo_target_dir=$cargo_target_dir
cargo_target_root=dev-shm
cargo_tmpdir=$cargo_tmpdir
cargo_build_jobs=1
cargo_profile_test_debug=0
cargo_profile_dev_debug=0
test_threads=1
EOF
df -h / /dev/shm > "$artifact_dir/disk.before.txt" 2>&1 || true
cat > "$artifact_dir/expected-run-summary.txt" <<EOF
route_profile=$route_profile
global_ordinary_map_owned_any_v2_production_ready=false
fixture_count=4
memcheck_run_count=8
default_and_explicit_mir_route_identities_match=true
EOF

if [[ "$source_before_ok" != true ]]; then
    printf 'INCOMPLETE: source snapshot could not be captured.\n' > "$artifact_dir/execution.status"
elif [[ ! -d /dev/shm || ! -w /dev/shm ]]; then
    printf 'INCOMPLETE: /dev/shm is not writable.\n' > "$artifact_dir/execution.status"
elif ! mkdir -p -- "$cargo_tmpdir"; then
    printf 'INCOMPLETE: could not create Cargo temporary directory: %s\n' "$cargo_tmpdir" \
        > "$artifact_dir/execution.status"
elif [[ ! -d "$llvm_prefix" ]]; then
    printf 'INCOMPLETE: LLVM_SYS_181_PREFIX is unavailable: %s\n' "$llvm_prefix" \
        > "$artifact_dir/execution.status"
elif ! command -v valgrind >/dev/null 2>&1; then
    printf 'INCOMPLETE: Valgrind is unavailable on PATH.\n' > "$artifact_dir/execution.status"
else
    valgrind_path="$(command -v valgrind)"
    valgrind_path="$(realpath -- "$valgrind_path")"
    if [[ -n "$(ps -eo args= | rg '[c]argo (test|build|check|clippy)' || true)" ]]; then
        printf 'INCOMPLETE: another Cargo build or test appears to be running; this gate did not start Cargo.\n' \
            > "$artifact_dir/execution.status"
    elif ! verify_l3_ignore_contract; then
        printf 'INCOMPLETE: ignore-rule visibility preflight failed.\n' > "$artifact_dir/execution.status"
    else
        {
            printf 'valgrind_path=%s\n' "$valgrind_path"
            printf 'command=env CARGO_TARGET_DIR=%s TMPDIR=%s CARGO_BUILD_JOBS=1 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 LLVM_SYS_181_PREFIX=%s MIMI_L3_VALGRIND=%s MIMI_GET_OR_DEFAULT_I32_L3_ARTIFACT_DIR=%s cargo test --manifest-path %s --test %s %s -- --ignored --exact --show-output --test-threads=1\n' \
                "$cargo_target_dir" "$cargo_tmpdir" "$llvm_prefix" "$valgrind_path" "$artifact_dir" \
                "$repo_root/Cargo.toml" "$test_target" "$test_name"
        } > "$artifact_dir/command.txt"
        env \
            "CARGO_TARGET_DIR=$cargo_target_dir" \
            "TMPDIR=$cargo_tmpdir" \
            CARGO_BUILD_JOBS=1 \
            CARGO_PROFILE_TEST_DEBUG=0 \
            CARGO_PROFILE_DEV_DEBUG=0 \
            CARGO_INCREMENTAL=1 \
            CARGO_TERM_COLOR=never \
            "LLVM_SYS_181_PREFIX=$llvm_prefix" \
            "MIMI_L3_VALGRIND=$valgrind_path" \
            "MIMI_GET_OR_DEFAULT_I32_L3_ARTIFACT_DIR=$artifact_dir" \
            cargo test --manifest-path "$repo_root/Cargo.toml" \
                --test "$test_target" "$test_name" -- \
                --ignored --exact --show-output --test-threads=1 \
                > "$artifact_dir/cargo.stdout" 2> "$artifact_dir/cargo.stderr"
        status=$?
        if [[ "$status" == 0 ]] \
            && rg -Fq -- "test $test_name ..." "$artifact_dir/cargo.stdout" \
            && rg -q '^test result: ok\. 1 passed; 0 failed;' "$artifact_dir/cargo.stdout" \
            && cmp -s "$artifact_dir/expected-run-summary.txt" \
                "$artifact_dir/native/run-summary.txt"; then
            test_ok=true
            state=PASS
            status=0
        else
            state=FAIL
            [[ "$status" != 0 ]] || status=1
        fi
        printf 'test_exit_code=%s\ntest_validation=%s\n' "$status" "$test_ok" \
            > "$artifact_dir/execution.status"
    fi
fi

if source_snapshot after; then
    source_after_ok=true
    if cmp -s "$artifact_dir/source-hashes.before.tsv" "$artifact_dir/source-hashes.after.tsv"; then
        source_unchanged=true
    fi
fi
branch_after="$(git branch --show-current 2>/dev/null || printf unavailable)"
head_after="$(git rev-parse HEAD 2>/dev/null || printf unavailable)"
sha256sum -- "$artifact_dir/source-hashes.before.tsv" \
    > "$artifact_dir/source-hashes.before.sha256" 2>&1 || true
sha256sum -- "$artifact_dir/source-hashes.after.tsv" \
    > "$artifact_dir/source-hashes.after.sha256" 2>&1 || true
git status --short --branch --untracked-files=all -- \
    src \
    "$test_file" \
    tests/support/memcheck_summary.rs \
    'tests/fixtures/mir_ordinary_map_owned_any_v2_get_or_default_i32_*.mimi' \
    std/maps.mimi \
    .gitignore \
    scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l1l2.sh \
    scripts/verify-ordinary-map-owned-any-v2-get-or-default-i32-l3.sh \
    Cargo.toml Cargo.lock build.rs rust-toolchain rust-toolchain.toml \
    > "$artifact_dir/git-state.after.txt" 2>&1 || true
df -h / /dev/shm > "$artifact_dir/disk.after.txt" 2>&1 || true
if [[ "$source_before_ok" != true || "$source_after_ok" != true || "$source_unchanged" != true \
    || "$branch_after" != "$branch_before" || "$head_after" != "$head_before" ]]; then
    state=FAIL
    status=1
fi
evidence_visible=false
if path_is_ignored "$artifact_dir/gate.status"; then
    state=FAIL
    status=1
    printf 'evidence_visibility=FAIL\npath=%s\n' \
        "${artifact_dir#"$repo_root"/}/gate.status" > "$artifact_dir/evidence-visibility.txt"
else
    ignore_status=$?
    if [[ "$ignore_status" == 1 ]]; then
        evidence_visible=true
        printf 'evidence_visibility=PASS\npath=%s\n' \
            "${artifact_dir#"$repo_root"/}/gate.status" > "$artifact_dir/evidence-visibility.txt"
    else
        state=FAIL
        status=1
        printf 'evidence_visibility=FAIL\ncheck_ignore_exit_code=%s\n' "$ignore_status" \
            > "$artifact_dir/evidence-visibility.txt"
    fi
fi
write_gate_status() {
    {
        printf 'state=%s\nexit_code=%s\nprofile=%s\nlevel=L3\n' "$state" "$status" "$route_profile"
        printf 'branch_before=%s\nhead_before=%s\nbranch_after=%s\nhead_after=%s\n' \
            "$branch_before" "$head_before" "$branch_after" "$head_after"
        printf 'source_before_captured=%s\nsource_after_captured=%s\nsource_unchanged=%s\n' \
            "$source_before_ok" "$source_after_ok" "$source_unchanged"
        printf 'strict_memcheck_test_passed=%s\nevidence_visible=%s\n' "$test_ok" "$evidence_visible"
        printf 'global_ordinary_map_owned_any_v2_production_ready=false\n'
    } > "$artifact_dir/gate.status"
}
write_gate_status
if (cd -- "$artifact_dir" && find . -type f ! -name SHA256SUMS -print0 \
    | LC_ALL=C sort -z | xargs -0 -r sha256sum) > "$artifact_dir/SHA256SUMS"; then
    :
else
    state=FAIL
    status=1
    write_gate_status
    (cd -- "$artifact_dir" && find . -type f ! -name SHA256SUMS -print0 \
        | LC_ALL=C sort -z | xargs -0 -r sha256sum) > "$artifact_dir/SHA256SUMS" || true
fi

printf 'GetOrDefault<i32> L3 %s: %s (exit %s)\n' "$run_id" "$state" "$status"
printf 'Evidence: %s\n' "$artifact_dir"
exit "$status"
