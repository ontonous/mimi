#!/usr/bin/env bash
set -euo pipefail
set -o noclobber

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT_RELATIVE=scripts/verify-args-string-index-ownership-l3.sh
TEST_TARGET=args_string_index_ownership
TEST_NAME=args_string_index_ownership_is_strict_memcheck_clean
TEST_ARTIFACT_ENV=MIMI_ARGS_STRING_INDEX_ARTIFACT_DIR

die() {
    printf 'args string index strict L3: %s\n' "$1" >&2
    exit "${2:-2}"
}

for tool in cargo rustc rustup sha256sum realpath git find sort grep awk cmp wc sed; do
    command -v "$tool" >/dev/null 2>&1 || die "required tool is unavailable: $tool"
done

[[ -v MIMI_REQUIRE_VALGRIND && "$MIMI_REQUIRE_VALGRIND" == 1 ]] \
    || die 'MIMI_REQUIRE_VALGRIND must be set to 1'
[[ -v MIMI_L3_VALGRIND && -n "$MIMI_L3_VALGRIND" ]] \
    || die 'MIMI_L3_VALGRIND must name the absolute canonical Valgrind executable'
case "$MIMI_L3_VALGRIND" in
    /*) ;;
    *) die 'MIMI_L3_VALGRIND must be absolute' ;;
esac
[[ -f "$MIMI_L3_VALGRIND" && -x "$MIMI_L3_VALGRIND" ]] \
    || die 'MIMI_L3_VALGRIND must name an executable regular file'
valgrind_path="$(realpath -- "$MIMI_L3_VALGRIND")"
[[ "$valgrind_path" == "$MIMI_L3_VALGRIND" ]] \
    || die 'MIMI_L3_VALGRIND must already be a canonical path'

[[ -v "$TEST_ARTIFACT_ENV" && -n "${!TEST_ARTIFACT_ENV}" ]] \
    || die "$TEST_ARTIFACT_ENV must name a fresh external absolute test-evidence directory"
test_artifact_requested="${!TEST_ARTIFACT_ENV}"
case "$test_artifact_requested" in
    /*) ;;
    *) die "$TEST_ARTIFACT_ENV must be absolute" ;;
esac
if [[ -e "$test_artifact_requested" || -L "$test_artifact_requested" ]]; then
    die "refusing to overwrite existing test-evidence path: $test_artifact_requested"
fi
test_artifact_dir="$(realpath -m -- "$test_artifact_requested")"
repo_real="$(realpath -- "$ROOT_DIR")"
case "$test_artifact_dir/" in
    "$repo_real/"*) die "test evidence must be outside the repository: $test_artifact_dir" ;;
esac
mkdir -p -- "$(dirname "$test_artifact_dir")"
if [[ -e "$test_artifact_dir" || -L "$test_artifact_dir" ]]; then
    die "refusing to overwrite existing test-evidence path: $test_artifact_dir"
fi

# The integration test creates its own evidence directory. Keep runner captures
# and its isolated Cargo target in a separate, equally non-overwriting path.
runner_dir="${test_artifact_dir}.runner"
case "$runner_dir/" in
    "$repo_real/"*) die "runner evidence must be outside the repository: $runner_dir" ;;
esac
if [[ -e "$runner_dir" || -L "$runner_dir" ]]; then
    die "refusing to overwrite existing runner-evidence path: $runner_dir"
fi

if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper
fi
[[ -d "$LLVM_SYS_181_PREFIX" ]] || die "LLVM wrapper directory is unavailable: $LLVM_SYS_181_PREFIX"

cargo_path="$(command -v cargo)"
rustc_path="$(command -v rustc)"
rustup_path="$(command -v rustup)"
sha256sum_path="$(command -v sha256sum)"
realpath_path="$(command -v realpath)"
git_path="$(command -v git)"
for tool_path in "$cargo_path" "$rustc_path" "$rustup_path" "$sha256sum_path" "$realpath_path" "$git_path" "$valgrind_path"; do
    case "$tool_path" in
        /*) ;;
        *) die "tool path is not absolute: $tool_path" ;;
    esac
    [[ -f "$tool_path" && -x "$tool_path" ]] || die "tool is not an executable file: $tool_path"
done

target_dir="$runner_dir/cargo-target"
[[ ! -e "$target_dir" && ! -L "$target_dir" ]] \
    || die "isolated Cargo target path is not fresh: $target_dir"

if [[ -v RUSTC_WRAPPER && -n "$RUSTC_WRAPPER" ]] \
    || [[ -v RUSTC_WORKSPACE_WRAPPER && -n "$RUSTC_WORKSPACE_WRAPPER" ]]; then
    die 'RUSTC_WRAPPER and RUSTC_WORKSPACE_WRAPPER must be unset for the pinned direct compiler route'
fi
if [[ -v CARGO_BUILD_RUSTC_WRAPPER && -n "$CARGO_BUILD_RUSTC_WRAPPER" ]] \
    || [[ -v CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER && -n "$CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER" ]]; then
    die 'Cargo rustc wrapper environment overrides must be unset for the pinned direct compiler route'
fi
mkdir -- "$runner_dir"
mkdir -- "$runner_dir/toolchain"

export MIMI_REQUIRE_VALGRIND=1
export MIMI_L3_VALGRIND="$valgrind_path"
export MIMI_ARGS_STRING_INDEX_ARTIFACT_DIR="$test_artifact_dir"

path_is_within() {
    local candidate="$1" directory="$2"
    [[ "$candidate" == "$directory" || "$candidate" == "$directory"/* ]]
}

source_input_inventory() {
    {
        find src tests std -type f ! -path '*/__pycache__/*' -print0
        printf '%s\0' \
            Cargo.toml Cargo.lock build.rs \
            .cargo/config .cargo/config.toml \
            rust-toolchain rust-toolchain.toml \
            tests/args_string_index_ownership.rs \
            tests/support/memcheck_summary.rs \
            "$SCRIPT_RELATIVE"
    } | sort -zu
}

capture_source_snapshot() {
    local phase="$1" input_list="$runner_dir/source-$1.inputs.nul"
    local manifest="$runner_dir/source-$1.sha256sums" relative
    source_input_inventory > "$input_list"
    (
        cd "$ROOT_DIR"
        while IFS= read -r -d '' relative; do
            [[ -f "$relative" ]] || continue
            "$sha256sum_path" -- "$relative"
        done < "$input_list"
    ) > "$manifest"
    "$sha256sum_path" -- "$manifest" | awk '{print $1}'
}

record_tool() {
    local label="$1" executable="$2"
    shift 2
    local directory="$runner_dir/toolchain/$label" resolved status=0
    mkdir -- "$directory"
    resolved="$(realpath -- "$executable")"
    {
        printf 'invoked_path=%s\nresolved_path=%s\n' "$executable" "$resolved"
        printf 'sha256='; "$sha256sum_path" -- "$resolved" | awk '{print $1}'
    } > "$directory/identity.txt"
    printf '%q ' "$executable" "$@" > "$directory/command.txt"
    printf '\n' >> "$directory/command.txt"
    printf '%s\0' "$executable" "$@" > "$directory/command.argv.nul"
    if "$executable" "$@" > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$directory/process.status"
    return "$status"
}

cd "$ROOT_DIR"
"$git_path" -C "$ROOT_DIR" rev-parse HEAD > "$runner_dir/git-head.before.txt"
"$git_path" -C "$ROOT_DIR" branch --show-current > "$runner_dir/git-branch.before.txt"
"$git_path" -C "$ROOT_DIR" status --porcelain=v1 --untracked-files=all \
    > "$runner_dir/git-status.before.txt"
snapshot_before="$(capture_source_snapshot before)"
printf '%s\n' "$snapshot_before" > "$runner_dir/source-snapshot.before.sha256"

toolchain_status=0
record_tool rustc "$rustc_path" --version || toolchain_status=$?
record_tool cargo "$cargo_path" --version || toolchain_status=$?
record_tool rustup "$rustup_path" --version || toolchain_status=$?
record_tool rustup-cargo "$rustup_path" which cargo || toolchain_status=$?
record_tool rustup-rustc "$rustup_path" which rustc || toolchain_status=$?
record_tool valgrind "$valgrind_path" --version || toolchain_status=$?
record_tool sha256sum "$sha256sum_path" --version || toolchain_status=$?
record_tool realpath "$realpath_path" --version || toolchain_status=$?
record_tool git "$git_path" --version || toolchain_status=$?

resolved_cargo_path="$(<"$runner_dir/toolchain/rustup-cargo/stdout.bin")"
resolved_rustc_path="$(<"$runner_dir/toolchain/rustup-rustc/stdout.bin")"
for effective_tool in "$resolved_cargo_path" "$resolved_rustc_path"; do
    case "$effective_tool" in
        /*) ;;
        *) toolchain_status=1 ;;
    esac
    if [[ ! -f "$effective_tool" || ! -x "$effective_tool" ]]; then
        toolchain_status=1
    elif [[ "$(realpath -- "$effective_tool")" != "$effective_tool" ]]; then
        toolchain_status=1
    fi
done
if [[ -n "$resolved_cargo_path" && -f "$resolved_cargo_path" ]]; then
    effective_cargo_real="$(realpath -- "$resolved_cargo_path")"
    printf 'effective_path=%s\neffective_realpath=%s\neffective_sha256=' \
        "$resolved_cargo_path" "$effective_cargo_real" \
        >> "$runner_dir/toolchain/cargo/identity.txt"
    "$sha256sum_path" -- "$effective_cargo_real" | awk '{print $1}' \
        >> "$runner_dir/toolchain/cargo/identity.txt"
fi
if [[ -n "$resolved_rustc_path" && -f "$resolved_rustc_path" ]]; then
    effective_rustc_real="$(realpath -- "$resolved_rustc_path")"
    printf 'effective_path=%s\neffective_realpath=%s\neffective_sha256=' \
        "$resolved_rustc_path" "$effective_rustc_real" \
        >> "$runner_dir/toolchain/rustc/identity.txt"
    "$sha256sum_path" -- "$effective_rustc_real" | awk '{print $1}' \
        >> "$runner_dir/toolchain/rustc/identity.txt"
fi

{
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
    printf 'MIMI_REQUIRE_VALGRIND=%s\n' "$MIMI_REQUIRE_VALGRIND"
    printf 'MIMI_L3_VALGRIND=%s\n' "$MIMI_L3_VALGRIND"
    printf 'MIMI_ARGS_STRING_INDEX_ARTIFACT_DIR=%s\n' "$MIMI_ARGS_STRING_INDEX_ARTIFACT_DIR"
    printf 'CARGO_BUILD_JOBS=1\nCARGO_TARGET_DIR=%s\n' "$target_dir"
    printf 'RUSTC=%s\nRUSTC_WRAPPER=unset\nRUSTC_WORKSPACE_WRAPPER=unset\n' "$rustc_path"
    printf 'RUSTUP_TOOLCHAIN=%s\n' "${RUSTUP_TOOLCHAIN:-<unset>}"
    for tool in rustc cargo rustup rustup-cargo rustup-rustc valgrind sha256sum realpath git; do
        printf '\n[%s]\n' "$tool"
        cat "$runner_dir/toolchain/$tool/identity.txt"
        cat "$runner_dir/toolchain/$tool/stdout.bin"
    done
} > "$runner_dir/toolchain-and-environment.txt" 2> "$runner_dir/toolchain-and-environment.stderr"

cargo_arguments=(
    test --manifest-path "$ROOT_DIR/Cargo.toml"
    --test "$TEST_TARGET" "$TEST_NAME" --
    --exact --ignored --nocapture --test-threads=1
)
{
    printf 'LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=%q RUSTC=%q ' \
        "$LLVM_SYS_181_PREFIX" "$target_dir" "$rustc_path"
    printf 'RUSTC_WRAPPER=unset RUSTC_WORKSPACE_WRAPPER=unset '
    printf 'MIMI_REQUIRE_VALGRIND=1 MIMI_L3_VALGRIND=%q %s=%q %q ' \
        "$MIMI_L3_VALGRIND" "$TEST_ARTIFACT_ENV" "$test_artifact_dir" "$cargo_path"
    printf '%q ' "${cargo_arguments[@]}"
    printf '\n'
} > "$runner_dir/test.command.txt"
printf '%s\0' "$cargo_path" "${cargo_arguments[@]}" > "$runner_dir/test.argv.nul"
{
    printf 'LLVM_SYS_181_PREFIX=%s\nCARGO_BUILD_JOBS=1\nCARGO_TARGET_DIR=%s\n' \
        "$LLVM_SYS_181_PREFIX" "$target_dir"
    printf 'RUSTC=%s\nRUSTC_WRAPPER=unset\nRUSTC_WORKSPACE_WRAPPER=unset\n' "$rustc_path"
    printf 'RUSTUP_TOOLCHAIN=%s\n' "${RUSTUP_TOOLCHAIN:-<unset>}"
    printf 'MIMI_REQUIRE_VALGRIND=%s\nMIMI_L3_VALGRIND=%s\n%s=%s\n' \
        "$MIMI_REQUIRE_VALGRIND" "$MIMI_L3_VALGRIND" \
        "$TEST_ARTIFACT_ENV" "$MIMI_ARGS_STRING_INDEX_ARTIFACT_DIR"
} > "$runner_dir/test.environment.txt"

cargo_status=0
if [[ "$toolchain_status" -ne 0 ]]; then
    cargo_status="$toolchain_status"
    : > "$runner_dir/test.stdout"
    printf 'toolchain identity probe failed; see toolchain captures\n' > "$runner_dir/test.stderr"
else
    if env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
        -u CARGO_BUILD_RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER \
        LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        CARGO_BUILD_JOBS=1 \
        CARGO_TARGET_DIR="$target_dir" \
        RUSTC="$rustc_path" \
        MIMI_REQUIRE_VALGRIND=1 \
        MIMI_L3_VALGRIND="$valgrind_path" \
        MIMI_ARGS_STRING_INDEX_ARTIFACT_DIR="$test_artifact_dir" \
        "$cargo_path" "${cargo_arguments[@]}" \
        > "$runner_dir/test.stdout" 2> "$runner_dir/test.stderr"; then
        cargo_status=0
    else
        cargo_status=$?
    fi
fi
printf '%s\n' "$cargo_status" > "$runner_dir/test.exit"
{
    printf '===== stdout =====\n'
    cat "$runner_dir/test.stdout"
    printf '\n===== stderr =====\n'
    cat "$runner_dir/test.stderr"
} > "$runner_dir/test.log"

snapshot_after="$(capture_source_snapshot after)"
printf '%s\n' "$snapshot_after" > "$runner_dir/source-snapshot.after.sha256"
"$git_path" -C "$ROOT_DIR" rev-parse HEAD > "$runner_dir/git-head.after.txt"
"$git_path" -C "$ROOT_DIR" branch --show-current > "$runner_dir/git-branch.after.txt"
"$git_path" -C "$ROOT_DIR" status --porcelain=v1 --untracked-files=all \
    > "$runner_dir/git-status.after.txt"

snapshot_status=0
if ! cmp -s "$runner_dir/source-before.inputs.nul" "$runner_dir/source-after.inputs.nul" \
    || ! cmp -s "$runner_dir/source-before.sha256sums" "$runner_dir/source-after.sha256sums" \
    || ! cmp -s "$runner_dir/git-head.before.txt" "$runner_dir/git-head.after.txt" \
    || ! cmp -s "$runner_dir/git-branch.before.txt" "$runner_dir/git-branch.after.txt"; then
    snapshot_status=98
    printf 'source inputs, Git HEAD, or branch changed during the strict L3 run\n' \
        > "$runner_dir/source-snapshot.error.txt"
fi

validation_status=0
validation_log="$runner_dir/validation.log"
: > "$validation_log"
validation_error() {
    printf '%s\n' "$1" | tee -a "$validation_log" >&2
    validation_status=1
}

validate_exact_cargo_result() {
    local combined="$runner_dir/test.log" test_line_count result_line_count
    grep -Fxq 'running 1 test' "$combined" \
        || validation_error 'Cargo output did not select exactly one ignored test'
    grep -Fxq "test $TEST_NAME ... ok" "$combined" \
        || validation_error 'Cargo output did not report the requested exact test as passed'
    test_line_count="$(grep -Ec '^test [[:alnum:]_:]+ \.\.\. (ok|FAILED|ignored)$' "$combined" || true)"
    [[ "$test_line_count" == 1 ]] \
        || validation_error "Cargo output reported $test_line_count executed test cases instead of one"
    result_line_count="$(grep -Ec '^test result: ok\. 1 passed; 0 failed; 0 ignored; [0-9]+ measured; [0-9]+ filtered out; finished in [0-9.]+s$' "$combined" || true)"
    [[ "$result_line_count" == 1 ]] \
        || validation_error 'Cargo output lacked the exact one-passed, zero-failed test summary'
    [[ "$cargo_status" == 0 ]] \
        || validation_error "Cargo test returned exit code $cargo_status"
}

expected_test_files=(
    valgrind-version.stdout
    valgrind-version.stderr
    tool-paths.txt
    expected-output.bin
    test-input.txt
)
for case_id in \
    args-list-only-control \
    index-outlives-parent-list-return \
    index-copy-cleanup-preserves-returned-list; do
    expected_test_files+=(
        "$case_id.mimi"
        "$case_id-native"
        "$case_id.build.stdout"
        "$case_id.build.stderr"
        "$case_id.route-receipt.txt"
        "$case_id.memcheck.log"
        "$case_id.memcheck.stdout"
        "$case_id.memcheck.stderr"
        "$case_id.memcheck-summary.txt"
    )
done

validate_test_evidence() {
    local file_count expected_count relative nonfile_entry
    if [[ ! -d "$test_artifact_dir" || -L "$test_artifact_dir" ]]; then
        validation_error "test did not create a regular evidence directory at $test_artifact_dir"
        return
    fi
    local canonical_test_dir
    canonical_test_dir="$(realpath -- "$test_artifact_dir")"
    [[ "$canonical_test_dir" == "$test_artifact_dir" ]] \
        || validation_error 'test evidence directory resolved through a noncanonical path'
    for relative in "${expected_test_files[@]}"; do
        if [[ ! -f "$test_artifact_dir/$relative" || -L "$test_artifact_dir/$relative" ]]; then
            validation_error "test evidence is missing or not a regular file: $relative"
        fi
    done
    file_count="$(find "$test_artifact_dir" -mindepth 1 -maxdepth 1 -type f -printf . | wc -c)"
    expected_count="${#expected_test_files[@]}"
    [[ "$file_count" == "$expected_count" ]] \
        || validation_error "test evidence inventory has $file_count files; expected exactly $expected_count"
    nonfile_entry="$(find "$test_artifact_dir" -mindepth 1 -maxdepth 1 ! -type f -print -quit)"
    [[ -z "$nonfile_entry" ]] \
        || validation_error "test evidence inventory contains an unexpected directory or non-file: $nonfile_entry"

    if [[ -f "$test_artifact_dir/expected-output.bin" ]]; then
        printf '%b' '\xE9\x9B\xAA\xE3\x81\xAE\xE4\xB8\x96\xE7\x95\x8C\xF0\x9F\xA7\xAA\n' \
            > "$runner_dir/expected-output.reference.bin"
        cmp -s "$runner_dir/expected-output.reference.bin" "$test_artifact_dir/expected-output.bin" \
            || validation_error 'test evidence expected-output.bin did not match the fixed Unicode output'
    fi
    if [[ -f "$test_artifact_dir/test-input.txt" ]]; then
        printf '%b' 'argument=\xE9\x9B\xAA\xE3\x81\xAE\xE4\xB8\x96\xE7\x95\x8C\xF0\x9F\xA7\xAA\n' \
            > "$runner_dir/test-input.reference.txt"
        cmp -s "$runner_dir/test-input.reference.txt" "$test_artifact_dir/test-input.txt" \
            || validation_error 'test evidence input did not match the fixed Unicode argument'
    fi
    if [[ -f "$test_artifact_dir/tool-paths.txt" ]]; then
        printf 'valgrind=%s\n' "$valgrind_path" > "$runner_dir/tool-paths.reference.txt"
        cmp -s "$runner_dir/tool-paths.reference.txt" "$test_artifact_dir/tool-paths.txt" \
            || validation_error 'test evidence Valgrind path did not match the runner-selected executable'
    fi
    if [[ -f "$test_artifact_dir/valgrind-version.stdout" \
        && -f "$runner_dir/toolchain/valgrind/stdout.bin" ]]; then
        cmp -s "$runner_dir/toolchain/valgrind/stdout.bin" \
            "$test_artifact_dir/valgrind-version.stdout" \
            || validation_error 'test Valgrind version output differed from the runner probe'
    fi
    if [[ -f "$test_artifact_dir/valgrind-version.stderr" \
        && -f "$runner_dir/toolchain/valgrind/stderr.bin" ]]; then
        cmp -s "$runner_dir/toolchain/valgrind/stderr.bin" \
            "$test_artifact_dir/valgrind-version.stderr" \
            || validation_error 'test Valgrind version diagnostics differed from the runner probe'
    fi

    for case_id in \
        args-list-only-control \
        index-outlives-parent-list-return \
        index-copy-cleanup-preserves-returned-list; do
        local receipt="$test_artifact_dir/$case_id.route-receipt.txt"
        local build_stderr="$test_artifact_dir/$case_id.build.stderr"
        local route_line route_count field value
        [[ -f "$test_artifact_dir/$case_id-native" && -x "$test_artifact_dir/$case_id-native" ]] \
            || validation_error "$case_id native binary is missing or not executable"
        [[ -f "$test_artifact_dir/$case_id.mimi" ]] \
            || validation_error "$case_id source was not retained"
        [[ -f "$test_artifact_dir/$case_id.memcheck.stdout" ]] \
            || continue
        if [[ "$case_id" == args-list-only-control ]]; then
            [[ ! -s "$test_artifact_dir/$case_id.memcheck.stdout" ]] \
                || validation_error 'args-list-only control unexpectedly printed to stdout'
        elif [[ -f "$test_artifact_dir/expected-output.bin" ]]; then
            cmp -s "$test_artifact_dir/expected-output.bin" \
                "$test_artifact_dir/$case_id.memcheck.stdout" \
                || validation_error "$case_id Memcheck stdout differed from the expected Unicode output"
        fi

        [[ -f "$build_stderr" ]] || continue
        route_count="$(grep -Ec '^MIMI_ROUTE_CENSUS canonical-native-build ' "$build_stderr" || true)"
        [[ "$route_count" == 1 ]] \
            || validation_error "$case_id native build did not emit exactly one canonical route receipt"
        if ! grep -Eq '^info: resolved emitter compiled .*, 0 fell back to legacy$' "$build_stderr"; then
            validation_error "$case_id native build did not prove zero legacy callable fallback"
        fi
        route_line="$(grep -m 1 '^MIMI_ROUTE_CENSUS canonical-native-build ' "$build_stderr" || true)"
        for field in consumer route_id profile mir_digest route_identity_digest; do
            value="$(sed -nE "s/.*[[:space:]]${field}=([^[:space:]]+).*/\\1/p" <<< "$route_line")"
            [[ -n "$value" ]] || validation_error "$case_id route output omitted $field"
            case "$field" in
                consumer) [[ "$value" == default ]] || validation_error "$case_id route consumer was $value" ;;
                route_id|profile) [[ "$value" == build-canonical-v1 ]] || validation_error "$case_id route profile was $value" ;;
                mir_digest|route_identity_digest)
                    [[ "$value" =~ ^[0-9a-f]{64}$ ]] || validation_error "$case_id $field is not a lowercase SHA-256 digest" ;;
            esac
            if [[ -f "$receipt" ]]; then
                grep -Fxq "$field=$value" "$receipt" \
                    || validation_error "$case_id route receipt did not bind $field to native build output"
            fi
        done
        if [[ -f "$receipt" ]]; then
            if ! awk -F= 'NF != 2 || $1 == "" || $2 == "" || seen[$1]++ { bad=1 } END { exit bad }' "$receipt"; then
                validation_error "$case_id route receipt had malformed or duplicate fields"
            fi
            while IFS='=' read -r field value; do
                [[ -n "$field" && -n "$value" ]] || continue
                [[ "$route_line" == *" $field=$value"* ]] \
                    || validation_error "$case_id route receipt field $field was not present in build output"
            done < "$receipt"
        fi

        local summary="$test_artifact_dir/$case_id.memcheck-summary.txt"
        if [[ -f "$summary" ]]; then
            for field in 'definitely lost' 'indirectly lost' 'possibly lost'; do
                grep -Eq "^(==[0-9]+==[[:space:]]*)?${field}: 0 bytes in 0 blocks( \(inferred from all-free summary\))?$" "$summary" \
                    || validation_error "$case_id Memcheck summary lacked a zero $field result"
            done
            grep -Eq '^(==[0-9]+==[[:space:]]*)?still reachable: 0 bytes in 0 blocks$' "$summary" \
                || validation_error "$case_id Memcheck summary retained reachable allocations"
            grep -Eq '^(==[0-9]+==[[:space:]]*)?ERROR SUMMARY: 0 errors from 0 contexts \(suppressed: 0 from 0\)$' "$summary" \
                || validation_error "$case_id Memcheck summary lacked a zero error and suppression result"
        fi
    done

    if [[ "$file_count" == "$expected_count" ]]; then
        : > "$runner_dir/test-evidence.sha256sums"
        for relative in "${expected_test_files[@]}"; do
            "$sha256sum_path" -- "$test_artifact_dir/$relative" \
                >> "$runner_dir/test-evidence.sha256sums"
        done
    fi
}

if [[ "$cargo_status" -eq 0 ]]; then
    validate_exact_cargo_result
    validate_test_evidence
else
    validation_error "strict L3 test did not complete successfully (cargo exit $cargo_status)"
fi

snapshot_status="${snapshot_status:-0}"
final_status=0
if [[ "$cargo_status" -ne 0 ]]; then
    final_status="$cargo_status"
elif [[ "$snapshot_status" -ne 0 ]]; then
    final_status="$snapshot_status"
elif [[ "$validation_status" -ne 0 ]]; then
    final_status=1
fi
printf 'cargo_exit=%s\nsource_snapshot_exit=%s\nvalidation_exit=%s\nrunner_exit=%s\n' \
    "$cargo_status" "$snapshot_status" "$validation_status" "$final_status" \
    > "$runner_dir/runner.status"
printf 'source_snapshot_before=%s\nsource_snapshot_after=%s\n' \
    "$snapshot_before" "$snapshot_after" > "$runner_dir/source-snapshot.status"
printf 'runner_evidence=%s\ntest_evidence=%s\nrunner_exit=%s\n' \
    "$runner_dir" "$test_artifact_dir" "$final_status"
exit "$final_status"
