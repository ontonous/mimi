#!/usr/bin/env bash
set -euo pipefail
set -o noclobber

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper; fi
ARTIFACT_ENV=MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR
TEST_TARGET=canonical_mir_ordinary_map_any_v2_values_local_index_i32_memcheck_l3
TEST_NAME=ordinary_map_owned_any_v2_values_local_index_i32_result_branch_cli_and_native_memcheck_are_strict
FAILURE_TEST_NAME=nested_result_values_publication_and_selected_owner_failures_are_memcheck_clean

for tool in cargo rustc valgrind sha256sum realpath git; do
    command -v "$tool" >/dev/null 2>&1 || { printf 'ValuesLocalIndexI32 strict L3 requires %s\n' "$tool" >&2; exit 1; }
done
[ -d "$LLVM_SYS_181_PREFIX" ] || { printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2; exit 1; }

artifact_dir=""
if [[ -v MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR && -n "$MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR" ]]; then
    requested="$MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR"
    case "$requested" in /*) ;; *) requested="$ROOT_DIR/$requested" ;; esac
else
    requested="${TMPDIR:-/tmp}/mimi-map-v2-values-local-index-i32-l3-$$"
fi
case "$requested" in /*) ;; *) requested="$ROOT_DIR/$requested" ;; esac
if [ -e "$requested" ] || [ -L "$requested" ]; then
    printf 'refusing to overwrite existing ValuesLocalIndexI32 L3 artifact path: %s\n' "$requested" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/scripts/"*|"$ROOT_DIR/target/"*)
        printf 'ValuesLocalIndexI32 L3 artifacts must be outside source/build trees: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
export MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR="$artifact_dir"

if [[ -v MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_LOG && -n "$MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_LOG" ]]; then
    log_file="$MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_LOG"
else
    log_file="/tmp/mimi-map-v2-values-local-index-i32-l3-$$.log"
fi
case "$log_file" in /*) ;; *) log_file="$ROOT_DIR/$log_file" ;; esac
if [ -e "$log_file" ] || [ -L "$log_file" ]; then
    printf 'refusing to overwrite existing ValuesLocalIndexI32 L3 log: %s\n' "$log_file" >&2
    exit 2
fi
mkdir -p -- "$(dirname "$log_file")"
log_file_absolute="$(realpath -m -- "$log_file")"
if [ "$log_file_absolute" = "$artifact_dir" ] || [[ "$log_file_absolute" == "$artifact_dir/"* ]]; then
    printf 'ValuesLocalIndexI32 L3 suite log must be outside the artifact directory: %s\n' "$log_file_absolute" >&2
    exit 2
fi
scratch="$(mktemp -d /tmp/mimi-map-v2-values-local-index-i32-l3.XXXXXX)"
trap 'rm -rf -- "$scratch"' EXIT

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
        printf '%s\0' scripts/verify-ordinary-map-v2-values-local-index-i32-l3.sh
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
git -C "$ROOT_DIR" rev-parse HEAD > "$artifact_dir/git-head.before.txt"
git -C "$ROOT_DIR" branch --show-current > "$artifact_dir/git-branch.before.txt"
git -C "$ROOT_DIR" status --porcelain=v1 --untracked-files=all > "$artifact_dir/git-status.before.txt"
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
    record_tool git git --version || toolchain_status=$?
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
        printf 'git_path=%s\ngit_version=' "$(command -v git)"
        cat "$artifact_dir/toolchain/git/stdout.bin"
    } > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"
fi

cargo_arguments=(
    test --manifest-path "$ROOT_DIR/Cargo.toml"
    --test "$TEST_TARGET" "$TEST_NAME" --
    --exact --ignored --nocapture --test-threads=1
)
failure_cargo_arguments=(
    test --manifest-path "$ROOT_DIR/Cargo.toml"
    --lib "$FAILURE_TEST_NAME" --
    --ignored --nocapture --test-threads=1
)
test_status=0
if [ -n "$artifact_dir" ]; then
    {
        printf 'LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR=%q MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 %q ' \
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
        MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR="$artifact_dir" \
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
    if env -u MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR \
        LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" CARGO_BUILD_JOBS=1 MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 \
        cargo "${cargo_arguments[@]}" 2>&1 | tee "$log_file"; then
        test_status=0
    else
        test_status=$?
    fi
fi

failure_test_status=1
if [ "$test_status" -eq 0 ]; then
    if [ -n "$artifact_dir" ]; then
        {
            printf 'LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR=%q %q ' \
                "$LLVM_SYS_181_PREFIX" "$artifact_dir" "$(command -v cargo)"
            printf '%q ' "${failure_cargo_arguments[@]}"
            printf '\n'
        } > "$artifact_dir/failure-suite.command.txt"
        printf '%s\0' "$(command -v cargo)" "${failure_cargo_arguments[@]}" \
            > "$artifact_dir/failure-suite.argv.nul"
        printf 'LLVM_SYS_181_PREFIX=%s\nCARGO_BUILD_JOBS=1\n%s=%s\n' \
            "$LLVM_SYS_181_PREFIX" "$ARTIFACT_ENV" "$artifact_dir" \
            > "$artifact_dir/failure-suite.environment.txt"
        if env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" CARGO_BUILD_JOBS=1 \
            MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR="$artifact_dir" \
            cargo "${failure_cargo_arguments[@]}" \
            > "$artifact_dir/failure-suite.stdout" 2> "$artifact_dir/failure-suite.stderr"; then
            failure_test_status=0
        else
            failure_test_status=$?
        fi
        printf '%s\n' "$failure_test_status" > "$artifact_dir/failure-suite.exit"
        {
            printf '===== stdout =====\n'; cat "$artifact_dir/failure-suite.stdout"
            printf '\n===== stderr =====\n'; cat "$artifact_dir/failure-suite.stderr"
        } > "$artifact_dir/failure-suite.log"
        cat "$artifact_dir/failure-suite.stdout"
        cat "$artifact_dir/failure-suite.stderr" >&2
    else
        if env -u MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR \
            LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" CARGO_BUILD_JOBS=1 \
            cargo "${failure_cargo_arguments[@]}" 2>&1 | tee -a "$log_file"; then
            failure_test_status=0
        else
            failure_test_status=$?
        fi
    fi
fi
if [ "$failure_test_status" -ne 0 ]; then
    test_status="$failure_test_status"
fi

source_snapshot_manifest > "$scratch/source-after.manifest"
git -C "$ROOT_DIR" rev-parse HEAD > "$artifact_dir/git-head.after.txt"
git -C "$ROOT_DIR" branch --show-current > "$artifact_dir/git-branch.after.txt"
git -C "$ROOT_DIR" status --porcelain=v1 --untracked-files=all > "$artifact_dir/git-status.after.txt"
snapshot_after="$(sha256sum "$scratch/source-after.manifest" | awk '{print $1}')"
printf 'source_snapshot_sha256=%s\nsource_snapshot_sha256_after=%s\n' "$snapshot_before" "$snapshot_after"
snapshot_status=0
if ! cmp -s "$scratch/source-before.manifest" "$scratch/source-after.manifest"; then
    printf 'source snapshot changed during the ValuesLocalIndexI32 strict L3 run\n' >&2
    snapshot_status=98
fi
if ! cmp -s "$artifact_dir/git-head.before.txt" "$artifact_dir/git-head.after.txt" \
    || ! cmp -s "$artifact_dir/git-branch.before.txt" "$artifact_dir/git-branch.after.txt"; then
    printf 'Git branch or HEAD changed during the ValuesLocalIndexI32 strict L3 run\n' >&2
    snapshot_status=98
fi

validation_status=0
if [ -n "$artifact_dir" ]; then
    cp "$scratch/source-after.manifest" "$artifact_dir/source-snapshot.after.manifest.sha256"
    printf '%s\n' "$snapshot_after" > "$artifact_dir/source-snapshot.after.sha256"
    if [ "$test_status" -eq 0 ]; then
        summary="$artifact_dir/native/positive/run-summary.txt"
        if [ ! -s "$summary" ]; then
            printf 'successful ValuesLocalIndexI32 L3 test left no positive run summary\n' >&2
            validation_status=1
        else
            for expected in \
                'route_profile=map-any-owner-v2-values-local-index-i32-result-branch-v2' \
                'global_map_any_v2_production_ready=false' \
                'expected_program_exit_code=41' \
                'expected_stdout_hex=' \
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
                'legacy_fallback_count=0'; do
                grep -Fxq "$expected" "$summary" || validation_status=1
            done
            route_manifest="$artifact_dir/native/positive/route-manifest.txt"
            expected_consumer_profiles="map-any-owner-v2-values-local-index-i32-result-branch-v2,map-any-owner-v2-values-local-index-i32-result-branch-v2,map-any-owner-v2-values-local-index-i32-result-branch-v2,map-any-owner-v2-values-local-index-i32-result-branch-v2,map-any-owner-v2-values-local-index-i32-result-branch-v2,map-any-owner-v2-values-local-index-i32-result-branch-v2"
            grep -Fxq "consumer_profiles=$expected_consumer_profiles" "$route_manifest" || validation_status=1
            for expected in \
                'schema=mimi-ordinary-map-owned-any-v2-l3-route-manifest-v1' \
                'route_profile=map-any-owner-v2-values-local-index-i32-result-branch-v2' \
                'global_map_any_v2_production_ready=false' \
                'fixture=tests/fixtures/mir_ordinary_map_any_owner_v2_values_local_index_i32_result_branch.mimi' \
                'expected_program_exit_code=41' \
                'expected_stdout_hex='; do
                grep -Fxq "$expected" "$route_manifest" || validation_status=1
            done
            fixture="$ROOT_DIR/tests/fixtures/mir_ordinary_map_any_owner_v2_values_local_index_i32_result_branch.mimi"
            [ -s "$fixture" ] || validation_status=1
            cmp -s "$fixture" "$artifact_dir/native/positive/source.fixture.mimi" || validation_status=1
            fixture_sha256="$(sha256sum -- "$fixture" | awk '{print $1}')"
            grep -Fxq "fixture_sha256=$fixture_sha256" "$route_manifest" || validation_status=1
            grep -Fxq "fixture_sha256=$fixture_sha256" "$summary" || validation_status=1
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
            for build_mode in default explicit; do
                build_directory="$artifact_dir/native/positive/${build_mode}-build"
                identity_file="$build_directory/program-identity.txt"
                program="$build_directory/program"
                if [ ! -s "$identity_file" ] || [ ! -s "$program" ]; then
                    validation_status=1
                    continue
                fi
                digest="$(sed -n 's/^sha256=//p' "$identity_file")"
                [[ "$(grep -c '^sha256=' "$identity_file")" == 1 ]] || validation_status=1
                [[ "$digest" =~ ^[[:xdigit:]]{64}$ ]] || validation_status=1
                [[ "$digest" == "$(sha256sum -- "$program" | awk '{print $1}')" ]] || validation_status=1
            done
            for report in "$artifact_dir"/native/positive/{default,explicit}-memcheck/memcheck-summary.txt; do
                [ -s "$report" ] || { validation_status=1; continue; }
                for expected in 'definitely lost: 0 bytes in 0 blocks' \
                    'indirectly lost: 0 bytes in 0 blocks' \
                    'possibly lost: 0 bytes in 0 blocks' \
                    'still reachable: 0 bytes in 0 blocks' \
                    'ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)'; do
                    grep -Fq "$expected" "$report" || validation_status=1
                done
            done
            if ! grep -Fxq '0' "$artifact_dir/failure-suite.exit" || \
                ! grep -Fq "test $FAILURE_TEST_NAME ... ok" "$artifact_dir/failure-suite.log" || \
                ! grep -Fq 'test result: ok.' "$artifact_dir/failure-suite.log"; then
                validation_status=1
            fi
            [[ "$(find "$artifact_dir/native/failure" -mindepth 1 -maxdepth 1 -type d | wc -l)" == 2 ]] || validation_status=1
            for case_name in values-publish selected-owner-clone; do
                failure_directory="$artifact_dir/native/failure/$case_name"
                [ -s "$failure_directory/process.status" ] || { validation_status=1; continue; }
                grep -Fxq 'exit_code=0' "$failure_directory/process.status" || validation_status=1
                grep -Fq 'exact_e0800=true' "$failure_directory/stderr.bin" || validation_status=1
                grep -Fq 'valid=true' "$failure_directory/stderr.bin" || validation_status=1
                grep -Fq 'result_published=false' "$failure_directory/stderr.bin" || validation_status=1
                grep -Fq 'ERROR SUMMARY: 0 errors' "$failure_directory/stderr.bin" || validation_status=1
                for expected in 'definitely lost: 0 bytes' 'indirectly lost: 0 bytes' 'possibly lost: 0 bytes'; do
                    grep -Fq "$expected" "$failure_directory/stderr.bin" || validation_status=1
                done
            done
            for status_file in \
                "$artifact_dir/native/mimi-binary-sha256/process.status" \
                "$artifact_dir/native/valgrind-version/process.status" \
                "$artifact_dir/native/positive/receipt/process.status" \
                "$artifact_dir/native/positive/default-build/process.status" \
                "$artifact_dir/native/positive/explicit-build/process.status" \
                "$artifact_dir/native/positive/default-verify/process.status" \
                "$artifact_dir/native/positive/explicit-verify/process.status"; do
                grep -Fxq 'exit_code=0' "$status_file" || validation_status=1
            done
            for status_file in \
                "$artifact_dir/native/positive/default-run/process.status" \
                "$artifact_dir/native/positive/explicit-run/process.status" \
                "$artifact_dir/native/positive/default-memcheck/process.status" \
                "$artifact_dir/native/positive/explicit-memcheck/process.status"; do
                grep -Fxq 'exit_code=41' "$status_file" || validation_status=1
            done
            for case_directory in \
                "$artifact_dir/native/negative/broader-values-unpack" \
                "$artifact_dir/native/negative/altered-local-index-source"; do
                [ -s "$case_directory/receipt/process.status" ] || validation_status=1
                for mode in default-run explicit-mir-run default-build explicit-mir-build default-verify explicit-mir-verify; do
                    status_file="$case_directory/$mode/process.status"
                    [ -s "$status_file" ] || { validation_status=1; continue; }
                    grep -Fxq 'exit_code=0' "$status_file" && validation_status=1
                done
                [[ "$(find "$case_directory" -type f -name program | wc -l)" == 0 ]] || validation_status=1
            done
            broader_receipt="$artifact_dir/native/negative/broader-values-unpack/receipt/process.status"
            grep -Fxq 'exit_code=0' "$broader_receipt" || validation_status=1
            altered_receipt="$artifact_dir/native/negative/altered-local-index-source/receipt/process.status"
            grep -Fxq 'exit_code=1' "$altered_receipt" || validation_status=1
            grep -Fq 'MIR lowering failed' \
                "$artifact_dir/native/negative/altered-local-index-source/receipt/stderr.bin" || validation_status=1
            grep -Fq 'index projection has no canonical ownership receipt' \
                "$artifact_dir/native/negative/altered-local-index-source/receipt/stderr.bin" || validation_status=1
        fi
    fi
    if [ "$snapshot_status" -ne 0 ]; then
        printf 'FAIL source snapshot changed\n' > "$artifact_dir/artifact-validation.txt"
    elif [ "$test_status" -ne 0 ]; then
        printf 'INCOMPLETE test exit=%s; partial evidence retained\n' "$test_status" > "$artifact_dir/artifact-validation.txt"
    elif [ "$validation_status" -eq 0 ]; then
        printf 'PASS ValuesLocalIndexI32 native and failure-injection strict Memcheck L3; global Map/Any V2 remains closed\n' > "$artifact_dir/artifact-validation.txt"
    else
        printf 'FAIL required ValuesLocalIndexI32 L3 artifact validation\n' > "$artifact_dir/artifact-validation.txt"
        test_status=97
    fi
    (
        cd "$artifact_dir"
        find . -type f \
            ! -path './native/positive/default-build/program' \
            ! -path './native/positive/explicit-build/program' \
            ! -name files.sha256 ! -name files.sha256.sha256 -print0 \
            | sort -z | xargs -0 -r sha256sum > files.sha256
        index_validation_status=0
        if grep -Eq '^[[:xdigit:]]{64}  \./native/positive/(default|explicit)-build/program$' files.sha256; then
            index_validation_status=1
        fi
        for build_mode in default explicit; do
            grep -Fq "  ./native/positive/${build_mode}-build/program-identity.txt" files.sha256 || index_validation_status=1
        done
        if [ "$index_validation_status" -ne 0 ]; then
            printf 'FAIL artifact index must omit generated programs and retain their identity files\n' >| artifact-validation.txt
            test_status=97
            find . -type f \
                ! -path './native/positive/default-build/program' \
                ! -path './native/positive/explicit-build/program' \
                ! -name files.sha256 ! -name files.sha256.sha256 -print0 \
                | sort -z | xargs -0 -r sha256sum >| files.sha256
        fi
        sha256sum --check files.sha256
        sha256sum files.sha256 > files.sha256.sha256
        sha256sum --check files.sha256.sha256
    )
fi

printf 'acceptance_test_exit=%s\n' "$test_status"
printf 'artifact_dir=%s\n' "$artifact_dir"
[ "$snapshot_status" -eq 0 ] || exit "$snapshot_status"
exit "$test_status"
