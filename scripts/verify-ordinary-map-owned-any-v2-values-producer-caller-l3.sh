#!/usr/bin/env bash
set -euo pipefail
set -o noclobber

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
ARTIFACT_ENV=MIMI_VALUES_PRODUCER_CALLER_L3_ARTIFACT_DIR
ARTIFACT_DIR_DEFAULT="$ROOT_DIR/evidence/0.1.11-dev/gates/values-crosscall-current-20261026-l3"
ARTIFACT_DIR_REQUESTED="${MIMI_VALUES_PRODUCER_CALLER_L3_EVIDENCE_DIR:-$ARTIFACT_DIR_DEFAULT}"
case "$ARTIFACT_DIR_REQUESTED" in
    /*) ;;
    *) ARTIFACT_DIR_REQUESTED="$ROOT_DIR/$ARTIFACT_DIR_REQUESTED" ;;
esac
ARTIFACT_DIR="$(realpath -m -- "$ARTIFACT_DIR_REQUESTED")"
TARGET_DIR_REQUESTED="${MIMI_VALUES_CROSSCALL_L3_TARGET_DIR:-${TMPDIR:-/tmp}/mimi-target-cache-values-crosscall-current-20261026-l3}"
TEST_TARGET=canonical_mir_ordinary_map_owned_any_v2_values_producer_caller_memcheck_l3
TEST_NAME=ordinary_map_owned_any_v2_values_producer_caller_default_and_explicit_native_builds_are_strict_memcheck_clean

for tool in cargo rustc valgrind sha256sum realpath git find sort cmp grep awk; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'Values producer/caller strict L3 requires %s\n' "$tool" >&2
        exit 1
    }
done

if [[ ! -d "$LLVM_SYS_181_PREFIX" ]]; then
    printf 'repository LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi
LLVM_SYS_181_PREFIX="$(realpath -- "$LLVM_SYS_181_PREFIX")"
case "$ARTIFACT_DIR/" in
    "$ROOT_DIR/evidence/"*) ;;
    *)
        printf 'Values producer/caller evidence must remain under the repository evidence tree: %s\n' "$ARTIFACT_DIR" >&2
        exit 2
        ;;
esac
TARGET_DIR="$(realpath -m -- "$TARGET_DIR_REQUESTED")"
case "$TARGET_DIR/" in
    "$ROOT_DIR/"*)
        printf 'Cargo target cache must be external to the repository: %s\n' "$TARGET_DIR" >&2
        exit 2
        ;;
esac

if [[ -e "$ARTIFACT_DIR" || -L "$ARTIFACT_DIR" ]]; then
    printf 'refusing to overwrite Values producer/caller L3 evidence: %s\n' "$ARTIFACT_DIR" >&2
    exit 2
fi
mkdir -p -- "$(dirname "$ARTIFACT_DIR")"
mkdir -- "$ARTIFACT_DIR"
ARTIFACT_DIR="$(realpath -- "$ARTIFACT_DIR")"
export "$ARTIFACT_ENV=$ARTIFACT_DIR"

source_snapshot_manifest() {
    (
        cd "$ROOT_DIR"
        {
            for source_tree in src std stdlib tests scripts; do
                if [[ -d "$source_tree" ]]; then
                    find "$source_tree" -type f -print0
                fi
            done
            if [[ -d .cargo ]]; then
                find .cargo -type f -print0
            fi
            find . -maxdepth 1 -type f \
                \( -name 'Cargo*' -o -name '.gitignore' -o -name 'build.rs' \) -print0
        } | LC_ALL=C sort -zu | while IFS= read -r -d '' source_path; do
            sha256sum -- "$source_path"
        done
    )
}

record_tool() {
    local label="$1" executable="$2" status=0 directory="$ARTIFACT_DIR/toolchain/$1"
    shift 2
    mkdir -- "$directory"
    {
        printf '%q ' "$executable" "$@"
        printf '\n'
    } > "$directory/command.txt"
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

source_snapshot_manifest > "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256"
sha256sum -- "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256" \
    | awk '{print $1}' > "$ARTIFACT_DIR/source-snapshot.before.sha256"
mkdir -p -- "$ARTIFACT_DIR/toolchain"

version_status=0
for tool in bash cargo rustc valgrind sha256sum realpath git; do
    executable="$(type -P -- "$tool")"
    case "$tool" in
        bash) version_arguments=(--version) ;;
        cargo|rustc|valgrind) version_arguments=(--version) ;;
        sha256sum|realpath|git) version_arguments=(--version) ;;
    esac
    if ! record_tool "$tool" "$executable" "${version_arguments[@]}"; then
        version_status=1
    fi
done

llvm_config="$LLVM_SYS_181_PREFIX/bin/llvm-config"
if [[ -x "$llvm_config" ]]; then
    if ! record_tool llvm-config "$llvm_config" --version; then
        version_status=1
    fi
else
    printf 'llvm_config=not-present-in-wrapper\n' > "$ARTIFACT_DIR/toolchain/llvm-config.txt"
fi

{
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
    printf 'CARGO_TARGET_DIR=%s\n' "$TARGET_DIR"
    printf 'external_target_cache=true\n'
    printf 'runner_script=%s\n' "$ROOT_DIR/scripts/verify-ordinary-map-owned-any-v2-values-producer-caller-l3.sh"
    printf 'evidence_directory=%s\n' "$ARTIFACT_DIR"
    printf 'test_target=%s\n' "$TEST_TARGET"
    printf 'test_name=%s\n' "$TEST_NAME"
    printf 'git_head='; git rev-parse HEAD
    printf 'git_branch='; git branch --show-current
    for tool in bash cargo rustc valgrind sha256sum realpath git; do
        printf '%s_path=%s\n' "$tool" "$(type -P -- "$tool")"
        printf '%s_version=' "$tool"
        cat "$ARTIFACT_DIR/toolchain/$tool/stdout.bin"
    done
    if [[ -x "$llvm_config" ]]; then
        printf 'llvm_config_path=%s\nllvm_config_version=' "$llvm_config"
        cat "$ARTIFACT_DIR/toolchain/llvm-config/stdout.bin"
    fi
} > "$ARTIFACT_DIR/toolchain.txt" 2> "$ARTIFACT_DIR/toolchain.stderr"

cargo_arguments=(
    test
    --manifest-path "$ROOT_DIR/Cargo.toml"
    --target-dir "$TARGET_DIR"
    --test "$TEST_TARGET"
    "$TEST_NAME"
    --
    --exact
    --ignored
    --nocapture
    --test-threads=1
)
{
    printf 'LLVM_SYS_181_PREFIX=%q CARGO_TARGET_DIR=%q CARGO_BUILD_JOBS=1 %s=%q ' \
        "$LLVM_SYS_181_PREFIX" "$TARGET_DIR" "$ARTIFACT_ENV" "$ARTIFACT_DIR"
    printf '%q ' "$(type -P -- cargo)" "${cargo_arguments[@]}"
    printf '\n'
} > "$ARTIFACT_DIR/suite.command.txt"
printf '%s\0' "$(type -P -- cargo)" "${cargo_arguments[@]}" \
    > "$ARTIFACT_DIR/suite.argv.nul"
{
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
    printf 'CARGO_TARGET_DIR=%s\n' "$TARGET_DIR"
    printf 'CARGO_BUILD_JOBS=1\n'
    printf '%s=%s\n' "$ARTIFACT_ENV" "$ARTIFACT_DIR"
    printf 'test_harness_threads=1\n'
} > "$ARTIFACT_DIR/suite.environment.txt"

suite_status=0
if [[ "$version_status" -ne 0 ]]; then
    suite_status=1
    : > "$ARTIFACT_DIR/suite.stdout"
    printf 'tool version capture failed; see toolchain/\n' > "$ARTIFACT_DIR/suite.stderr"
else
    if env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        CARGO_TARGET_DIR="$TARGET_DIR" CARGO_BUILD_JOBS=1 \
        "$ARTIFACT_ENV=$ARTIFACT_DIR" cargo "${cargo_arguments[@]}" \
        > "$ARTIFACT_DIR/suite.stdout" 2> "$ARTIFACT_DIR/suite.stderr"; then
        suite_status=0
    else
        suite_status=$?
    fi
fi
printf 'exit_code=%s\nsuccess=%s\n' "$suite_status" \
    "$([[ "$suite_status" -eq 0 ]] && printf true || printf false)" \
    > "$ARTIFACT_DIR/suite.process.status"

snapshot_status=0
if source_snapshot_manifest > "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256"; then
    sha256sum -- "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256" \
        | awk '{print $1}' > "$ARTIFACT_DIR/source-snapshot.after.sha256"
    if cmp -s "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256" \
        "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256"; then
        printf 'stable=true\n' > "$ARTIFACT_DIR/source-snapshot.status"
    else
        printf 'stable=false\n' > "$ARTIFACT_DIR/source-snapshot.status"
        snapshot_status=1
    fi
else
    printf 'stable=false\nmanifest_capture_failed=true\n' > "$ARTIFACT_DIR/source-snapshot.status"
    snapshot_status=1
fi

ignore_status=0
: > "$ARTIFACT_DIR/binary-ignore-check.txt"
while IFS= read -r -d '' binary; do
    relative_binary="${binary#"$ROOT_DIR"/}"
    if git check-ignore -v -- "$relative_binary" >> "$ARTIFACT_DIR/binary-ignore-check.txt"; then
        :
    else
        printf 'not ignored: %s\n' "$relative_binary" >> "$ARTIFACT_DIR/binary-ignore-check.txt"
        ignore_status=1
    fi
done < <(find "$ARTIFACT_DIR/native" -type f -name program -print0 2>/dev/null || true)

validation_status=0
if [[ "$suite_status" -eq 0 ]]; then
    summary="$ARTIFACT_DIR/test-summary.txt"
    for expected in \
        "route_profile=ordinary-map-owned-any-v2-values-result-producer-caller-v1-one-length-return" \
        'fixture_count=1' 'native_build_count=2' 'strict_memcheck_count=2' 'case_count=2' \
        'fixture_source_sha256=79a48cbba5e154452f3f6bfb1ca5dde41b86b89689df350f2dad17169753519e' \
        'result-one-len_expected_stdout_hex=310a' \
        'result-one-len_default_explicit_identity_equal=true'; do
        if [[ ! -f "$summary" ]] || ! grep -Fxq "$expected" "$summary"; then
            printf 'L3 summary is missing %s\n' "$expected" >&2
            validation_status=1
        fi
    done
    [[ "$(find "$ARTIFACT_DIR/native" -type f -name program | wc -l | tr -d '[:space:]')" == 2 ]] || validation_status=1
    [[ "$(find "$ARTIFACT_DIR/native" -type f -name valgrind.log | wc -l | tr -d '[:space:]')" == 2 ]] || validation_status=1
    [[ "$(find "$ARTIFACT_DIR/native" -type f -name memcheck-summary.txt | wc -l | tr -d '[:space:]')" == 2 ]] || validation_status=1
    [[ "$(find "$ARTIFACT_DIR/native" -type f -name program.sha256 | wc -l | tr -d '[:space:]')" == 2 ]] || validation_status=1
    [[ "$(find "$ARTIFACT_DIR/native" -type f -path '*/memcheck/valgrind-command/command.argv.json' | wc -l | tr -d '[:space:]')" == 2 ]] || validation_status=1

    for case_name in result-one-len; do
        for mode in default explicit-mir; do
            mode_dir="$ARTIFACT_DIR/native/$case_name/$mode"
            if [[ ! -x "$mode_dir/program" ]]; then
                printf 'missing executable native artifact: %s\n' "$mode_dir/program" >&2
                validation_status=1
                continue
            fi
            expected_stdout=$'1\n'
            printf '%s' "$expected_stdout" | cmp -s - "$mode_dir/memcheck/valgrind-command/stdout.bin" || validation_status=1
            for expected in \
                'definitely lost: 0 bytes in 0 blocks' \
                'indirectly lost: 0 bytes in 0 blocks' \
                'possibly lost: 0 bytes in 0 blocks' \
                'ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)'; do
                if [[ ! -f "$mode_dir/memcheck/memcheck-summary.txt" ]] \
                    || ! grep -Fq "$expected" "$mode_dir/memcheck/memcheck-summary.txt"; then
                    printf '%s Memcheck summary is missing %s\n' "$mode_dir" "$expected" >&2
                    validation_status=1
                fi
            done
        done
    done
fi

final_status=0
if [[ "$suite_status" -ne 0 || "$snapshot_status" -ne 0 || "$ignore_status" -ne 0 || "$validation_status" -ne 0 ]]; then
    final_status=1
fi
printf 'exit_code=%s\nsuccess=%s\n' "$final_status" \
    "$([[ "$final_status" -eq 0 ]] && printf true || printf false)" \
    > "$ARTIFACT_DIR/runner.process.status"
if [[ "$final_status" -eq 0 ]]; then
    printf 'Values producer/caller strict L3 evidence captured at %s\n' "$ARTIFACT_DIR"
else
    printf 'Values producer/caller strict L3 gate failed; inspect %s\n' "$ARTIFACT_DIR" >&2
fi
exit "$final_status"
