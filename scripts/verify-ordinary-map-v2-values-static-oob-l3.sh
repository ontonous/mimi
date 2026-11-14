#!/usr/bin/env bash
# Source-bound strict Memcheck gate for receipt-bound static Values OOB traps.
set -Eeuo pipefail

if [[ ${1:-} == --help ]]; then
    printf 'Usage: %s [run-id]\n' "$0"
    exit 0
fi
if [[ $# -gt 1 ]]; then
    printf 'Usage: %s [run-id]\n' "$0" >&2
    exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
run_id="${1:-$(date -u +%Y%m%dt%H%M%Sz)-$$}"
[[ "$run_id" =~ ^[a-z0-9][a-z0-9-]{0,80}$ ]] || {
    printf 'Invalid run id: %s\n' "$run_id" >&2
    exit 2
}
artifact_env=MIMI_MAP_V2_VALUES_STATIC_OOB_L3_ARTIFACT_DIR
artifact_dir="$repo_root/evidence/0.1.11-dev/gates/map-v2-values-static-oob-l3-$run_id"
target_dir="${MIMI_MAP_V2_VALUES_STATIC_OOB_L3_TARGET_DIR:-${TMPDIR:-/tmp}/mimi-target-map-v2-values-static-oob-l3-$run_id}"
llvm_prefix="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
case "$target_dir" in /*) ;; *) target_dir="$repo_root/$target_dir" ;; esac
case "$llvm_prefix" in /*) ;; *) llvm_prefix="$repo_root/$llvm_prefix" ;; esac

for tool in cargo rustc valgrind cc sha256sum realpath git find sort cmp grep xargs awk tr date mkdir cat env rm; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'static Values OOB strict L3 requires %s\n' "$tool" >&2
        exit 2
    }
done
[[ -d "$llvm_prefix" ]] || {
    printf 'LLVM wrapper directory is unavailable: %s\n' "$llvm_prefix" >&2
    exit 2
}
llvm_prefix="$(realpath -- "$llvm_prefix")"
target_dir="$(realpath -m -- "$target_dir")"
case "$target_dir/" in
    "$repo_root/"*)
        printf 'Cargo target directory must remain outside the repository: %s\n' "$target_dir" >&2
        exit 2
        ;;
esac
[[ ! -e "$artifact_dir" && ! -L "$artifact_dir" ]] || {
    printf 'Refusing to overwrite static Values OOB evidence: %s\n' "$artifact_dir" >&2
    exit 2
}
mkdir -p -- "$(dirname -- "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
cd -- "$repo_root"

source_file_list() {
    (
        cd -- "$repo_root"
        {
            for directory in src tests scripts .cargo; do
                [[ ! -d "$directory" ]] || find "$directory" -type f -print0
            done
            find . -maxdepth 1 -type f \
                \( -name 'Cargo.toml' -o -name 'Cargo.lock' -o -name '.gitignore' \
                    -o -name 'build.rs' -o -name 'rust-toolchain*' \
                    -o -name 'rustfmt.toml' -o -name '.rustfmt.toml' \) -print0
        } | LC_ALL=C sort -zu
    )
}

hash_source_list() {
    xargs -0 -r sha256sum -- < "$1"
}

status=0
finalize() {
    local shell_status=$?
    trap - EXIT
    [[ "$shell_status" -eq 0 ]] || status="$shell_status"

    if [[ -d "$artifact_dir/native-tmp" ]]; then
        if rm -rf -- "$artifact_dir/native-tmp"; then
            printf 'true\n' > "$artifact_dir/native-tmp-cleaned.txt"
        else
            [[ "$status" -ne 0 ]] || status=96
            printf 'false\n' > "$artifact_dir/native-tmp-cleaned.txt"
        fi
    else
        printf 'true\n' > "$artifact_dir/native-tmp-cleaned.txt"
    fi

    if [[ -f "$artifact_dir/source-files.before.nul" ]]; then
        if source_file_list > "$artifact_dir/source-files.after.nul" 2> "$artifact_dir/source-snapshot.after.stderr"; then
            tr '\0' '\n' < "$artifact_dir/source-files.after.nul" > "$artifact_dir/source-files.after.txt"
            if hash_source_list "$artifact_dir/source-files.after.nul" > "$artifact_dir/source.after.sha256" \
                && cmp -s "$artifact_dir/source-files.before.nul" "$artifact_dir/source-files.after.nul" \
                && cmp -s "$artifact_dir/source.before.sha256" "$artifact_dir/source.after.sha256"; then
                printf 'stable=true\n' > "$artifact_dir/source-snapshot.status"
            else
                printf 'stable=false\n' > "$artifact_dir/source-snapshot.status"
                [[ "$status" -ne 0 ]] || status=98
            fi
        else
            printf 'stable=false\ncapture_failed=true\n' > "$artifact_dir/source-snapshot.status"
            [[ "$status" -ne 0 ]] || status=98
        fi
    else
        printf 'stable=false\nbefore_capture_missing=true\n' > "$artifact_dir/source-snapshot.status"
        [[ "$status" -ne 0 ]] || status=98
    fi

    printf 'profile=map-v2-values-static-oob-strict-memcheck-v1\nexit_code=%s\n' \
        "$status" > "$artifact_dir/gate.status"
    if ! (
        cd -- "$artifact_dir"
        find . -type f ! -name SHA256SUMS ! -name SHA256SUMS.sha256 -print0 \
            | LC_ALL=C sort -z | xargs -0 -r sha256sum > SHA256SUMS
        sha256sum --check SHA256SUMS >/dev/null
        sha256sum SHA256SUMS > SHA256SUMS.sha256
        sha256sum --check SHA256SUMS.sha256 >/dev/null
    ); then
        [[ "$status" -ne 0 ]] || status=99
        printf 'profile=map-v2-values-static-oob-strict-memcheck-v1\nexit_code=%s\n' \
            "$status" > "$artifact_dir/gate.status"
    fi
    if [[ "$status" -eq 0 ]]; then
        printf 'static Values OOB strict L3 passed; evidence: %s\n' "$artifact_dir"
    else
        printf 'static Values OOB strict L3 failed (exit %s); evidence: %s\n' \
            "$status" "$artifact_dir" >&2
    fi
    exit "$status"
}
trap finalize EXIT

source_file_list > "$artifact_dir/source-files.before.nul"
tr '\0' '\n' < "$artifact_dir/source-files.before.nul" > "$artifact_dir/source-files.before.txt"
hash_source_list "$artifact_dir/source-files.before.nul" > "$artifact_dir/source.before.sha256"
printf '%s\n' "$(sha256sum -- "$artifact_dir/source.before.sha256" | awk '{print $1}')" \
    > "$artifact_dir/source.before.manifest.sha256"
git -C "$repo_root" rev-parse HEAD > "$artifact_dir/git-head.before.txt"
git -C "$repo_root" status --porcelain=v1 --untracked-files=all > "$artifact_dir/git-status.before.txt"
mkdir -p -- "$artifact_dir/toolchain"
for tool in bash cargo rustc valgrind cc sha256sum realpath git; do
    executable="$(type -P -- "$tool")"
    directory="$artifact_dir/toolchain/$tool"
    mkdir -- "$directory"
    printf '%s\n' "$executable" > "$directory/path.txt"
    "$executable" --version > "$directory/version.stdout" 2> "$directory/version.stderr"
    sha256sum -- "$executable" > "$directory/executable.sha256"
done
llvm_config="$llvm_prefix/bin/llvm-config"
[[ -x "$llvm_config" ]] || { printf 'missing llvm-config in wrapper: %s\n' "$llvm_config" >&2; exit 2; }
"$llvm_config" --version > "$artifact_dir/toolchain/llvm-config.version.stdout" \
    2> "$artifact_dir/toolchain/llvm-config.version.stderr"
sha256sum -- "$llvm_config" > "$artifact_dir/toolchain/llvm-config.sha256"
{
    printf 'repo_root=%s\nrun_id=%s\nartifact_directory=%s\n' "$repo_root" "$run_id" "$artifact_dir"
    printf 'LLVM_SYS_181_PREFIX=%s\nCARGO_TARGET_DIR=%s\nexternal_target=true\n' "$llvm_prefix" "$target_dir"
    printf '%s=%s\n' "$artifact_env" "$artifact_dir"
    printf 'git_head='; cat "$artifact_dir/git-head.before.txt"
    for tool in cargo rustc valgrind cc; do
        printf '%s_path=' "$tool"; cat "$artifact_dir/toolchain/$tool/path.txt"
        printf '%s_version=' "$tool"; cat "$artifact_dir/toolchain/$tool/version.stdout"
    done
    printf 'llvm_config_path=%s\nllvm_config_version=' "$llvm_config"
    cat "$artifact_dir/toolchain/llvm-config.version.stdout"
} > "$artifact_dir/toolchain.txt"

cargo_bin="$(type -P -- cargo)"
suite_failure=0

run_case() {
    local case_name="$1"
    local test_name="$2"
    local case_dir="$artifact_dir/suite/$case_name"
    local case_status
    mkdir -p -- "$case_dir"
    {
        printf '%q ' env "LLVM_SYS_181_PREFIX=$llvm_prefix" "CARGO_TARGET_DIR=$target_dir" \
            CARGO_BUILD_JOBS=1 MIMI_KEEP_TMP=1 "TMPDIR=$artifact_dir/native-tmp" \
            "$artifact_env=$artifact_dir" "$cargo_bin" test --locked \
            --manifest-path "$repo_root/Cargo.toml" --target-dir "$target_dir" \
            --lib "$test_name" -- --exact --nocapture --test-threads=1
        printf '\n'
    } > "$case_dir/command.txt"
    printf '%s\0' env "LLVM_SYS_181_PREFIX=$llvm_prefix" "CARGO_TARGET_DIR=$target_dir" \
        CARGO_BUILD_JOBS=1 MIMI_KEEP_TMP=1 "TMPDIR=$artifact_dir/native-tmp" \
        "$artifact_env=$artifact_dir" "$cargo_bin" test --locked \
        --manifest-path "$repo_root/Cargo.toml" --target-dir "$target_dir" \
        --lib "$test_name" -- --exact --nocapture --test-threads=1 > "$case_dir/command.argv.nul"
    {
        printf 'case=%s\ntest_filter=%s\nexact_filter=true\ntest_harness_threads=1\n' "$case_name" "$test_name"
        printf 'LLVM_SYS_181_PREFIX=%s\nCARGO_TARGET_DIR=%s\nCARGO_BUILD_JOBS=1\n' "$llvm_prefix" "$target_dir"
        printf 'MIMI_KEEP_TMP=1\nTMPDIR=%s/native-tmp\n' "$artifact_dir"
        printf '%s=%s\n' "$artifact_env" "$artifact_dir"
    } > "$case_dir/environment.txt"

    if env "LLVM_SYS_181_PREFIX=$llvm_prefix" "CARGO_TARGET_DIR=$target_dir" \
        CARGO_BUILD_JOBS=1 MIMI_KEEP_TMP=1 "TMPDIR=$artifact_dir/native-tmp" \
        "$artifact_env=$artifact_dir" "$cargo_bin" test --locked \
        --manifest-path "$repo_root/Cargo.toml" --target-dir "$target_dir" \
        --lib "$test_name" -- --exact --nocapture --test-threads=1 \
        > "$case_dir/stdout" 2> "$case_dir/stderr"; then
        case_status=0
    else
        case_status=$?
    fi
    printf 'exit_code=%s\n' "$case_status" > "$case_dir/status.txt"
    {
        printf '===== stdout =====\n'; cat "$case_dir/stdout"
        printf '\n===== stderr =====\n'; cat "$case_dir/stderr"
    } > "$case_dir/log"
    cat "$case_dir/stdout"
    cat "$case_dir/stderr" >&2
    if [[ "$case_status" -ne 0 && "$suite_failure" -eq 0 ]]; then
        suite_failure="$case_status"
    fi
    return 0
}

run_case raw-index-1 codegen::mir::tests::ordinary_map_owned_any_v2_values_static_oob_without_selected_target_traps_before_get
run_case raw-index-minus-2 codegen::mir::tests::ordinary_map_owned_any_v2_values_static_negative_oob_without_selected_target_traps_before_get

replay_case() {
    local case_name="$1"
    local case_dir="$artifact_dir/$case_name"
    local link_status=127
    local run_status=127
    local -a link_argv=()
    local -a run_argv=()

    mapfile -d '' -t link_argv < "$case_dir/link.replay.argv.nul"
    if (cd -- "$case_dir" && "${link_argv[@]}") \
        > "$case_dir/link.replay.stdout.bin" 2> "$case_dir/link.replay.stderr.bin"; then
        link_status=0
    else
        link_status=$?
    fi
    printf 'exit_code=%s\n' "$link_status" > "$case_dir/link.replay.status.txt"

    if [[ "$link_status" -eq 0 && -x "$case_dir/relinked-program" ]]; then
        mapfile -d '' -t run_argv < "$case_dir/valgrind.replay.argv.nul"
        if (cd -- "$case_dir" && "${run_argv[@]}") \
            > "$case_dir/valgrind.replay.stdout.bin" 2> "$case_dir/valgrind.replay.stderr.bin"; then
            run_status=0
        else
            run_status=$?
        fi
    else
        : > "$case_dir/valgrind.replay.stdout.bin"
        : > "$case_dir/valgrind.replay.stderr.bin"
    fi
    printf 'exit_code=%s\n' "$run_status" > "$case_dir/valgrind.replay.status.txt"
}

replay_case raw-index-1
replay_case raw-index-minus-2

validate_memcheck_report() {
    local report_path="$1"
    [[ -s "$report_path" ]] || return 1
    [[ "$(grep -aEc '^==[0-9]+== Memcheck, a memory error detector$' "$report_path" || true)" == 1 ]] \
        || return 1
    [[ "$(grep -aEc '^==[0-9]+== ERROR SUMMARY: 0 errors from 0 contexts [(]suppressed: 0 from 0[)]$' "$report_path" || true)" == 1 ]] \
        || return 1
    grep -aFq 'E0803' "$report_path" || return 1
    if grep -aFq 'All heap blocks were freed -- no leaks are possible' "$report_path"; then
        grep -aEq '^==[0-9]+==[[:space:]]+in use at exit: 0 bytes in 0 blocks$' "$report_path" \
            || return 1
    else
        for row in \
            'definitely lost: 0 bytes in 0 blocks' \
            'indirectly lost: 0 bytes in 0 blocks' \
            'possibly lost: 0 bytes in 0 blocks' \
            'suppressed: 0 bytes in 0 blocks'; do
            grep -aFq "$row" "$report_path" || return 1
        done
    fi
}

validate_case() {
    local case_name="$1"
    local test_name="$2"
    local expected_index="$3"
    local case_dir="$artifact_dir/$case_name"
    local suite_case_dir="$artifact_dir/suite/$case_name"
    local validation_status=0
    local replay_output_found=0
    local argv_index
    local role
    local -a replay_link_argv=()
    local -a valgrind_argv=()
    local -a valgrind_replay_argv=()
    local -a expected_valgrind_args=(
        valgrind
        --tool=memcheck
        --leak-check=full
        --show-leak-kinds=all
        --errors-for-leak-kinds=definite,indirect,possible
        --error-exitcode=97
    )
    local native_binary_source_path=""

    for file in valgrind.stderr.bin stdout.bin run.status.txt valgrind.command.argv.nul \
        valgrind.command.txt valgrind.replay.argv.nul valgrind.replay.command.txt \
        link.command.argv.nul link.command.txt link.replay.argv.nul link.replay.command.txt \
        link.replay.stdout.bin link.replay.stderr.bin link.replay.status.txt \
        valgrind.replay.stdout.bin valgrind.replay.stderr.bin valgrind.replay.status.txt \
        native-artifacts.txt program native-object runtime-archive; do
        [[ -f "$case_dir/$file" ]] || {
            printf 'missing %s/%s\n' "$case_name" "$file" >&2
            validation_status=1
        }
    done
    validate_memcheck_report "$case_dir/valgrind.stderr.bin" || validation_status=1
    [[ ! -s "$case_dir/stdout.bin" ]] || { printf '%s produced stdout\n' "$case_name" >&2; validation_status=1; }
    grep -Fxq "raw_index=$expected_index" "$case_dir/run.status.txt" || validation_status=1
    grep -Fxq 'exit_code=1' "$case_dir/run.status.txt" || validation_status=1
    grep -Fxq 'expected_exit_code=1' "$case_dir/run.status.txt" || validation_status=1
    grep -Fxq 'expected_trap=E0803' "$case_dir/run.status.txt" || validation_status=1
    mapfile -d '' -t valgrind_argv < "$case_dir/valgrind.command.argv.nul"
    mapfile -d '' -t valgrind_replay_argv < "$case_dir/valgrind.replay.argv.nul"
    [[ "${#valgrind_argv[@]}" -eq 7 && "${#valgrind_replay_argv[@]}" -eq 7 ]] \
        || validation_status=1
    for ((argv_index = 0; argv_index < ${#expected_valgrind_args[@]}; argv_index++)); do
        [[ ${valgrind_argv[argv_index]:-} == "${expected_valgrind_args[argv_index]}" ]] \
            || validation_status=1
        [[ ${valgrind_replay_argv[argv_index]:-} == "${expected_valgrind_args[argv_index]}" ]] \
            || validation_status=1
    done
    while IFS= read -r line; do
        if [[ "$line" == 'role=native-binary' ]]; then
            IFS= read -r line || break
            [[ "$line" == source_path=* ]] && native_binary_source_path="${line#source_path=}"
            break
        fi
    done < "$case_dir/native-artifacts.txt"
    [[ -n "$native_binary_source_path" ]] || validation_status=1
    [[ ${valgrind_argv[6]:-} == "$native_binary_source_path" ]] || validation_status=1
    [[ ${valgrind_replay_argv[6]:-} == './relinked-program' ]] || validation_status=1
    [[ "$(cat "$case_dir/valgrind.replay.status.txt")" == 'exit_code=1' ]] || validation_status=1
    [[ ! -s "$case_dir/valgrind.replay.stdout.bin" ]] || validation_status=1
    validate_memcheck_report "$case_dir/valgrind.replay.stderr.bin" || validation_status=1
    [[ "$(cat "$case_dir/link.replay.status.txt")" == 'exit_code=0' ]] || validation_status=1
    [[ ! -s "$case_dir/link.replay.stderr.bin" ]] || validation_status=1
    grep -Fxq 'exit_code=0' "$suite_case_dir/status.txt" || validation_status=1
    for role in native-binary native-object runtime-archive; do
        [[ "$(grep -Fxc "role=$role" "$case_dir/native-artifacts.txt" || true)" == 1 ]] \
            || validation_status=1
    done
    [[ "$(grep -Ec '^blake3=[0-9a-f]{64}$' "$case_dir/native-artifacts.txt" || true)" == 3 ]] \
        || validation_status=1
    [[ "$(grep -Ec '^hash_(before_link|before_run|after_run)=[0-9a-f]{64}$' \
        "$case_dir/native-artifacts.txt" || true)" == 8 ]] || validation_status=1
    [[ "$(grep -Fxc 'hash_before_link=<not-retained>' "$case_dir/native-artifacts.txt" || true)" == 1 ]] \
        || validation_status=1
    [[ "$(grep -Fxc 'stable_during_execution=true' "$case_dir/native-artifacts.txt" || true)" == 3 ]] \
        || validation_status=1
    mapfile -d '' -t replay_link_argv < "$case_dir/link.replay.argv.nul"
    [[ ${replay_link_argv[0]:-} == cc ]] || validation_status=1
    [[ "$(printf '%s\n' "${replay_link_argv[@]}" | grep -Fxc './native-object' || true)" == 1 ]] \
        || validation_status=1
    [[ "$(printf '%s\n' "${replay_link_argv[@]}" | grep -Fxc './runtime-archive' || true)" == 1 ]] \
        || validation_status=1
    for ((argv_index = 0; argv_index < ${#replay_link_argv[@]}; argv_index++)); do
        if [[ ${replay_link_argv[argv_index]} == "-o" ]]; then
            if [[ ${replay_link_argv[argv_index + 1]:-} == "./relinked-program" ]]; then
                replay_output_found=1
            else
                validation_status=1
            fi
            break
        fi
    done
    [[ "$replay_output_found" -eq 1 ]] || validation_status=1
    [[ -x "$case_dir/relinked-program" ]] || validation_status=1
    grep -Fxq "test $test_name ... ok" "$suite_case_dir/log" || validation_status=1
    grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;' \
        "$suite_case_dir/log" || validation_status=1
    grep -Fxq "test_filter=$test_name" "$suite_case_dir/environment.txt" || validation_status=1
    grep -Fxq 'exact_filter=true' "$suite_case_dir/environment.txt" || validation_status=1
    grep -Fq -- '--exact' "$suite_case_dir/command.txt" || validation_status=1
    return "$validation_status"
}

validation_status=0
validate_case raw-index-1 codegen::mir::tests::ordinary_map_owned_any_v2_values_static_oob_without_selected_target_traps_before_get 1 \
    || validation_status=1
validate_case raw-index-minus-2 codegen::mir::tests::ordinary_map_owned_any_v2_values_static_negative_oob_without_selected_target_traps_before_get -2 \
    || validation_status=1

if [[ "$suite_failure" -ne 0 ]]; then
    status="$suite_failure"
    printf 'strict L3 cargo case failed; see per-case suite logs\n' > "$artifact_dir/artifact-validation.txt"
    exit "$status"
fi
if [[ "$validation_status" -ne 0 ]]; then
    printf 'artifact validation failed\n' > "$artifact_dir/artifact-validation.txt"
    status=97
    exit "$status"
fi
printf 'PASS: raw indexes 1 and -2 each trapped with E0803 under strict Memcheck; stdout empty; no Memcheck or leak errors\n' \
    > "$artifact_dir/artifact-validation.txt"
