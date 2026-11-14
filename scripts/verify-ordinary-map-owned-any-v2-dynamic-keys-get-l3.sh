#!/usr/bin/env bash
set -Eeuo pipefail
set -o noclobber
umask 077

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
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
run_id="${timestamp}-$$-${RANDOM}${RANDOM}"
test_file=tests/canonical_mir_ordinary_map_any_v2_dynamic_keys_get_memcheck_l3.rs
test_target=canonical_mir_ordinary_map_any_v2_dynamic_keys_get_memcheck_l3
test_name=ordinary_map_owned_any_v2_dynamic_keys_get_default_and_explicit_are_memcheck_clean
inherited_cargo_target_dir="${CARGO_TARGET_DIR:-<unset>}"
inherited_llvm_prefix="${LLVM_SYS_181_PREFIX:-<unset>}"

artifact_dir="$(realpath -m -- "/tmp/mimi-ordinary-map-owned-any-v2-dynamic-keys-get-l3-${run_id}-runner")"
if [[ -v MIMI_MAP_DYNAMIC_KEYS_GET_L3_ARTIFACT_DIR && -n "$MIMI_MAP_DYNAMIC_KEYS_GET_L3_ARTIFACT_DIR" ]]; then
    requested_test_artifact_dir="$MIMI_MAP_DYNAMIC_KEYS_GET_L3_ARTIFACT_DIR"
else
    requested_test_artifact_dir="/tmp/mimi-ordinary-map-owned-any-v2-dynamic-keys-get-l3-${run_id}-test"
fi
case "$requested_test_artifact_dir" in
    /*) ;;
    *) requested_test_artifact_dir="$repo_root/$requested_test_artifact_dir" ;;
esac
test_artifact_dir="$(realpath -m -- "$requested_test_artifact_dir")"
target_dir="$(realpath -m -- "/tmp/mimi-ordinary-map-owned-any-v2-dynamic-keys-get-l3-${run_id}-target")"
gate_log="$(realpath -m -- "/tmp/mimi-ordinary-map-owned-any-v2-dynamic-keys-get-l3-${run_id}.log")"

if [[ -e "$artifact_dir" || -L "$artifact_dir" ]]; then
    printf 'refusing to overwrite runner evidence directory: %s\n' "$artifact_dir" >&2
    exit 2
fi
mkdir -- "$artifact_dir"

status=2
state=INCOMPLETE
test_started=false
test_exit=not-run
source_before_ok=false
source_after_ok=false
source_unchanged=false
test_result_ok=false
snapshot_started=false
valgrind_path=unavailable
llvm_prefix="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
llvm_config_path=unavailable

finalize() {
    local shell_status=$?
    trap - EXIT
    set +e
    if [[ "$shell_status" -ne 0 && "$status" -eq 0 ]]; then
        status="$shell_status"
        state=FAIL
    fi

    if [[ "$snapshot_started" == true ]]; then
        if source_snapshot after; then
            source_after_ok=true
            if cmp -s "$artifact_dir/source-snapshot.before.tsv" \
                "$artifact_dir/source-snapshot.after.tsv" \
                && cmp -s "$artifact_dir/source-snapshot.before.paths.nul" \
                    "$artifact_dir/source-snapshot.after.paths.nul"; then
                source_unchanged=true
            else
                status=98
                state=FAIL
            fi
        else
            source_after_ok=false
            status=98
            state=FAIL
        fi
    fi

    if [[ "$test_started" == true ]]; then
        local test_line_count=0 result_line_count=0 matches
        if [[ -f "$artifact_dir/test.stdout" ]]; then
            matches="$(grep -Fxc -- "test $test_name ... ok" "$artifact_dir/test.stdout" || true)"
            test_line_count=$((test_line_count + matches))
            matches="$(grep -Ec '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in [0-9]+(\.[0-9]+)?s$' \
                "$artifact_dir/test.stdout" || true)"
            result_line_count=$((result_line_count + matches))
        fi
        if [[ -f "$artifact_dir/test.stderr" ]]; then
            matches="$(grep -Fxc -- "test $test_name ... ok" "$artifact_dir/test.stderr" || true)"
            test_line_count=$((test_line_count + matches))
            matches="$(grep -Ec '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in [0-9]+(\.[0-9]+)?s$' \
                "$artifact_dir/test.stderr" || true)"
            result_line_count=$((result_line_count + matches))
        fi
        if [[ "$test_exit" == 0 && "$test_line_count" -eq 1 && "$result_line_count" -eq 1 ]]; then
            test_result_ok=true
        else
            test_result_ok=false
            if [[ "$status" -eq 0 ]]; then
                status=1
            fi
            state=FAIL
        fi
        {
            printf 'exit_code=%s\n' "$test_exit"
            printf 'expected_test_line_count=1\nobserved_test_line_count=%s\n' "$test_line_count"
            printf 'expected_result_line_count=1\nobserved_result_line_count=%s\n' "$result_line_count"
            printf 'expected_passed=1\nexpected_failed=0\nexpected_ignored=0\n'
            printf 'validation=%s\n' "$test_result_ok"
        } > "$artifact_dir/test.validation.txt"
    fi

    if [[ -e "$gate_log" || -L "$gate_log" ]]; then
        {
            printf 'run_id=%s\nstate=%s\nexit_code=%s\n' "$run_id" "$state" "$status"
            if [[ -f "$artifact_dir/test.stdout" ]]; then
                printf '\n===== cargo stdout =====\n'
                cat -- "$artifact_dir/test.stdout"
            fi
            if [[ -f "$artifact_dir/test.stderr" ]]; then
                printf '\n===== cargo stderr =====\n'
                cat -- "$artifact_dir/test.stderr"
            fi
        } >| "$gate_log" 2>/dev/null || true
    fi

    {
        printf 'profile=ordinary-map-owned-any-v2-dynamic-keys-get-v1\n'
        printf 'state=%s\nexit_code=%s\n' "$state" "$status"
        printf 'runner_artifact_dir=%s\ntest_artifact_dir=%s\nCARGO_TARGET_DIR=%s\ngate_log=%s\n' \
            "$artifact_dir" "$test_artifact_dir" "$target_dir" "$gate_log"
        printf 'test_started=%s\ntest_exit_code=%s\ntest_result_validation=%s\n' \
            "$test_started" "$test_exit" "$test_result_ok"
        printf 'source_snapshot_before_ok=%s\nsource_snapshot_after_ok=%s\nsource_snapshot_unchanged=%s\n' \
            "$source_before_ok" "$source_after_ok" "$source_unchanged"
    } > "$artifact_dir/run-status.txt"

    if [[ "$status" -eq 0 ]]; then
        printf 'DynamicKeysGet strict L3 passed; evidence: %s\n' "$artifact_dir"
        printf 'Test evidence: %s\nCargo target: %s\nGate log: %s\n' \
            "$test_artifact_dir" "$target_dir" "$gate_log"
    else
        printf 'DynamicKeysGet strict L3 %s (exit %s); partial evidence: %s\n' \
            "$state" "$status" "$artifact_dir" >&2
        printf 'Test evidence path: %s\nCargo target path: %s\nGate log path: %s\n' \
            "$test_artifact_dir" "$target_dir" "$gate_log" >&2
    fi
    exit "$status"
}
trap finalize EXIT

record_preflight_error() {
    printf 'error=%s\n' "$1" >> "$artifact_dir/preflight.errors.txt"
    printf '%s\n' "$1" >&2
}

path_is_inside_repo() {
    [[ "$1" == "$repo_root" || "$1" == "$repo_root/"* ]]
}

paths_overlap() {
    local left="$1" right="$2"
    [[ "$left" == "$right" || "$left" == "$right/"* || "$right" == "$left/"* ]]
}

for path_label in runner_artifact test_artifact cargo_target gate_log; do
    case "$path_label" in
        runner_artifact) path_value="$artifact_dir" ;;
        test_artifact) path_value="$test_artifact_dir" ;;
        cargo_target) path_value="$target_dir" ;;
        gate_log) path_value="$gate_log" ;;
    esac
    if path_is_inside_repo "$path_value"; then
        record_preflight_error "$path_label path must be outside the repository: $path_value"
        status=2
        exit 2
    fi
done

all_paths=("$artifact_dir" "$test_artifact_dir" "$target_dir" "$gate_log")
for ((left_index = 0; left_index < ${#all_paths[@]}; left_index++)); do
    for ((right_index = left_index + 1; right_index < ${#all_paths[@]}; right_index++)); do
        if paths_overlap "${all_paths[left_index]}" "${all_paths[right_index]}"; then
            record_preflight_error "artifact, target, and log paths must be distinct and non-nested: ${all_paths[left_index]} ; ${all_paths[right_index]}"
            status=2
            exit 2
        fi
    done
done

if [[ -e "$test_artifact_dir" || -L "$test_artifact_dir" ]]; then
    record_preflight_error "refusing to overwrite test evidence path: $test_artifact_dir"
    status=2
    exit 2
fi
if [[ -e "$target_dir" || -L "$target_dir" ]]; then
    record_preflight_error "refusing to reuse Cargo target path: $target_dir"
    status=2
    exit 2
fi
if [[ -e "$gate_log" || -L "$gate_log" ]]; then
    record_preflight_error "refusing to overwrite gate log: $gate_log"
    status=2
    exit 2
fi
test_artifact_parent="${test_artifact_dir%/*}"
[[ -n "$test_artifact_parent" ]] || test_artifact_parent=/
mkdir -p -- "$test_artifact_parent"
mkdir -- "$target_dir"
: > "$gate_log"

resolve_command() {
    local name="$1" candidate
    candidate="$(command -v "$name")" || return 1
    case "$candidate" in
        /*) printf '%s\n' "$candidate" ;;
        *) printf '%s/%s\n' "$PWD" "$candidate" ;;
    esac
}

for tool in cargo rustc sha256sum realpath find sort cmp grep env dirname; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        record_preflight_error "required tool is unavailable: $tool"
        status=2
        exit 2
    fi
done
cargo_bin="$(resolve_command cargo)" || { record_preflight_error 'cannot resolve cargo executable'; exit 2; }
rustc_bin="$(resolve_command rustc)" || { record_preflight_error 'cannot resolve rustc executable'; exit 2; }
env_bin="$(resolve_command env)" || { record_preflight_error 'cannot resolve env executable'; exit 2; }
sha256sum_bin="$(resolve_command sha256sum)" || { record_preflight_error 'cannot resolve sha256sum executable'; exit 2; }

if [[ -v MIMI_L3_VALGRIND && -n "$MIMI_L3_VALGRIND" ]]; then
    valgrind_requested="$MIMI_L3_VALGRIND"
else
    valgrind_requested="$(command -v valgrind || true)"
fi
if [[ -z "$valgrind_requested" ]]; then
    record_preflight_error 'Valgrind is unavailable; strict DynamicKeysGet L3 cannot run'
    status=2
    exit 2
fi
case "$valgrind_requested" in
    /*) ;;
    */*) valgrind_requested="$PWD/$valgrind_requested" ;;
    *) valgrind_requested="$(command -v "$valgrind_requested" || true)" ;;
esac
if [[ -n "$valgrind_requested" ]]; then
    valgrind_path="$(realpath -- "$valgrind_requested" 2>/dev/null || true)"
fi
if [[ -z "$valgrind_path" || ! -f "$valgrind_path" || ! -x "$valgrind_path" ]]; then
    record_preflight_error "Valgrind must resolve to an executable regular file: $valgrind_requested"
    status=2
    exit 2
fi

case "$llvm_prefix" in
    /*) ;;
    *) llvm_prefix="$repo_root/$llvm_prefix" ;;
esac
if [[ ! -d "$llvm_prefix" ]]; then
    record_preflight_error "LLVM_SYS_181_PREFIX directory is unavailable: $llvm_prefix"
    status=2
    exit 2
fi
llvm_prefix="$(realpath -- "$llvm_prefix")"
if [[ -x "$llvm_prefix/bin/llvm-config" ]]; then
    llvm_config_path="$(realpath -- "$llvm_prefix/bin/llvm-config")"
elif [[ -x "$llvm_prefix/llvm-config" ]]; then
    llvm_config_path="$(realpath -- "$llvm_prefix/llvm-config")"
else
    record_preflight_error "LLVM wrapper has no executable llvm-config: $llvm_prefix"
    status=2
    exit 2
fi

export LLVM_SYS_181_PREFIX="$llvm_prefix"
export MIMI_REQUIRE_VALGRIND=1
export MIMI_L3_VALGRIND="$valgrind_path"
export MIMI_MAP_DYNAMIC_KEYS_GET_L3_ARTIFACT_DIR="$test_artifact_dir"
export CARGO_TARGET_DIR="$target_dir"
export CARGO_BUILD_JOBS=1
export CARGO_TERM_COLOR=never
export MIMI_STDLIB="$repo_root/std"

{
    printf 'repo_root=%s\nrun_id=%s\n' "$repo_root" "$run_id"
    printf 'LLVM_SYS_181_PREFIX=%s\nMIMI_REQUIRE_VALGRIND=1\nMIMI_L3_VALGRIND=%s\n' \
        "$LLVM_SYS_181_PREFIX" "$MIMI_L3_VALGRIND"
    printf 'MIMI_MAP_DYNAMIC_KEYS_GET_L3_ARTIFACT_DIR=%s\n' "$MIMI_MAP_DYNAMIC_KEYS_GET_L3_ARTIFACT_DIR"
    printf 'CARGO_TARGET_DIR=%s\nCARGO_BUILD_JOBS=1\nCARGO_TERM_COLOR=never\nMIMI_STDLIB=%s\n' \
        "$CARGO_TARGET_DIR" "$MIMI_STDLIB"
    printf 'inherited_CARGO_TARGET_DIR=%s\ninherited_LLVM_SYS_181_PREFIX=%s\n' \
        "$inherited_cargo_target_dir" "$inherited_llvm_prefix"
} > "$artifact_dir/runner-environment.txt"

record_tool() {
    local label="$1" executable="$2"
    shift 2
    local tool_dir="$artifact_dir/toolchain/$label" exit_code
    mkdir -p -- "$tool_dir"
    {
        printf '%q ' "$executable" "$@"
        printf '\n'
    } > "$tool_dir/command.txt"
    printf '%s\0' "$executable" "$@" > "$tool_dir/argv.nul"
    if "$executable" "$@" > "$tool_dir/stdout" 2> "$tool_dir/stderr"; then
        exit_code=0
    else
        exit_code=$?
    fi
    printf 'exit_code=%s\n' "$exit_code" > "$tool_dir/exit.txt"
    return "$exit_code"
}

mkdir -p -- "$artifact_dir/toolchain"
toolchain_status=0
record_tool bash "$(resolve_command bash)" --version || toolchain_status=1
record_tool cargo "$cargo_bin" --version --verbose || toolchain_status=1
record_tool rustc "$rustc_bin" --version --verbose || toolchain_status=1
record_tool valgrind "$valgrind_path" --version || toolchain_status=1
record_tool llvm-config "$llvm_config_path" --version || toolchain_status=1
record_tool uname "$(resolve_command uname)" -a || toolchain_status=1
{
    printf 'repo_root=%s\nrunner_artifact_dir=%s\ntest_artifact_dir=%s\n' \
        "$repo_root" "$artifact_dir" "$test_artifact_dir"
    printf 'cargo=%s\nrustc=%s\nvalgrind=%s\nllvm_config=%s\n' \
        "$cargo_bin" "$rustc_bin" "$valgrind_path" "$llvm_config_path"
    printf 'CARGO_TARGET_DIR=%s\nLLVM_SYS_181_PREFIX=%s\n' "$CARGO_TARGET_DIR" "$LLVM_SYS_181_PREFIX"
    printf '\n[cargo]\n'; cat "$artifact_dir/toolchain/cargo/stdout"
    printf '\n[rustc]\n'; cat "$artifact_dir/toolchain/rustc/stdout"
    printf '\n[valgrind]\n'; cat "$artifact_dir/toolchain/valgrind/stdout"
    printf '\n[llvm-config]\n'; cat "$artifact_dir/toolchain/llvm-config/stdout"
} > "$artifact_dir/toolchain.txt"
"$sha256sum_bin" -- "$cargo_bin" "$rustc_bin" "$valgrind_path" "$llvm_config_path" \
    > "$artifact_dir/toolchain/binaries.sha256"
if [[ "$toolchain_status" -ne 0 ]]; then
    record_preflight_error 'one or more toolchain version commands failed; see toolchain captures'
    status=2
    exit 2
fi

source_snapshot() {
    local phase="$1" paths_file="$artifact_dir/source-snapshot.$1.paths.nul"
    local manifest="$artifact_dir/source-snapshot.$1.tsv"
    local source_path digest
    (
        cd -- "$repo_root"
        {
            for directory in src tests std scripts .cargo; do
                [[ ! -d "$directory" ]] || find "$directory" \( -type f -o -type l \) -print0
            done
            for file in Cargo.toml Cargo.lock build.rs rust-toolchain rust-toolchain.toml \
                .gitignore .cargo/config .cargo/config.toml; do
                [[ ! -f "$file" && ! -L "$file" ]] || printf '%s\0' "$file"
            done
        } | LC_ALL=C sort -zu > "$paths_file"
    ) || return 1
    : > "$manifest" || return 1
    while IFS= read -r -d '' source_path; do
        if [[ ! -f "$repo_root/$source_path" && ! -L "$repo_root/$source_path" ]]; then
            printf 'missing=%s\n' "$source_path" >> "$manifest"
            return 1
        fi
        digest="$("$sha256sum_bin" -- "$repo_root/$source_path")" || return 1
        digest="${digest%% *}"
        printf '%s\t%s\n' "$digest" "$source_path" >> "$manifest" || return 1
    done < "$paths_file"
    "$sha256sum_bin" -- "$manifest" > "$artifact_dir/source-snapshot.$phase.manifest.sha256" || return 1
}

if [[ ! -f "$repo_root/$test_file" ]]; then
    record_preflight_error "required ignored L3 test file is missing: $test_file"
    status=2
    exit 2
fi
snapshot_started=true
if source_snapshot before; then
    source_before_ok=true
else
    record_preflight_error 'could not create the before source path and SHA snapshot'
    status=2
    exit 2
fi

test_arguments=(
    test
    --manifest-path "$repo_root/Cargo.toml"
    --test "$test_target"
    "$test_name"
    --
    --exact
    --ignored
    --nocapture
    --test-threads=1
)
launch_arguments=(
    -u VALGRIND_OPTS
    "LLVM_SYS_181_PREFIX=$LLVM_SYS_181_PREFIX"
    MIMI_REQUIRE_VALGRIND=1
    "MIMI_L3_VALGRIND=$MIMI_L3_VALGRIND"
    "MIMI_MAP_DYNAMIC_KEYS_GET_L3_ARTIFACT_DIR=$MIMI_MAP_DYNAMIC_KEYS_GET_L3_ARTIFACT_DIR"
    "CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
    CARGO_BUILD_JOBS=1
    CARGO_TERM_COLOR=never
    "MIMI_STDLIB=$MIMI_STDLIB"
    "$cargo_bin"
    "${test_arguments[@]}"
)
{
    printf '%q ' "$env_bin" "${launch_arguments[@]}"
    printf '\n'
} > "$artifact_dir/test.command.txt"
printf '%s\0' "$env_bin" "${launch_arguments[@]}" > "$artifact_dir/test.launcher.argv.nul"
printf '%s\0' "$cargo_bin" "${test_arguments[@]}" > "$artifact_dir/test.argv.nul"
cp -- "$artifact_dir/runner-environment.txt" "$artifact_dir/test.environment.txt"

test_started=true
if "$env_bin" "${launch_arguments[@]}" > "$artifact_dir/test.stdout" 2> "$artifact_dir/test.stderr"; then
    test_exit=0
else
    test_exit=$?
fi
printf 'exit_code=%s\n' "$test_exit" > "$artifact_dir/test.exit.txt"
if [[ "$test_exit" -eq 0 ]]; then
    status=0
    state=PASS
else
    status="$test_exit"
    state=FAIL
fi
exit "$status"
