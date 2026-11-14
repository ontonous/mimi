#!/usr/bin/env bash
set -Eeuo pipefail
set -o noclobber

ROOT_DIR="$(cd -- "$(dirname -- "$0")/.." && pwd -P)"
PROFILE=ordinary-map-owned-any-v2-get-or-default-string-v1
TEST_TARGET=canonical_mir_ordinary_map_owned_any_v2_get_or_default_string
TEST_NAME=get_or_default_string_native_default_and_explicit_mir_are_memcheck_clean
ARTIFACT_ENV=MIMI_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_L3_ARTIFACT_DIR
TARGET_ENV=MIMI_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_L3_TARGET_DIR
TMPDIR_ENV=MIMI_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_L3_TMPDIR
LOG_ENV=MIMI_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_L3_LOG
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"

if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper
fi
if [[ -v "$ARTIFACT_ENV" && -n "${!ARTIFACT_ENV}" ]]; then
    requested="${!ARTIFACT_ENV}"
else
    requested="${TMPDIR:-/tmp}/mimi-map-owned-any-v2-get-or-default-string-l3-$run_id"
fi
case "$requested" in /*) ;; *) requested="$ROOT_DIR/$requested" ;; esac
if [[ -e "$requested" || -L "$requested" ]]; then
    printf 'refusing to overwrite String GetOrDefault L3 artifact path: %s\n' "$requested" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    "$ROOT_DIR/"*)
        printf 'String GetOrDefault L3 artifacts must be outside the repository: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname -- "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
export MIMI_GET_OR_DEFAULT_STRING_L3_ARTIFACT_DIR="$artifact_dir"

failed=0
error_count=0
snapshot_before_ok=false
snapshot_after_ok=false
source_tree_identical=false
manifest_digests_verified=false
valgrind_path=unavailable
: > "$artifact_dir/validation-errors.txt"

record_error() {
    failed=1
    error_count=$((error_count + 1))
    printf 'reason=%s\n' "$1" >> "$artifact_dir/validation-errors.txt"
    printf 'validation_error=%s\n' "$1" >&2
}

build_root="${MIMI_GET_OR_DEFAULT_STRING_BUILD_ROOT:-${TMPDIR:-/tmp}}"
if [[ -v "$TARGET_ENV" && -n "${!TARGET_ENV}" ]]; then
    target_requested="${!TARGET_ENV}"
else
    target_requested="$build_root/mimi-map-owned-any-v2-get-or-default-string-target-$run_id"
fi
if [[ -v "$TMPDIR_ENV" && -n "${!TMPDIR_ENV}" ]]; then
    temporary_requested="${!TMPDIR_ENV}"
else
    temporary_requested="$build_root/mimi-map-owned-any-v2-get-or-default-string-tmp-$run_id"
fi
if [[ -v "$LOG_ENV" && -n "${!LOG_ENV}" ]]; then
    gate_log_requested="${!LOG_ENV}"
else
    gate_log_requested="$artifact_dir/gate.log"
fi
target_dir="$(realpath -m -- "$target_requested")"
temporary_dir="$(realpath -m -- "$temporary_requested")"
gate_log="$(realpath -m -- "$gate_log_requested")"

for path_label in target_dir temporary_dir gate_log; do
    case "${!path_label}/" in
        "$ROOT_DIR/"*) record_error "$path_label must be outside the repository: ${!path_label}" ;;
    esac
done
case "$target_dir/" in
    "$artifact_dir/"*) record_error "CARGO_TARGET_DIR must be outside the artifact directory: $target_dir" ;;
esac
case "$artifact_dir/" in
    "$target_dir/"*) record_error "artifact directory must be outside CARGO_TARGET_DIR: $artifact_dir" ;;
esac
case "$temporary_dir/" in
    "$artifact_dir/"*) record_error "TMPDIR must be outside the evidence directory: $temporary_dir" ;;
esac
case "$artifact_dir/" in
    "$temporary_dir/"*) record_error "evidence directory must be outside TMPDIR: $artifact_dir" ;;
esac
case "$gate_log/" in
    "$target_dir/"*) record_error "gate log must be outside CARGO_TARGET_DIR: $gate_log" ;;
esac
case "$target_dir/" in
    "$gate_log/"*) record_error "CARGO_TARGET_DIR must be outside the gate log path: $target_dir" ;;
esac
for pair in "$target_dir:$temporary_dir" "$temporary_dir:$target_dir"; do
    left="${pair%%:*}"
    right="${pair#*:}"
    case "$right/" in "$left/"*) record_error 'CARGO_TARGET_DIR and TMPDIR must be separate sibling paths' ;; esac
done
for path in "$target_dir" "$temporary_dir" "$gate_log"; do
    if [[ -e "$path" || -L "$path" ]]; then
        record_error "refusing to reuse an existing external build/log path: $path"
    fi
done
snapshot_sources() {
    local label="$1"
    local paths="$artifact_dir/source-snapshot.$label.paths.nul"
    local text_paths="$artifact_dir/source-snapshot.$label.paths.txt"
    local manifest="$artifact_dir/source-snapshot.$label.manifest.sha256"
    local path required status=0 ignore_status
    if ! (
        cd "$ROOT_DIR"
        for directory in src tests std; do
            [[ -d "$directory" ]] || exit 1
            find "$directory" -type f -print0
        done
        printf '%s\0' Cargo.toml \
            .gitignore \
            scripts/verify-ordinary-map-owned-any-v2-get-or-default-string-l1l2.sh \
            scripts/verify-ordinary-map-owned-any-v2-get-or-default-string-l3.sh
        for optional in Cargo.lock build.rs rust-toolchain rust-toolchain.toml .cargo/config .cargo/config.toml; do
            [[ ! -f "$optional" ]] || printf '%s\0' "$optional"
        done
    ) | LC_ALL=C sort -zu > "$paths"; then
        record_error "could not enumerate source snapshot $label"
        return 1
    fi
    if ! tr '\0' '\n' < "$paths" > "$text_paths"; then
        record_error "could not render source path list $label"
        return 1
    fi
    local -a required_sources=(
        .gitignore
        Cargo.toml
        std/maps.mimi
        tests/support/memcheck_summary.rs
        tests/canonical_mir_ordinary_map_owned_any_v2_get_or_default_string.rs
        scripts/verify-ordinary-map-owned-any-v2-get-or-default-string-l1l2.sh
        scripts/verify-ordinary-map-owned-any-v2-get-or-default-string-l3.sh
    )
    local -a profile_fixtures=()
    mapfile -t profile_fixtures < <(
        find "$ROOT_DIR/tests/fixtures" -maxdepth 1 -type f \
            -name 'mir_ordinary_map_owned_any_v2_get_or_default_string_*.mimi' -print \
            | LC_ALL=C sort
    )
    if [[ "${#profile_fixtures[@]}" -ne 25 ]]; then
        printf 'expected 25 GetOrDefault<String> fixtures, found %s\n' \
            "${#profile_fixtures[@]}" >> "$artifact_dir/source-snapshot.$label.errors.txt"
        status=1
    fi
    for path in "${profile_fixtures[@]}"; do
        required_sources+=("${path#"$ROOT_DIR"/}")
    done
    for required in "${required_sources[@]}"; do
        if [[ ! -f "$ROOT_DIR/$required" ]]; then
            printf 'required source input is missing: %s\n' "$required" \
                >> "$artifact_dir/source-snapshot.$label.errors.txt"
            status=1
        fi
        if git -C "$ROOT_DIR" check-ignore -q -- "$required"; then
            printf 'required source input is ignored: %s\n' "$required" \
                >> "$artifact_dir/source-snapshot.$label.errors.txt"
            status=1
        else
            ignore_status=$?
            if [[ "$ignore_status" -ne 1 ]]; then
                printf 'could not check ignore status for required source input: %s (exit %s)\n' \
                    "$required" "$ignore_status" \
                    >> "$artifact_dir/source-snapshot.$label.errors.txt"
                status=1
            fi
        fi
        if ! grep -F -x -q -- "$required" "$text_paths"; then
            printf 'required source input is absent from the source snapshot: %s\n' "$required" \
                >> "$artifact_dir/source-snapshot.$label.errors.txt"
            status=1
        fi
    done
    : > "$manifest"
    while IFS= read -r -d '' path; do
        if [[ ! -f "$ROOT_DIR/$path" ]]; then
            printf 'source_disappeared=%s\n' "$path" >> "$artifact_dir/source-snapshot.$label.errors.txt"
            status=1
        elif ! sha256sum -- "$ROOT_DIR/$path" >> "$manifest"; then
            printf 'source_hash_failed=%s\n' "$path" >> "$artifact_dir/source-snapshot.$label.errors.txt"
            status=1
        fi
    done < "$paths"
    return "$status"
}

run_stage() {
    local name="$1" expected="$2" status=0
    shift 2
    local directory="$artifact_dir/$name"
    mkdir -p -- "$directory"
    {
        printf 'expected_exit_code=%s\n' "$expected"
        printf 'cwd=%q\n' "$ROOT_DIR"
        printf '%q ' "$@"
        printf '\n'
    } > "$directory/command.txt"
    printf '%s\0' "$@" > "$directory/command.argv.nul"
    if (cd "$ROOT_DIR" && "$@") > "$directory/stdout" 2> "$directory/stderr"; then
        status=0
    else
        status=$?
    fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$directory/process.status"
    {
        printf 'stage=%s exit_code=%s expected_exit_code=%s\n' "$name" "$status" "$expected"
        printf '%s\n' '--- stdout ---'
        cat "$directory/stdout"
        printf '%s\n' '--- stderr ---'
        cat "$directory/stderr"
    } >> "$gate_log"
    if [[ "$status" -ne "$expected" ]]; then
        record_error "$name returned $status; expected $expected"
    fi
    return 0
}

validate_memcheck_outputs() {
    local fixture mode run_root summary raw_summary raw_report
    local -a fixture_names=(hit empty-hit miss reachable-helper)
    local -a modes=(default explicit-mir)
    local count=0
    : > "$artifact_dir/memcheck-rollup.txt"
    for fixture in "${fixture_names[@]}"; do
        for mode in "${modes[@]}"; do
            run_root="$artifact_dir/native/$fixture/$mode"
            summary="$run_root/memcheck-summary.txt"
            raw_summary="$run_root/memcheck-summary.raw.txt"
            raw_report="$run_root/valgrind.log"
            if [[ ! -x "$run_root/program" ]]; then
                record_error "native executable is missing: $run_root/program"
            fi
            if [[ ! -s "$raw_report" ]]; then
                record_error "raw Valgrind report is missing or empty: $raw_report"
            fi
            if [[ ! -s "$raw_summary" ]]; then
                record_error "raw Memcheck summary excerpt is missing or empty: $raw_summary"
            fi
            if [[ ! -s "$summary" ]]; then
                record_error "parsed Memcheck summary is missing or empty: $summary"
            else
                for kind in 'definitely lost' 'indirectly lost' 'possibly lost'; do
                    if [[ "$(grep -Ec "^[[:space:]]*${kind}:" "$summary" || true)" -ne 1 ]] || \
                        ! grep -Eq "^[[:space:]]*${kind}: 0 bytes in 0 blocks([[:space:]]|$)" "$summary"; then
                        record_error "$fixture/$mode reports nonzero or absent $kind allocations"
                    fi
                done
                if [[ "$(grep -Ec '^[[:space:]]*still reachable:' "$summary" || true)" -ne 1 ]] || \
                    ! grep -Eq '^[[:space:]]*still reachable: [0-9,]+ bytes in [0-9,]+ blocks' "$summary"; then
                    record_error "$fixture/$mode summary omits a single valid still-reachable memory row"
                fi
                if [[ "$(grep -Ec 'ERROR SUMMARY:' "$summary" || true)" -ne 1 ]] || \
                    ! grep -Eq '^[[:space:]]*(==[0-9]+==[[:space:]]+)?ERROR SUMMARY: 0 errors from 0 contexts \(suppressed: 0 from 0\)$' "$summary"; then
                    record_error "$fixture/$mode has a nonzero or malformed Memcheck error summary"
                fi
                {
                    printf 'fixture=%s mode=%s\n' "$fixture" "$mode"
                    grep -E '^[[:space:]]*(definitely lost|indirectly lost|possibly lost|still reachable|ERROR SUMMARY|All heap blocks were freed)' "$summary" || true
                } >> "$artifact_dir/memcheck-rollup.txt"
            fi
            count=$((count + 1))
        done
    done
    printf 'expected_run_count=8\nobserved_run_count=%s\n' "$count" >> "$artifact_dir/memcheck-rollup.txt"
    if [[ "$count" -ne 8 ]]; then record_error "expected 8 native Memcheck runs, observed $count"; fi

    local expected_summary="$artifact_dir/native/expected-run-summary.txt"
    local observed_summary="$artifact_dir/native/run-summary.txt"
    if [[ ! -s "$expected_summary" || ! -s "$observed_summary" ]]; then
        record_error 'expected or observed native run summary is missing'
    elif ! cmp -s -- "$expected_summary" "$observed_summary"; then
        record_error 'native/run-summary.txt differs byte-for-byte from expected-run-summary.txt'
    fi

    local test_stage="$artifact_dir/strict-native-memcheck"
    local test_stdout="$test_stage/stdout"
    local test_name_count=0
    if [[ ! -s "$test_stdout" ]]; then
        record_error 'Cargo test stdout is missing; exact-one-test count is unavailable'
    else
        test_name_count="$(grep -Ec "^test ${TEST_NAME} \.\.\. (ok|FAILED)$" "$test_stdout" || true)"
        if ! grep -F -x -q 'running 1 test' "$test_stdout" || [[ "$test_name_count" -ne 1 ]] || \
            ! grep -F -x -q "test ${TEST_NAME} ... ok" "$test_stdout" || \
            ! grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out' "$test_stdout"; then
            record_error "Cargo L3 invocation did not run exactly one passing ignored test (observed named test rows=$test_name_count)"
        fi
    fi
}

write_report() {
    local validation=FAIL
    if [[ "$failed" -eq 0 && "$manifest_digests_verified" == true && "$source_tree_identical" == true ]]; then
        validation=PASS
    fi
    {
        printf 'validation=%s\nprofile=%s\nlevel=L3\n' "$validation" "$PROFILE"
        printf 'test_target=%s\ntest_name=%s\n' "$TEST_TARGET" "$TEST_NAME"
        printf 'source_tree_identical=%s\nsource_snapshot_before_ok=%s\nsource_snapshot_after_ok=%s\n' \
            "$source_tree_identical" "$snapshot_before_ok" "$snapshot_after_ok"
        printf 'valgrind=%s\nvalgrind_arguments=--leak-check=full --show-leak-kinds=all --errors-for-leak-kinds=all --error-exitcode=97\n' "$valgrind_path"
        printf 'CARGO_TARGET_DIR=%s\nTMPDIR=%s\nlog=%s\n' "$target_dir" "$temporary_dir" "$gate_log"
        printf 'native_artifact_pattern=%s/native/{hit,empty-hit,miss,reachable-helper}/{default,explicit-mir}/program\n' "$artifact_dir"
        printf 'raw_report_pattern=%s/native/<case>/<mode>/valgrind.log\nraw_summary_pattern=%s/native/<case>/<mode>/memcheck-summary.raw.txt\nparsed_summary_pattern=%s/native/<case>/<mode>/memcheck-summary.txt\n' "$artifact_dir" "$artifact_dir" "$artifact_dir"
        printf 'reachable_memory=reported separately in memcheck-rollup.txt\n'
        printf 'failure_count=%s\n' "$error_count"
        cat "$artifact_dir/validation-errors.txt"
    } >| "$artifact_dir/artifact-validation.txt"
}

finalize() {
    local exit_status=$?
    trap - EXIT
    if [[ -n "${artifact_dir:-}" && -d "$artifact_dir" ]]; then
        validate_memcheck_outputs
        if snapshot_sources after; then snapshot_after_ok=true; else snapshot_after_ok=false; fi
        if [[ "$snapshot_before_ok" == true && "$snapshot_after_ok" == true ]] && \
            cmp -s "$artifact_dir/source-snapshot.before.paths.nul" "$artifact_dir/source-snapshot.after.paths.nul" && \
            cmp -s "$artifact_dir/source-snapshot.before.manifest.sha256" "$artifact_dir/source-snapshot.after.manifest.sha256"; then
            source_tree_identical=true
        else
            record_error 'source tree paths or hashes changed during the L3 capture'
        fi
        if [[ "$exit_status" -ne 0 && "$failed" -eq 0 ]]; then
            record_error "script exited before completing all L3 checks (exit=$exit_status)"
        fi
        : > "$artifact_dir/manifest-validation.exit"
        if (
            cd "$artifact_dir"
            find . -type f \
                ! -name artifacts.sha256 \
                ! -name artifacts.sha256.sha256 \
                ! -name manifest-validation.log \
                ! -name manifest-validation.exit \
                ! -name artifact-validation.txt \
                -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum > artifacts.sha256 &&
            sha256sum --check artifacts.sha256 &&
            sha256sum artifacts.sha256 > artifacts.sha256.sha256 &&
            sha256sum --check artifacts.sha256.sha256
        ) > "$artifact_dir/manifest-validation.log" 2>&1; then
            manifest_digests_verified=true
            printf '0\n' >| "$artifact_dir/manifest-validation.exit"
        else
            status=$?
            printf '%s\n' "$status" >| "$artifact_dir/manifest-validation.exit"
            record_error "artifact checksum contract failed (exit=$status)"
        fi
        write_report
        sha256sum "$artifact_dir/artifact-validation.txt" > "$artifact_dir/artifact-validation.txt.sha256"
        if ! (cd "$artifact_dir" && sha256sum --check artifact-validation.txt.sha256) >> "$artifact_dir/manifest-validation.log" 2>&1; then
            record_error 'final validation report checksum failed'
            write_report
        fi
        printf 'artifact_dir=%s\nacceptance_test_exit=%s\n' "$artifact_dir" "$failed"
        cat "$artifact_dir/artifact-validation.txt"
        if [[ "$failed" -ne 0 || "$exit_status" -ne 0 ]]; then exit 1; fi
    fi
    exit "$exit_status"
}
trap finalize EXIT

if [[ "$failed" -eq 0 ]]; then
    if ! mkdir -p -- "$target_dir" "$temporary_dir" "$(dirname -- "$gate_log")"; then
        record_error 'could not create external L3 target, scratch, or gate-log directories'
        exit 1
    fi
    if ! : > "$gate_log"; then
        record_error "could not create external L3 gate log without overwriting: $gate_log"
        exit 1
    fi
    export CARGO_TARGET_DIR="$target_dir"
    export TMPDIR="$temporary_dir"
fi

cd "$ROOT_DIR"
if snapshot_sources before; then snapshot_before_ok=true; else snapshot_before_ok=false; fi
printf '%s\n' "$PROFILE" > "$artifact_dir/profile.txt"
printf 'head=%s\nbranch=%s\n' "$(git rev-parse HEAD 2>/dev/null || printf unavailable)" \
    "$(git branch --show-current 2>/dev/null || printf unavailable)" > "$artifact_dir/repository.txt"

for tool in bash cargo rustc sha256sum realpath find sort cmp git grep tr date; do
    if ! command -v "$tool" >/dev/null 2>&1; then record_error "required tool is missing: $tool"; fi
done
if [[ -v MIMI_L3_VALGRIND && -n "$MIMI_L3_VALGRIND" ]]; then
    valgrind_path="$(realpath -- "$MIMI_L3_VALGRIND" 2>/dev/null || true)"
elif command -v valgrind >/dev/null 2>&1; then
    valgrind_path="$(realpath -- "$(command -v valgrind)")"
fi
if [[ -z "$valgrind_path" || ! -x "$valgrind_path" ]]; then
    record_error 'Valgrind is unavailable; set MIMI_L3_VALGRIND to its canonical executable path'
else
    export MIMI_L3_VALGRIND="$valgrind_path"
fi
if [[ ! -d "$LLVM_SYS_181_PREFIX" ]]; then record_error "LLVM wrapper directory is unavailable: $LLVM_SYS_181_PREFIX"; fi
if [[ -n "${CARGO_TARGET_DIR:-}" && "$CARGO_TARGET_DIR" != "$target_dir" ]]; then
    record_error "CARGO_TARGET_DIR was changed after the external target path was selected"
fi
if [[ -n "${TMPDIR:-}" && "$TMPDIR" != "$temporary_dir" ]]; then
    record_error "TMPDIR was changed after the external scratch path was selected"
fi
if [[ "$failed" -ne 0 ]]; then exit 1; fi
LLVM_SYS_181_PREFIX="$(realpath -- "$LLVM_SYS_181_PREFIX")"
export LLVM_SYS_181_PREFIX
export CARGO_BUILD_JOBS=1
export CARGO_TERM_COLOR=never

run_stage strict-native-memcheck 0 cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --test "$TEST_TARGET" "$TEST_NAME" -- --exact --ignored --test-threads=1 --show-output

printf 'L3 Memcheck evidence captured for profile=%s\n' "$PROFILE"
