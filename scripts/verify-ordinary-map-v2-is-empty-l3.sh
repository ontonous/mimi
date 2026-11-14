#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
requested_artifact_dir="${MIMI_MAP_V2_IS_EMPTY_L3_ARTIFACT_DIR:?Set MIMI_MAP_V2_IS_EMPTY_L3_ARTIFACT_DIR to a new evidence directory}"

if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
    printf 'IsEmpty L3 evidence capture requires Cargo and rustc\n' >&2
    exit 1
fi
if ! command -v valgrind >/dev/null 2>&1; then
    printf 'ordinary Map/Any IsEmpty L3 requires Valgrind\n' >&2
    exit 1
fi
if [ ! -d "$LLVM_SYS_181_PREFIX" ]; then
    printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi
LLVM_SYS_181_PREFIX="$(realpath -- "$LLVM_SYS_181_PREFIX")"

case "$requested_artifact_dir" in
    /*) ;;
    *) requested_artifact_dir="$ROOT_DIR/$requested_artifact_dir" ;;
esac
if [ -e "$requested_artifact_dir" ] || [ -L "$requested_artifact_dir" ]; then
    printf 'refusing to overwrite existing IsEmpty L3 evidence: %s\n' "$requested_artifact_dir" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested_artifact_dir")"
case "$artifact_dir" in
    "$ROOT_DIR"/*) ;;
    *)
        printf 'IsEmpty L3 evidence must be inside this repository: %s\n' "$artifact_dir" >&2
        exit 2
        ;;
esac
artifact_relative="${artifact_dir#"$ROOT_DIR"/}"
case "$artifact_relative" in
    .git|.git/*|.cargo|.cargo/*|src|src/*|tests|tests/*|std|std/*|scripts|scripts/*|Cargo.toml|Cargo.lock|build.rs)
        printf 'IsEmpty L3 evidence must be outside source and Git metadata: %s\n' "$artifact_dir" >&2
        exit 2
        ;;
esac
mkdir -p "$(dirname "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
export MIMI_MAP_V2_IS_EMPTY_L3_ARTIFACT_DIR="$artifact_dir"

# Keep only the evidence directory variable for the strict Rust harness. The
# child Mimi builds and Valgrind runs explicitly set their own route census and
# HOME, so ambient Mimi flags must not change the gate's semantics.
mapfile -t caller_mimi_environment_names < <(
    compgen -e | LC_ALL=C sort | awk 'index($0, "MIMI_") == 1'
)
{
    printf 'effective_mimi_environment=MIMI_MAP_V2_IS_EMPTY_L3_ARTIFACT_DIR only\n'
    for name in "${caller_mimi_environment_names[@]}"; do
        if [ "$name" != MIMI_MAP_V2_IS_EMPTY_L3_ARTIFACT_DIR ]; then
            printf 'caller_%s=was_set_and_sanitized\n' "$name"
        fi
    done
    for name in MIMI_ASAN MIMI_OPT MIMI_VALUE_GLUE; do
        if [[ -v $name ]]; then
            printf 'caller_original_%s=%q\n' "$name" "${!name}"
        else
            printf 'caller_original_%s=unset\n' "$name"
        fi
    done
    if [[ -v LD_PRELOAD ]]; then
        printf 'caller_original_LD_PRELOAD=%q\n' "$LD_PRELOAD"
    else
        printf 'caller_original_LD_PRELOAD=unset\n'
    fi
} > "$artifact_dir/mimi-environment.txt"
for name in "${caller_mimi_environment_names[@]}"; do
    if [ "$name" != MIMI_MAP_V2_IS_EMPTY_L3_ARTIFACT_DIR ]; then
        unset "$name"
    fi
done
unset LD_PRELOAD

source_snapshot_manifest() {
    (
        cd "$ROOT_DIR"
        {
            find src tests std -type f -print0
            if [ -d .cargo ]; then
                find .cargo -type f -print0
            fi
            is_empty_l3_cargo_config_home="${CARGO_HOME:-${HOME:-}/.cargo}"
            if [ -n "$is_empty_l3_cargo_config_home" ]; then
                for cargo_config in \
                    "$is_empty_l3_cargo_config_home/config" \
                    "$is_empty_l3_cargo_config_home/config.toml"; do
                    if [ -f "$cargo_config" ]; then
                        printf '%s\0' "$cargo_config"
                    fi
                done
            fi
            printf '%s\0' Cargo.toml Cargo.lock scripts/verify-ordinary-map-v2-is-empty-l3.sh
            if [ -f build.rs ]; then
                printf '%s\0' build.rs
            fi
        } | sort -zu | while IFS= read -r -d '' source_path; do
            sha256sum -- "$source_path"
        done
    )
}

cd "$ROOT_DIR"
if find src tests std .cargo -type l -print -quit 2>/dev/null | grep -q .; then
    printf 'IsEmpty L3 cannot snapshot symlinked source or Cargo configuration inputs\n' >&2
    exit 2
fi
source_snapshot_manifest > "$artifact_dir/source-snapshot.before.manifest.sha256"
sha256sum "$artifact_dir/source-snapshot.before.manifest.sha256" \
    | awk '{print $1}' > "$artifact_dir/source-snapshot.before.sha256"

test_name=ordinary_map_owned_any_v2_is_empty_default_and_explicit_native_builds_are_strict_memcheck_clean
cargo_arguments=(
    test
    --manifest-path "$ROOT_DIR/Cargo.toml"
    --test canonical_mir_ordinary_map_any_v2_is_empty_memcheck_l3
    "$test_name"
    --
    --exact
    --ignored
    --nocapture
    --test-threads=1
)
{
    printf 'LLVM_SYS_181_PREFIX=%q MIMI_MAP_V2_IS_EMPTY_L3_ARTIFACT_DIR=%q ' \
        "$LLVM_SYS_181_PREFIX" "$artifact_dir"
    printf '%q ' cargo "${cargo_arguments[@]}"
    printf '\n'
} > "$artifact_dir/suite.command.txt"
printf '%s\0' cargo "${cargo_arguments[@]}" > "$artifact_dir/suite.argv.nul"
{
    printf 'head='; git rev-parse HEAD
    printf 'branch='; git branch --show-current
    printf 'git_status_short_begin\n'; git status --short
    printf 'git_status_short_end\n'
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
    printf 'MIMI_MAP_V2_IS_EMPTY_L3_ARTIFACT_DIR=%s\n' "$artifact_dir"
    for name in CARGO_HOME CARGO_BUILD_JOBS CARGO_INCREMENTAL CARGO_TARGET_DIR RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER CC CXX AR CFLAGS CXXFLAGS CPPFLAGS LD_LIBRARY_PATH LD_PRELOAD VALGRIND_OPTS VALGRIND_LIB MIMI_TEST_FORCE_NO_Z3 PATH HOME TMPDIR LANG LC_ALL RUST_LOG; do
        if [[ -v $name ]]; then
            printf '%s=%q\n' "$name" "${!name}"
        else
            printf '%s=unset\n' "$name"
        fi
    done
    is_empty_l3_cargo_home="${CARGO_HOME:-${HOME:-}/.cargo}"
    if [ -n "$is_empty_l3_cargo_home" ] && [ -d "$is_empty_l3_cargo_home" ]; then
        for config in "$is_empty_l3_cargo_home/config" "$is_empty_l3_cargo_home/config.toml"; do
            if [ -f "$config" ]; then
                printf 'cargo_config='; realpath -- "$config"
                sha256sum -- "$config"
            fi
        done
    fi
    printf 'rustc_path=%s\n' "$(command -v rustc)"
    printf 'rustc_version='; rustc --version
    printf 'cargo_path=%s\n' "$(command -v cargo)"
    printf 'cargo_version='; cargo --version
    printf 'valgrind_path=%s\n' "$(command -v valgrind)"
    printf 'valgrind_version='; valgrind --version
} > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"

test_status=0
if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
    MIMI_MAP_V2_IS_EMPTY_L3_ARTIFACT_DIR="$artifact_dir" \
    cargo "${cargo_arguments[@]}" \
        > "$artifact_dir/suite.stdout" \
        2> "$artifact_dir/suite.stderr"; then
    test_status=0
else
    test_status=$?
fi
printf '%s\n' "$test_status" > "$artifact_dir/acceptance-test.exit"

source_snapshot_manifest > "$artifact_dir/source-snapshot.after.manifest.sha256"
sha256sum "$artifact_dir/source-snapshot.after.manifest.sha256" \
    | awk '{print $1}' > "$artifact_dir/source-snapshot.after.sha256"
snapshot_status=0
if ! cmp -s "$artifact_dir/source-snapshot.before.manifest.sha256" \
    "$artifact_dir/source-snapshot.after.manifest.sha256"; then
    snapshot_status=1
fi

validation_status=0
target_expected_stdout="$artifact_dir/expected-target-native-stdout.bin"
control_expected_stdout="$artifact_dir/expected-control-native-stdout.bin"
printf 'true\nfalse\n' > "$target_expected_stdout"
: > "$control_expected_stdout"
validate_memcheck_capture() {
    local memcheck_dir="$1"
    local expected_stdout_file="$2"
    local label="$3"
    local required
    for required in \
        "$memcheck_dir/valgrind.log" \
        "$memcheck_dir/memcheck-summary.txt" \
        "$memcheck_dir/process.status" \
        "$memcheck_dir/stdout.bin" \
        "$memcheck_dir/stderr.bin" \
        "$memcheck_dir/expected-native-result.txt" \
        "$memcheck_dir/expected-native-stdout.bin" \
        "$memcheck_dir/memcheck-environment.txt"; do
        if [ ! -f "$required" ]; then
            printf '%s Memcheck evidence is missing: %s\n' "$label" "$required" >&2
            validation_status=1
        fi
    done
    if [ ! -f "$memcheck_dir/process.status" ] || \
        ! cmp -s "$memcheck_dir/process.status" <(printf 'exit_code=0\nsuccess=true\n'); then
        printf '%s Memcheck process did not exit successfully\n' "$label" >&2
        validation_status=1
    fi
    if [ -f "$memcheck_dir/stderr.bin" ] && [ -s "$memcheck_dir/stderr.bin" ]; then
        printf '%s native stderr is not empty\n' "$label" >&2
        validation_status=1
    fi
    if [ -f "$memcheck_dir/expected-native-result.txt" ] && \
        ! grep -Fxq "expected_exit_code=0" "$memcheck_dir/expected-native-result.txt"; then
        printf '%s expected native exit code is not zero\n' "$label" >&2
        validation_status=1
    fi
    if [ -f "$memcheck_dir/expected-native-stdout.bin" ] && \
        ! cmp -s "$memcheck_dir/expected-native-stdout.bin" "$expected_stdout_file"; then
        printf '%s expected stdout artifact does not match the profile contract\n' "$label" >&2
        validation_status=1
    fi
    if [ -f "$memcheck_dir/stdout.bin" ] && \
        ! cmp -s "$memcheck_dir/stdout.bin" "$expected_stdout_file"; then
        printf '%s actual stdout does not match the profile contract\n' "$label" >&2
        validation_status=1
    fi
    if [ -f "$memcheck_dir/expected-native-result.txt" ]; then
        local expected_bytes actual_expected_bytes
        expected_bytes="$(wc -c < "$expected_stdout_file" | tr -d '[:space:]')"
        actual_expected_bytes="$(sed -n 's/^expected_stdout_bytes=//p' "$memcheck_dir/expected-native-result.txt")"
        if [ "$actual_expected_bytes" != "$expected_bytes" ]; then
            printf '%s expected stdout length is %s, wanted %s\n' \
                "$label" "$actual_expected_bytes" "$expected_bytes" >&2
            validation_status=1
        fi
    fi
    if [ -f "$memcheck_dir/memcheck-environment.txt" ]; then
        for expected in 'VALGRIND_OPTS=unset' 'VALGRIND_LIB=unset' \
            'command_line_only=true' 'default_suppressions=false'; do
            if ! grep -Fxq "$expected" "$memcheck_dir/memcheck-environment.txt"; then
                printf '%s Memcheck environment is not isolated (%s)\n' "$label" "$expected" >&2
                validation_status=1
            fi
        done
    fi
    local summary="$memcheck_dir/memcheck-summary.txt"
    if [ -f "$summary" ]; then
        local summary_rows valgrind_log raw_pids all_free_count in_use_count error_count heap_totals allocs frees leak_detail_count
        summary_rows="$(wc -l < "$summary" | tr -d '[:space:]')"
        if [ "$summary_rows" != 6 ] && [ "$summary_rows" != 3 ]; then
            printf '%s Memcheck summary must contain exactly six regular rows or three actual all-free rows\n' "$label" >&2
            validation_status=1
        fi
        summary_pids="$(sed -n 's/^==\([0-9][0-9]*\)== .*/\1/p' "$summary" | sort -u)"
        if [ "$(printf '%s\n' "$summary_pids" | sed '/^$/d' | wc -l | tr -d '[:space:]')" != 1 ]; then
            printf '%s Memcheck summary does not use one numeric process identity\n' "$label" >&2
            validation_status=1
        fi
        summary_pid="$(printf '%s\n' "$summary_pids" | sed -n '1p')"
        if [ "$summary_rows" = 6 ]; then
            for expected in \
                'definitely lost: 0 bytes in 0 blocks' \
                'indirectly lost: 0 bytes in 0 blocks' \
                'possibly lost: 0 bytes in 0 blocks' \
                'suppressed: 0 bytes in 0 blocks' \
                'ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)'; do
                count="$(grep -Fxc "==${summary_pid}== ${expected}" "$summary" || true)"
                if [ "$count" -ne 1 ]; then
                    printf '%s Memcheck summary must contain one exact zero row for %s (found %s)\n' \
                        "$label" "$expected" "$count" >&2
                    validation_status=1
                fi
            done
            reachable_count="$(grep -Fxc "==${summary_pid}== still reachable: 0 bytes in 0 blocks" "$summary" || true)"
            if [ "$reachable_count" -ne 1 ]; then
                printf '%s Memcheck summary must prove zero still-reachable bytes and blocks (found %s rows)\n' \
                    "$label" "$reachable_count" >&2
                validation_status=1
            fi
        elif [ "$summary_rows" = 3 ]; then
            for expected in \
                'in use at exit: 0 bytes in 0 blocks' \
                'All heap blocks were freed -- no leaks are possible' \
                'ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)'; do
                count="$(grep -Fxc "==${summary_pid}== ${expected}" "$summary" || true)"
                if [ "$count" -ne 1 ]; then
                    printf '%s all-free Memcheck evidence must contain one exact row for %s (found %s)\n' \
                        "$label" "$expected" "$count" >&2
                    validation_status=1
                fi
            done
            valgrind_log="$memcheck_dir/valgrind.log"
            if [ ! -s "$valgrind_log" ]; then
                printf '%s all-free Memcheck evidence has no raw Valgrind report\n' "$label" >&2
                validation_status=1
            else
                raw_pids="$(sed -n 's/^==\([0-9][0-9]*\)==.*/\1/p' "$valgrind_log" | sort -u)"
                if [ "$(printf '%s\n' "$raw_pids" | sed '/^$/d' | wc -l | tr -d '[:space:]')" != 1 ] || \
                    [ "$raw_pids" != "$summary_pid" ]; then
                    printf '%s raw all-free report does not use the summary process identity\n' "$label" >&2
                    validation_status=1
                fi
                in_use_count="$(grep -Ec "^==${summary_pid}==[[:space:]]+in use at exit: 0 bytes in 0 blocks$" "$valgrind_log" || true)"
                all_free_count="$(grep -Ec "^==${summary_pid}==[[:space:]]+All heap blocks were freed -- no leaks are possible$" "$valgrind_log" || true)"
                error_count="$(grep -Fxc "==${summary_pid}== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)" "$valgrind_log" || true)"
                heap_totals="$(sed -nE "s/^==${summary_pid}==[[:space:]]+total heap usage: ([0-9]+) allocs, ([0-9]+) frees, [0-9][0-9,]* bytes allocated$/\\1 \\2/p" "$valgrind_log")"
                if [ "$in_use_count" -ne 1 ] || [ "$all_free_count" -ne 1 ] || [ "$error_count" -ne 1 ]; then
                    printf '%s raw all-free report must contain one zero-in-use, all-free, and zero-error row\n' "$label" >&2
                    validation_status=1
                fi
                if [ "$(printf '%s\n' "$heap_totals" | sed '/^$/d' | wc -l | tr -d '[:space:]')" != 1 ]; then
                    printf '%s raw all-free report must contain one total heap usage row\n' "$label" >&2
                    validation_status=1
                else
                    read -r allocs frees <<< "$heap_totals"
                    if [ -z "$allocs" ] || [ "$allocs" != "$frees" ]; then
                        printf '%s raw all-free report allocation and free totals disagree\n' "$label" >&2
                        validation_status=1
                    fi
                fi
                leak_detail_count="$(grep -Ec "^==${summary_pid}==[[:space:]]+(LEAK SUMMARY:|definitely lost:|indirectly lost:|possibly lost:|still reachable:|suppressed:)| loss record " "$valgrind_log" || true)"
                if [ "$leak_detail_count" -ne 0 ]; then
                    printf '%s all-free report mixes in leak categories or detail records\n' "$label" >&2
                    validation_status=1
                fi
            fi
        fi
    fi
}
if [ "$test_status" -eq 0 ]; then
    summary="$artifact_dir/native/run-summary.txt"
    if [ ! -s "$summary" ]; then
        printf 'successful IsEmpty L3 test left no run summary\n' >&2
        validation_status=1
    else
        for expected in \
            'route_profile=map-any-owner-v2-is-empty-v1' \
            'control_route_profile=map-any-owner-v2-map-set-drop-control-v1' \
            'global_map_any_v2_production_ready=false' \
            'case_count=1' \
            'native_build_count=3' \
            'control_native_build_count=1' \
            'memcheck_run_count=3' \
            'control_memcheck_run_count=1' \
            'default_route_receipt_count=1' \
            'control_route_receipt_count=1' \
            'explicit_mir_census_count=1' \
            'selected_payload_type_count=1' \
            'payload_type_selection_equal=true' \
            'route_graphs_distinct=true' \
            'legacy_fallback_count=0'; do
            if ! grep -Fxq "$expected" "$summary"; then
                printf 'IsEmpty L3 summary lacks %s\n' "$expected" >&2
                validation_status=1
            fi
        done
    fi
    for mode in default explicit-mir; do
        case_dir="$artifact_dir/native/$mode"
        for required in \
            "$case_dir/build/program" \
            "$case_dir/build/stderr.bin" \
            "$case_dir/build/process.status"; do
            if [ ! -s "$required" ]; then
                printf 'IsEmpty L3 evidence is missing or empty: %s\n' "$required" >&2
                validation_status=1
            fi
        done
        validate_memcheck_capture "$case_dir/memcheck" "$target_expected_stdout" "$mode IsEmpty target"
    done
    control_dir="$artifact_dir/native/control-map-set-drop"
    for required in \
        "$control_dir/build/program" \
        "$control_dir/build/stderr.bin" \
        "$control_dir/build/process.status" \
        "$control_dir/route-identity.txt" \
        "$artifact_dir/native/still-reachable-comparison.txt"; do
        if [ ! -s "$required" ]; then
            printf 'IsEmpty L3 control evidence is missing or empty: %s\n' "$required" >&2
            validation_status=1
        fi
    done
    validate_memcheck_capture "$control_dir/memcheck" "$control_expected_stdout" "MapSetDropControl"
    comparison="$artifact_dir/native/still-reachable-comparison.txt"
    if [ -f "$comparison" ]; then
        for expected in \
            'target_route_profile=map-any-owner-v2-is-empty-v1' \
            'control_route_profile=map-any-owner-v2-map-set-drop-control-v1' \
            'payload_type_selection_equal=true' \
            'payload_type_desc_shape_equal_ignoring_source_field_node_ids=true' \
            'route_graphs_distinct=true' \
            'control_reachable_bytes=0' \
            'control_reachable_blocks=0' \
            'default_added_reachable_bytes=0' \
            'default_added_reachable_blocks=0' \
            'explicit_added_reachable_bytes=0' \
            'explicit_added_reachable_blocks=0'; do
            if ! grep -Fxq "$expected" "$comparison"; then
                printf 'IsEmpty L3 reachable-memory comparison lacks %s\n' "$expected" >&2
                validation_status=1
            fi
        done
    fi
    target_identity="$artifact_dir/native/route-control.txt"
    control_identity="$control_dir/route-identity.txt"
    for identity in "$target_identity" "$control_identity"; do
        if [ ! -s "$identity" ]; then
            printf 'IsEmpty L3 route identity evidence is missing or empty: %s\n' "$identity" >&2
            validation_status=1
        fi
    done
    if [ -s "$target_identity" ] && [ -s "$control_identity" ]; then
        if ! grep -Fxq 'profile=map-any-owner-v2-is-empty-v1' "$target_identity"; then
            printf 'target route identity does not bind the IsEmpty profile\n' >&2
            validation_status=1
        fi
        if ! grep -Fxq 'route_profile=map-any-owner-v2-map-set-drop-control-v1' "$control_identity"; then
            printf 'control route identity does not bind MapSetDropControl\n' >&2
            validation_status=1
        fi
        target_type_count="$(sed -n 's/^selected_type_count=//p' "$target_identity")"
        control_type_count="$(sed -n 's/^selected_type_count=//p' "$control_identity")"
        target_type_id="$(sed -n 's/^selected_type_id=//p' "$target_identity")"
        control_type_id="$(sed -n 's/^selected_type_id=//p' "$control_identity")"
        if [ "$target_type_count" != 1 ] || [ "$control_type_count" != 1 ] \
            || [ -z "$target_type_id" ] || [ "$target_type_id" != "$control_type_id" ]; then
            printf 'IsEmpty and MapSetDropControl must select one identical payload TypeId\n' >&2
            validation_status=1
        fi
        target_type_desc_digest="$(sed -n 's/^selected_type_desc_digest=//p' "$target_identity")"
        control_type_desc_digest="$(sed -n 's/^selected_type_desc_digest=//p' "$control_identity")"
        for digest in "$target_type_desc_digest" "$control_type_desc_digest"; do
            if [ "${#digest}" -ne 64 ] || [[ "$digest" == *[!0-9a-f]* ]]; then
                printf 'IsEmpty and MapSetDropControl must retain valid selected payload TypeDesc digests\n' >&2
                validation_status=1
            fi
        done
        for expected in \
            "target_payload_type_id=$target_type_id" \
            "control_payload_type_id=$control_type_id" \
            "target_payload_type_desc_digest=$target_type_desc_digest" \
            "control_payload_type_desc_digest=$control_type_desc_digest"; do
            if ! grep -Fxq "$expected" "$comparison"; then
                printf 'IsEmpty L3 comparison does not bind %s\n' "$expected" >&2
                validation_status=1
            fi
        done
        target_mir="$(sed -n 's/^mir_digest=//p' "$target_identity")"
        control_mir="$(sed -n 's/^mir_digest=//p' "$control_identity")"
        target_receipt="$(sed -n 's/^receipt_identity_digest=//p' "$target_identity")"
        control_receipt="$(sed -n 's/^receipt_identity_digest=//p' "$control_identity")"
        if [ -z "$target_mir" ] || [ "$target_mir" = "$control_mir" ] \
            || [ -z "$target_receipt" ] || [ "$target_receipt" = "$control_receipt" ]; then
            printf 'IsEmpty and MapSetDropControl must carry distinct MIR and route receipts\n' >&2
            validation_status=1
        fi
    fi
fi

acceptance_status="$test_status"
if [ "$snapshot_status" -ne 0 ] && [ "$acceptance_status" -eq 0 ]; then
    acceptance_status=97
fi
if [ "$validation_status" -ne 0 ] && [ "$acceptance_status" -eq 0 ]; then
    acceptance_status=97
fi
if [ "$acceptance_status" -eq 0 ]; then
    printf 'PASS exact IsEmpty and independent MapSetDropControl route receipts, matching payload TypeId/TypeDesc, distinct MIR graphs, exact outputs, and zero retained memory under strict Memcheck\n' \
        > "$artifact_dir/artifact-validation.txt"
else
    printf 'INCOMPLETE/FAIL test_exit=%s source_snapshot_status=%s evidence_validation_status=%s\n' \
        "$test_status" "$snapshot_status" "$validation_status" \
        > "$artifact_dir/artifact-validation.txt"
fi

if [ -e "$artifact_dir/files.sha256" ] || [ -e "$artifact_dir/files.sha256.sha256" ]; then
    printf 'refusing to overwrite existing IsEmpty L3 checksum files\n' >&2
    exit 2
fi
(
    cd "$artifact_dir"
    find . -type f ! -name files.sha256 ! -name files.sha256.sha256 -print0 \
        | sort -z | xargs -0 sha256sum > files.sha256
    sha256sum --check files.sha256
    sha256sum files.sha256 > files.sha256.sha256
)

printf 'acceptance_test_exit=%s\n' "$acceptance_status"
exit "$acceptance_status"
