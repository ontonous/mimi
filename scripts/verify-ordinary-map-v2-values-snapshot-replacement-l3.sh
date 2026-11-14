#!/usr/bin/env bash
set -euo pipefail
set -o noclobber

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
source "$ROOT_DIR/scripts/lib/l3_memcheck_validator.sh"
if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper; fi
ARTIFACT_ENV=MIMI_MAP_V2_VALUES_SNAPSHOT_REPLACEMENT_L3_ARTIFACT_DIR
TEST_TARGET=canonical_mir_ordinary_map_any_v2_values_snapshot_replacement_memcheck_l3
TEST_NAME=ordinary_map_owned_any_v2_values_snapshot_replacement_cli_and_native_memcheck_are_strict

for tool in cargo rustc valgrind sha256sum realpath; do
    command -v "$tool" >/dev/null 2>&1 || { printf 'ValuesSnapshotReplacement strict L3 requires %s\n' "$tool" >&2; exit 1; }
done
[ -d "$LLVM_SYS_181_PREFIX" ] || { printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2; exit 1; }
bash "$ROOT_DIR/scripts/test-l3-memcheck-validator.sh"

artifact_dir=""
if [[ -v MIMI_MAP_V2_VALUES_SNAPSHOT_REPLACEMENT_L3_ARTIFACT_DIR && -n "$MIMI_MAP_V2_VALUES_SNAPSHOT_REPLACEMENT_L3_ARTIFACT_DIR" ]]; then
    requested="$MIMI_MAP_V2_VALUES_SNAPSHOT_REPLACEMENT_L3_ARTIFACT_DIR"
    case "$requested" in /*) ;; *) requested="$ROOT_DIR/$requested" ;; esac
else
    requested="${TMPDIR:-/tmp}/mimi-map-v2-values-snapshot-replacement-l3-$$"
fi
case "$requested" in /*) ;; *) requested="$ROOT_DIR/$requested" ;; esac
if [ -e "$requested" ] || [ -L "$requested" ]; then
    printf 'refusing to overwrite existing ValuesSnapshotReplacement L3 artifact path: %s\n' "$requested" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/scripts/"*|"$ROOT_DIR/target/"*)
        printf 'ValuesSnapshotReplacement L3 artifacts must be outside source/build trees: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
export MIMI_MAP_V2_VALUES_SNAPSHOT_REPLACEMENT_L3_ARTIFACT_DIR="$artifact_dir"

if [[ -v MIMI_MAP_V2_VALUES_SNAPSHOT_REPLACEMENT_L3_LOG && -n "$MIMI_MAP_V2_VALUES_SNAPSHOT_REPLACEMENT_L3_LOG" ]]; then
    log_file="$MIMI_MAP_V2_VALUES_SNAPSHOT_REPLACEMENT_L3_LOG"
else
    log_file="/tmp/mimi-map-v2-values-snapshot-replacement-l3-$$.log"
fi
case "$log_file" in /*) ;; *) log_file="$ROOT_DIR/$log_file" ;; esac
if [ -e "$log_file" ] || [ -L "$log_file" ]; then
    printf 'refusing to overwrite existing ValuesSnapshotReplacement L3 log: %s\n' "$log_file" >&2
    exit 2
fi
mkdir -p -- "$(dirname "$log_file")"
log_file_absolute="$(realpath -m -- "$log_file")"
if [ "$log_file_absolute" = "$artifact_dir" ] || [[ "$log_file_absolute" == "$artifact_dir/"* ]]; then
    printf 'ValuesSnapshotReplacement L3 suite log must be outside the artifact directory: %s\n' "$log_file_absolute" >&2
    exit 2
fi
scratch="$(mktemp -d /tmp/mimi-map-v2-values-snapshot-replacement-l3.XXXXXX)"
trap 'rm -rf -- "$scratch"' EXIT

validate_values_snapshot_replacement_memcheck_capture() {
    local capture_directory="$1" mode="$2" expected_binary="$3"
    local summary="$capture_directory/memcheck-summary.txt"
    local raw="$capture_directory/valgrind.log"
    local summary_pid last_byte normalized_directory

    # The shared validator already covers Valgrind's compact all-free shape
    # (no loss-category rows) and the reachable shape. For Valgrind builds that
    # include zero-valued loss categories or a separate zero suppressed-heap
    # row alongside the all-free marker, remove only those verified rows and
    # pass the remaining full raw report through the strict layout checks.
    if ! grep -Fq 'All heap blocks were freed -- no leaks are possible' "$summary"; then
        _l3_memcheck_validate_capture "$capture_directory" "$mode" "$expected_binary"
        return $?
    fi
    if ! grep -Eq '^==[1-9][0-9]*==[[:space:]]+(LEAK SUMMARY:|(definitely lost|indirectly lost|possibly lost|still reachable|suppressed):)' "$raw"; then
        _l3_memcheck_validate_capture "$capture_directory" "$mode" "$expected_binary"
        return $?
    fi

    if [[ ! -s "$summary" || ! -s "$raw" ]]; then
        _l3_memcheck_error 'missing non-empty summary or raw Valgrind log' "$mode"
        return 1
    fi
    last_byte="$(tail -c 1 -- "$raw" | od -An -t x1 | tr -d '[:space:]')"
    if [[ "$last_byte" != 0a ]]; then
        _l3_memcheck_error 'raw Valgrind log is truncated or lacks its final newline' "$mode"
        return 1
    fi
    local -a summary_rows=()
    mapfile -t summary_rows < "$summary"
    if (( ${#summary_rows[@]} != 3 )); then
        _l3_memcheck_error 'all-free summary is not the exact three-row form' "$mode"
        return 1
    fi
    if [[ ! "${summary_rows[0]}" =~ ^==([1-9][0-9]*)==[[:space:]]+ ]]; then
        _l3_memcheck_error 'all-free summary is not the exact three-row form' "$mode"
        return 1
    fi
    summary_pid="${BASH_REMATCH[1]}"
    if [[ "${summary_rows[0]}" != "==${summary_pid}== in use at exit: 0 bytes in 0 blocks" \
        || "${summary_rows[1]}" != "==${summary_pid}== All heap blocks were freed -- no leaks are possible" \
        || "${summary_rows[2]}" != "==${summary_pid}== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)" ]]; then
        _l3_memcheck_error 'all-free summary lacks exact zero-in-use, all-free, or zero-suppression rows' "$mode"
        return 1
    fi

    normalized_directory="$scratch/${mode}-normalized-memcheck"
    mkdir -- "$normalized_directory"
    cp -- "$summary" "$normalized_directory/memcheck-summary.txt"
    if ! awk -v pid="$summary_pid" '
        function fail() { invalid = 1; exit 1 }
        BEGIN {
            prefix = "==" pid "=="
            in_leak_summary = 0
            leak_summary_count = 0
            freed_count = 0
            definitely_count = 0
            indirectly_count = 0
            possibly_count = 0
            reachable_count = 0
            suppressed_count = 0
            footer_seen = 0
            outside_loss_rows_seen = 0
        }
        {
            if ($0 == prefix " LEAK SUMMARY:") {
                if (leak_summary_count++ != 0 || freed_count != 1 || in_leak_summary || footer_seen) fail()
                in_leak_summary = 1
                next
            }
            if (in_leak_summary) {
                if ($0 == prefix "    definitely lost: 0 bytes in 0 blocks") {
                    if (definitely_count++ != 0) fail()
                    next
                }
                if ($0 == prefix "    indirectly lost: 0 bytes in 0 blocks") {
                    if (indirectly_count++ != 0) fail()
                    next
                }
                if ($0 == prefix "      possibly lost: 0 bytes in 0 blocks") {
                    if (possibly_count++ != 0) fail()
                    next
                }
                if ($0 == prefix "    still reachable: 0 bytes in 0 blocks") {
                    if (reachable_count++ != 0) fail()
                    next
                }
                if ($0 == prefix "         suppressed: 0 bytes in 0 blocks") {
                    if (suppressed_count++ != 0) fail()
                    next
                }
                if ($0 == prefix " ") {
                    if (definitely_count != 1 || indirectly_count != 1 || possibly_count != 1 || reachable_count != 1) fail()
                    in_leak_summary = 0
                    next
                }
                fail()
            }
            if ($0 == prefix "    definitely lost: 0 bytes in 0 blocks") {
                if (definitely_count++ != 0 || freed_count != 1 || footer_seen) fail()
                outside_loss_rows_seen = 1
                next
            }
            if ($0 == prefix "    indirectly lost: 0 bytes in 0 blocks") {
                if (indirectly_count++ != 0 || freed_count != 1 || footer_seen) fail()
                outside_loss_rows_seen = 1
                next
            }
            if ($0 == prefix "      possibly lost: 0 bytes in 0 blocks") {
                if (possibly_count++ != 0 || freed_count != 1 || footer_seen) fail()
                outside_loss_rows_seen = 1
                next
            }
            if ($0 == prefix "    still reachable: 0 bytes in 0 blocks") {
                if (reachable_count++ != 0 || freed_count != 1 || footer_seen) fail()
                outside_loss_rows_seen = 1
                next
            }
            if ($0 == prefix "         suppressed: 0 bytes in 0 blocks") {
                if (suppressed_count++ != 0 || freed_count != 1 || footer_seen) fail()
                outside_loss_rows_seen = 1
                next
            }
            if ($0 == prefix " " && outside_loss_rows_seen) {
                outside_loss_rows_seen = 0
                next
            }
            if ($0 == prefix " All heap blocks were freed -- no leaks are possible") {
                freed_count++
            }
            if ($0 == prefix " For lists of detected and suppressed errors, rerun with: -s" \
                || $0 ~ ("^" prefix " ERROR SUMMARY:")) footer_seen = 1
            print
        }
        END {
            if (invalid || in_leak_summary || leak_summary_count > 1 || freed_count != 1 || suppressed_count > 1) exit 1
            if (!((definitely_count == 0 && indirectly_count == 0 && possibly_count == 0 && reachable_count == 0) \
                || (definitely_count == 1 && indirectly_count == 1 && possibly_count == 1 && reachable_count == 1))) exit 1
            if (leak_summary_count == 1 \
                && (definitely_count != 1 || indirectly_count != 1 || possibly_count != 1 || reachable_count != 1)) exit 1
        }
    ' "$raw" > "$normalized_directory/valgrind.log"; then
        _l3_memcheck_error 'all-free report has incomplete, nonzero, malformed, or duplicated loss-summary rows' "$mode"
        return 1
    fi
    _l3_memcheck_validate_capture "$normalized_directory" "$mode" "$expected_binary"
}

path_is_within() {
    local candidate="$1" directory="$2"
    [[ -n "$directory" && ( "$candidate" == "$directory" || "$candidate" == "$directory"/* ) ]]
}

source_snapshot_manifest() {
    {
        find src tests std -type f -print0
        printf '%s\0' Cargo.toml
        [ ! -f Cargo.lock ] || printf '%s\0' Cargo.lock
        [ ! -f build.rs ] || printf '%s\0' build.rs
        printf '%s\0' scripts/verify-ordinary-map-v2-values-snapshot-replacement-l3.sh
        printf '%s\0' scripts/lib/l3_memcheck_validator.sh scripts/test-l3-memcheck-validator.sh
    } | sort -zu | while IFS= read -r -d '' source_path; do
        source_absolute="$(realpath -m -- "$ROOT_DIR/$source_path")"
        if path_is_within "$source_absolute" "$artifact_dir" || path_is_within "$source_absolute" "$log_file_absolute"; then
            continue
        fi
        sha256sum -- "$source_path"
    done
}

record_tool() {
    local label="$1"
    local command_name="$2"
    shift 2
    local executable
    local directory="$artifact_dir/toolchain/$label"
    executable="$(command -v -- "$command_name")"
    mkdir -- "$directory"
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
source_snapshot_manifest > "$scratch/source-before.manifest"
snapshot_before="$(sha256sum "$scratch/source-before.manifest" | awk '{print $1}')"
if [ -n "$artifact_dir" ]; then
    cp "$scratch/source-before.manifest" "$artifact_dir/source-snapshot.before.manifest.sha256"
    printf '%s\n' "$snapshot_before" > "$artifact_dir/source-snapshot.before.sha256"
    mkdir -- "$artifact_dir/toolchain"
    toolchain_status=0
    record_tool rustc rustc --version || toolchain_status=$?
    record_tool cargo cargo --version || toolchain_status=$?
    record_tool valgrind valgrind --version || toolchain_status=$?
    record_tool sha256sum sha256sum --version || toolchain_status=$?
    {
        printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
        printf 'rustc_path=%s\nrustc_version=' "$(command -v rustc)"
        cat "$artifact_dir/toolchain/rustc/stdout.bin"
        printf 'cargo_path=%s\ncargo_version=' "$(command -v cargo)"
        cat "$artifact_dir/toolchain/cargo/stdout.bin"
        printf 'valgrind_path=%s\nvalgrind_version=' "$(command -v valgrind)"
        cat "$artifact_dir/toolchain/valgrind/stdout.bin"
        printf 'sha256sum_path=%s\nsha256sum_version=' "$(command -v sha256sum)"
        cat "$artifact_dir/toolchain/sha256sum/stdout.bin"
    } > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"
fi

cargo_arguments=(
    test --manifest-path "$ROOT_DIR/Cargo.toml"
    --test "$TEST_TARGET" "$TEST_NAME" --
    --exact --ignored --nocapture --test-threads=1
)
test_status=0
if [ -n "$artifact_dir" ]; then
    {
        printf 'LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 MIMI_MAP_V2_VALUES_SNAPSHOT_REPLACEMENT_L3_ARTIFACT_DIR=%q MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 %q ' \
            "$LLVM_SYS_181_PREFIX" "$artifact_dir" "$(command -v cargo)"
        printf '%q ' "${cargo_arguments[@]}"
        printf '\n'
    } > "$artifact_dir/suite.command.txt"
    printf '%s\0' "$(command -v cargo)" "${cargo_arguments[@]}" > "$artifact_dir/suite.argv.nul"
    printf 'LLVM_SYS_181_PREFIX=%s\nCARGO_BUILD_JOBS=1\nMIMI_ROUTE_CENSUS=1\nMIMI_VERBOSE=1\n%s=%s\n' \
        "$LLVM_SYS_181_PREFIX" "$ARTIFACT_ENV" "$artifact_dir" > "$artifact_dir/suite.environment.txt"
    if [ "$toolchain_status" -ne 0 ]; then
        test_status="$toolchain_status"
        : > "$artifact_dir/suite.stdout"
        printf 'toolchain version command failed; see toolchain captures\n' > "$artifact_dir/suite.stderr"
    elif env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" CARGO_BUILD_JOBS=1 MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
        MIMI_MAP_V2_VALUES_SNAPSHOT_REPLACEMENT_L3_ARTIFACT_DIR="$artifact_dir" \
        cargo "${cargo_arguments[@]}" > "$artifact_dir/suite.stdout" 2> "$artifact_dir/suite.stderr"; then
        test_status=0
    else
        test_status=$?
    fi
    printf '%s\n' "$test_status" > "$artifact_dir/suite.exit"
    {
        printf '===== stdout =====\n'; cat "$artifact_dir/suite.stdout"
        printf '\n===== stderr =====\n'; cat "$artifact_dir/suite.stderr"
    } > "$artifact_dir/suite.log"
    cat "$artifact_dir/suite.stdout"
    cat "$artifact_dir/suite.stderr" >&2
    cp "$artifact_dir/suite.log" "$log_file"
else
    if env -u MIMI_MAP_V2_VALUES_SNAPSHOT_REPLACEMENT_L3_ARTIFACT_DIR \
        LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" CARGO_BUILD_JOBS=1 MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
        cargo "${cargo_arguments[@]}" 2>&1 | tee "$log_file"; then
        test_status=0
    else
        test_status=$?
    fi
fi

source_snapshot_manifest > "$scratch/source-after.manifest"
snapshot_after="$(sha256sum "$scratch/source-after.manifest" | awk '{print $1}')"
printf 'source_snapshot_sha256=%s\nsource_snapshot_sha256_after=%s\n' "$snapshot_before" "$snapshot_after"
snapshot_status=0
if ! cmp -s "$scratch/source-before.manifest" "$scratch/source-after.manifest"; then
    printf 'source snapshot changed during the ValuesSnapshotReplacement strict L3 run\n' >&2
    snapshot_status=98
fi

validation_status=0
if [ -n "$artifact_dir" ]; then
    cp "$scratch/source-after.manifest" "$artifact_dir/source-snapshot.after.manifest.sha256"
    printf '%s\n' "$snapshot_after" > "$artifact_dir/source-snapshot.after.sha256"
    if [ "$test_status" -eq 0 ]; then
        summary="$artifact_dir/native/positive/run-summary.txt"
        if [ ! -s "$summary" ]; then
            printf 'successful ValuesSnapshotReplacement L3 test left no positive run summary\n' >&2
            validation_status=1
        else
            for expected in \
                'route_profile=map-any-owner-v2-values-snapshot-replacement-v1' \
                'global_map_any_v2_production_ready=false' \
                'cli_receipt_profile=cli-mir-v1' \
                'case_count=1' \
                'cli_receipt_count=1' \
                'default_run_count=1' \
                'explicit_mir_run_count=1' \
                'native_build_count=2' \
                'memcheck_run_count=2' \
                'default_native_build_receipt_count=1' \
                'explicit_native_build_receipt_count=1' \
                'default_verify_count=1' \
                'explicit_mir_verify_count=1' \
                'negative_case_count=2' \
                'negative_route_attempt_count=12' \
                'legacy_fallback_count=0' \
                'expected_stdout_hex=747275650a747275650a'; do
                grep -Fxq "$expected" "$summary" || validation_status=1
            done
            receipt_manifest="$artifact_dir/native/positive/receipt/stdout.bin"
            for pair in 'schema receipt_schema' 'mir_digest mir_digest' \
                'type_desc_digest type_desc_digest' 'abi_digest abi_digest' \
                'ffi_digest ffi_digest' 'ownership_digest ownership_digest' \
                'flow_transition_digest flow_transition_digest' 'root_owners root_owners'; do
                read -r receipt_field summary_field <<< "$pair"
                receipt_value="$(sed -n "s/^${receipt_field}=//p" "$receipt_manifest")"
                [ -n "$receipt_value" ] || { validation_status=1; continue; }
                grep -Fxq "${summary_field}=${receipt_value}" "$summary" || validation_status=1
            done
            [[ "$(find "$artifact_dir/native" -type f -name program | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name valgrind.log | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name memcheck-summary.txt | wc -l)" == 2 ]] || validation_status=1
            [[ "$(find "$artifact_dir/native" -type f -name program ! -perm -111 | wc -l)" == 0 ]] || validation_status=1
            for report in "$artifact_dir"/native/positive/{default,explicit}-memcheck/memcheck-summary.txt; do
                [ -s "$report" ] || { validation_status=1; continue; }
                capture_directory="${report%/memcheck-summary.txt}"
                memcheck_mode="$(basename "$capture_directory")"
                memcheck_mode="${memcheck_mode%-memcheck}"
                expected_binary="$(realpath "$capture_directory/../${memcheck_mode}-build/program")"
                validate_values_snapshot_replacement_memcheck_capture \
                    "$capture_directory" "$memcheck_mode" "$expected_binary" || validation_status=1
            done
            for status_file in \
                "$artifact_dir/native/mimi-binary-sha256/process.status" \
                "$artifact_dir/native/valgrind-version/process.status" \
                "$artifact_dir/native/positive/receipt/process.status" \
                "$artifact_dir/native/positive/default-run/process.status" \
                "$artifact_dir/native/positive/explicit-run/process.status" \
                "$artifact_dir/native/positive/default-build/process.status" \
                "$artifact_dir/native/positive/explicit-build/process.status" \
                "$artifact_dir/native/positive/default-verify/process.status" \
                "$artifact_dir/native/positive/explicit-verify/process.status" \
                "$artifact_dir/native/positive/default-memcheck/process.status" \
                "$artifact_dir/native/positive/explicit-memcheck/process.status"; do
                grep -Fxq 'exit_code=0' "$status_file" || validation_status=1
            done
            for case_directory in \
                "$artifact_dir/native/negative/broader-values-unpack" \
                "$artifact_dir/native/negative/altered-index-source"; do
                [ -s "$case_directory/receipt/process.status" ] || validation_status=1
                grep -Fxq 'exit_code=0' "$case_directory/receipt/process.status" || validation_status=1
                for mode in default-run explicit-mir-run default-build explicit-mir-build default-verify explicit-mir-verify; do
                    status_file="$case_directory/$mode/process.status"
                    [ -s "$status_file" ] || { validation_status=1; continue; }
                    grep -Fxq 'exit_code=0' "$status_file" && validation_status=1
                done
                [[ "$(find "$case_directory" -type f -name program | wc -l)" == 0 ]] || validation_status=1
            done
        fi
    fi
    if [ "$snapshot_status" -ne 0 ]; then
        printf 'FAIL source snapshot changed\n' > "$artifact_dir/artifact-validation.txt"
    elif [ "$test_status" -ne 0 ]; then
        printf 'INCOMPLETE test exit=%s; partial evidence retained\n' "$test_status" > "$artifact_dir/artifact-validation.txt"
    elif [ "$validation_status" -eq 0 ]; then
        printf 'PASS ValuesSnapshotReplacement-only strict L3; global Map/Any V2 remains closed\n' > "$artifact_dir/artifact-validation.txt"
    else
        printf 'FAIL required ValuesSnapshotReplacement L3 artifact validation\n' > "$artifact_dir/artifact-validation.txt"
        test_status=97
    fi
    (
        cd "$artifact_dir"
        find . -type f ! -name files.sha256 ! -name files.sha256.sha256 -print0 \
            | sort -z | xargs -0 -r sha256sum > files.sha256
        sha256sum --check files.sha256
        sha256sum files.sha256 > files.sha256.sha256
    )
fi

printf 'acceptance_test_exit=%s\n' "$test_status"
printf 'artifact_dir=%s\n' "$artifact_dir"
[ "$snapshot_status" -eq 0 ] || exit "$snapshot_status"
exit "$test_status"
