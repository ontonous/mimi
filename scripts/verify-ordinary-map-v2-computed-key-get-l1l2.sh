#!/usr/bin/env bash
set -euo pipefail
set -o pipefail
set -o noclobber

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
FIXTURE=tests/fixtures/mir_ordinary_map_any_owner_v2_computed_key_get.mimi
CLI_TEST_SOURCE=tests/canonical_mir_ordinary_map_any_owner_v2_cli.rs
CLI_TEST_NAME=map_any_owner_v2_exact_computed_key_get_uses_its_production_profile
REFERENCE_TEST_SOURCE=tests/canonical_mir_ordinary_map_any_v2_computed_key_get_reference.rs
REFERENCE_TEST_NAME=computed_key_get_materializes_its_receipt_and_executes_in_reference_mir
ARTIFACT_ENV=MIMI_MAP_V2_COMPUTED_KEY_GET_L1L2_ARTIFACT_DIR

if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper
fi

for tool in bash cargo rustc sha256sum realpath git mktemp tee find sort wc; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'ComputedKeyGet L1/L2 capture requires %s\n' "$tool" >&2
        exit 1
    }
done
[ -d "$LLVM_SYS_181_PREFIX" ] || {
    printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
}

CARGO_BIN="$(command -v cargo)"
RUSTC_BIN="$(command -v rustc)"
BASH_BIN="$(command -v bash)"
if [ -x "$LLVM_SYS_181_PREFIX/bin/llvm-config" ]; then
    LLVM_CONFIG_BIN="$LLVM_SYS_181_PREFIX/bin/llvm-config"
elif [ -x "$LLVM_SYS_181_PREFIX/llvm-config" ]; then
    LLVM_CONFIG_BIN="$LLVM_SYS_181_PREFIX/llvm-config"
elif command -v llvm-config >/dev/null 2>&1; then
    LLVM_CONFIG_BIN="$(command -v llvm-config)"
else
    printf 'llvm-config is unavailable under LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi

if [[ -v "$ARTIFACT_ENV" && -n "${!ARTIFACT_ENV}" ]]; then
    requested="${!ARTIFACT_ENV}"
else
    requested="${TMPDIR:-/tmp}/mimi-map-v2-computed-key-get-l1l2-$(date -u +%Y%m%dT%H%M%SZ)-$$"
fi
case "$requested" in
    /*) ;;
    *) requested="$ROOT_DIR/$requested" ;;
esac
if [ -e "$requested" ] || [ -L "$requested" ]; then
    printf 'refusing to overwrite existing ComputedKeyGet L1/L2 artifact path: %s\n' "$requested" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/scripts/"*|"$ROOT_DIR/target/"*)
        printf 'ComputedKeyGet L1/L2 artifacts must be outside source/build trees: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
mkdir -p -- "$(dirname "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
unset "$ARTIFACT_ENV"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/mimi-map-v2-computed-key-get-l1l2.XXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT

write_source_manifest() {
    local destination="$1"
    local errors_file="${destination%.manifest.sha256}.errors.txt"
    local base="$(basename "$destination")"
    local paths="$scratch/$base.paths.nul"
    local sorted_paths="$scratch/$base.sorted.paths.nul"
    local status=0
    local path_bytes observed_path_bytes=0 input
    local -a source_paths=()
    local LC_ALL=C
    if ! : > "$destination" || ! : > "$errors_file" || ! : > "$paths"; then
        printf 'source_manifest_initialization_failed=%s\n' "$destination" >&2
        return 1
    fi
    for source_dir in src tests std; do
        if [ ! -d "$source_dir" ]; then
            printf 'missing_source_directory=%s\n' "$source_dir" >> "$errors_file"
            status=1
            continue
        fi
        if ! find "$source_dir" \( -type f -o -type l \) -print0 >> "$paths" 2>> "$errors_file"; then
            printf 'source_enumeration_failed=%s\n' "$source_dir" >> "$errors_file"
            status=1
        fi
    done
    for input in \
        "$CLI_TEST_SOURCE" "$REFERENCE_TEST_SOURCE" "$FIXTURE" \
        scripts/verify-ordinary-map-v2-computed-key-get-l1l2.sh Cargo.toml; do
        if [ ! -f "$input" ]; then
            printf 'required_source_missing=%s\n' "$input" >> "$errors_file"
            status=1
        else
            if ! printf '%s\0' "$input" >> "$paths"; then
                printf 'source_path_write_failed=%s\n' "$input" >> "$errors_file"
                status=1
            fi
        fi
    done
    for input in Cargo.lock build.rs rust-toolchain rust-toolchain.toml .cargo/config .cargo/config.toml; do
        if [ -e "$input" ] && [ ! -f "$input" ]; then
            printf 'optional_source_not_regular_file=%s\n' "$input" >> "$errors_file"
            status=1
        elif [ -f "$input" ]; then
            if ! printf '%s\0' "$input" >> "$paths"; then
                printf 'source_path_write_failed=%s\n' "$input" >> "$errors_file"
                status=1
            fi
        fi
    done
    if ! sort -zu "$paths" -o "$sorted_paths" 2>> "$errors_file"; then
        printf 'source_path_sort_failed\n' >> "$errors_file"
        status=1
        return "$status"
    fi
    if ! mapfile -d '' -t source_paths < "$sorted_paths"; then
        printf 'source_path_manifest_read_failed=%s\n' "$sorted_paths" >> "$errors_file"
        status=1
    fi
    if ! path_bytes="$(wc -c < "$sorted_paths")"; then
        printf 'source_path_manifest_size_failed=%s\n' "$sorted_paths" >> "$errors_file"
        status=1
    else
        path_bytes="${path_bytes//[[:space:]]/}"
        if [[ ! "$path_bytes" =~ ^[0-9]+$ ]]; then
            printf 'source_path_manifest_size_malformed=%s\n' "$sorted_paths" >> "$errors_file"
            status=1
        else
            for input in "${source_paths[@]}"; do
                observed_path_bytes=$((observed_path_bytes + ${#input} + 1))
            done
            if [[ "$observed_path_bytes" -ne "$path_bytes" ]]; then
                printf 'source_path_manifest_truncated=%s expected_bytes=%s observed_bytes=%s\n' \
                    "$sorted_paths" "$path_bytes" "$observed_path_bytes" >> "$errors_file"
                status=1
            fi
        fi
    fi
    for input in "${source_paths[@]}"; do
        if [ ! -f "$input" ]; then
            printf 'source_disappeared_before_hash=%s\n' "$input" >> "$errors_file"
            status=1
        elif ! sha256sum -- "$input" >> "$destination" 2>> "$errors_file"; then
            printf 'source_hash_failed=%s\n' "$input" >> "$errors_file"
            status=1
        fi
    done
    return "$status"
}

is_sha256() {
    [[ "$1" =~ ^[0-9a-f]{64}$ ]]
}

write_route_profile_summary() {
    local source_log="$1"
    local destination="$2"
    local rows="$scratch/computed-key-get-route-summary.lines"
    local errors_file="$artifact_dir/route-profile-summary.errors.txt"
    local status=0
    local line path profile digest expected_path known_path observed_digest=''
    local source_digest='' source_line
    local -A observed_paths=()
    local expected_paths=(
        default-run explicit-mir-run
        default-build explicit-mir-build
        default-verify explicit-mir-verify
    )
    if ! : > "$destination" || ! : > "$errors_file"; then
        printf 'route_summary_initialization_failed=%s\n' "$destination" >&2
        return 1
    fi
    local source_rows="$scratch/computed-key-get-source-summary.lines"
    if ! grep '^MIMI_COMPUTED_KEY_GET_L1L2_SOURCE ' "$source_log" > "$source_rows"; then
        : > "$source_rows"
        printf 'source_receipt_row_missing=true\n' >> "$errors_file"
        status=1
    fi
    if [[ "$(wc -l < "$source_rows")" -ne 1 ]]; then
        printf 'expected_exactly_one_source_receipt_row=true\n' >> "$errors_file"
        status=1
    fi
    while IFS= read -r source_line; do
        if [[ ! "$source_line" =~ ^MIMI_COMPUTED_KEY_GET_L1L2_SOURCE\ profile=cli-mir-v1\ mir_digest=([0-9a-f]{64})$ ]]; then
            printf 'malformed_source_receipt_row=%s\n' "$source_line" >> "$errors_file"
            status=1
            continue
        fi
        if [[ -n "$source_digest" ]]; then
            printf 'duplicate_source_receipt_row=true\n' >> "$errors_file"
            status=1
        fi
        source_digest="${BASH_REMATCH[1]}"
    done < "$source_rows"
    local prefixed_route_count=0
    local test_prefix="test $CLI_TEST_NAME ..."
    local observed_receipt_identity_digest=""
    if ! grep 'MIMI_COMPUTED_KEY_GET_L1L2_ROUTE ' "$source_log" > "$rows"; then
        : > "$rows"
        printf 'route_summary_rows_missing=true\n' >> "$errors_file"
        status=1
    fi
    if [[ "$(wc -l < "$rows")" -ne 6 ]]; then
        printf 'expected_exactly_six_route_rows=true\n' >> "$errors_file"
        status=1
    fi
    while IFS= read -r line; do
        if [[ "$line" == "$test_prefix MIMI_COMPUTED_KEY_GET_L1L2_ROUTE "* ]]; then
            line="${line#"$test_prefix "}"
            prefixed_route_count=$((prefixed_route_count + 1))
        fi
        if [[ ! "$line" =~ ^MIMI_COMPUTED_KEY_GET_L1L2_ROUTE\ path=([^[:space:]]+)\ profile=([^[:space:]]+)\ mir_digest=([0-9a-f]{64})\ receipt_identity_digest=([0-9a-f]{64})\ no_legacy_fallback=true$ ]]; then
            printf 'malformed_route_summary_row=%s\n' "$line" >> "$errors_file"
            status=1
            continue
        fi
        path="${BASH_REMATCH[1]}"
        profile="${BASH_REMATCH[2]}"
        digest="${BASH_REMATCH[3]}"
        receipt_identity_digest="${BASH_REMATCH[4]}"
        known_path=0
        for expected_path in "${expected_paths[@]}"; do
            if [[ "$path" == "$expected_path" ]]; then
                known_path=1
                break
            fi
        done
        if [[ "$known_path" -eq 0 ]]; then
            printf 'unexpected_route_path=%s\n' "$path" >> "$errors_file"
            status=1
            continue
        fi
        if [[ "$profile" != map-any-owner-v2-computed-key-get-v1 ]]; then
            printf 'unexpected_route_profile=%s\n' "$profile" >> "$errors_file"
            status=1
        fi
        if [[ -n "$observed_digest" && "$observed_digest" != "$digest" ]]; then
            printf 'route_digest_mismatch=%s\n' "$line" >> "$errors_file"
            status=1
        fi
        if [[ -n "$observed_receipt_identity_digest" && "$observed_receipt_identity_digest" != "$receipt_identity_digest" ]]; then
            printf 'route_receipt_identity_digest_mismatch=%s expected=%s\n' \
                "$line" "$observed_receipt_identity_digest" >> "$errors_file"
            status=1
        fi
        if [[ -z "$source_digest" || "$source_digest" != "$digest" ]]; then
            printf 'route_source_digest_mismatch=%s source_digest=%s\n' \
                "$line" "$source_digest" >> "$errors_file"
            status=1
        fi
        observed_digest="$digest"
        observed_receipt_identity_digest="$receipt_identity_digest"
        if [[ -n "${observed_paths[$path]+present}" ]]; then
            printf 'duplicate_route_path=%s\n' "$path" >> "$errors_file"
            status=1
        fi
        observed_paths[$path]=1
        if ! printf 'MIMI_COMPUTED_KEY_GET_L1L2_ROUTE path=%s profile=%s mir_digest=%s receipt_identity_digest=%s no_legacy_fallback=true\n' \
            "$path" "$profile" "$digest" "$receipt_identity_digest" >> "$destination"; then
            printf 'route_summary_write_failed=%s\n' "$path" >> "$errors_file"
            status=1
        fi
    done < "$rows"
    if [[ "$prefixed_route_count" -ne 1 ]]; then
        printf 'expected_exactly_one_test_prefixed_route_row=true observed=%s\n' \
            "$prefixed_route_count" >> "$errors_file"
        status=1
    fi
    if grep -Fq 'canonical route disposition: legacy' "$source_log"; then
        printf 'legacy_route_disposition_observed=true\n' >> "$errors_file"
        status=1
    else
        grep_status=$?
        if [[ "$grep_status" -gt 1 ]]; then
            printf 'legacy_route_disposition_scan_failed=true\n' >> "$errors_file"
            status=1
        fi
    fi
    for path in "${expected_paths[@]}"; do
        if [[ -z "${observed_paths[$path]+present}" ]]; then
            printf 'missing_route_path=%s\n' "$path" >> "$errors_file"
            status=1
        fi
    done
    return "$status"
}

write_vcs_identity() {
    local destination="$1"
    (
        cd "$ROOT_DIR"
        printf 'branch='
        git branch --show-current
        printf 'head='
        git rev-parse HEAD
    ) > "$destination"
}

capture_tool() {
    local label="$1"
    local executable="$2"
    shift 2
    local directory="$artifact_dir/toolchain/$label"
    local status=0
    mkdir -- "$directory"
    {
        printf '%q' "$executable"
        printf ' %q' "$@"
        printf '\n'
    } > "$directory/command.txt"
    printf '%s\0' "$executable" "$@" > "$directory/command.argv.nul"
    if "$executable" "$@" > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
        status=0
    else
        status=$?
        toolchain_status=1
    fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$directory/process.status"
}

cd "$ROOT_DIR"
validation_errors=()
manifest_before_status=0
write_vcs_identity "$artifact_dir/vcs.before.txt"
git status --short --untracked-files=no > "$artifact_dir/git-status.before.txt"
write_source_manifest "$artifact_dir/compilation-inputs.before.manifest.sha256" || manifest_before_status=$?
snapshot_before=''
if manifest_hash_line="$(sha256sum -- "$artifact_dir/compilation-inputs.before.manifest.sha256" 2>> "$artifact_dir/compilation-inputs.before.errors.txt")"; then
    snapshot_before="${manifest_hash_line%% *}"
    if ! is_sha256 "$snapshot_before"; then
        manifest_before_status=1
        printf 'manifest_hash_malformed=compilation-inputs.before.manifest.sha256\n' >> "$artifact_dir/compilation-inputs.before.errors.txt"
    fi
else
    manifest_before_status=1
    printf 'manifest_hash_failed=compilation-inputs.before.manifest.sha256\n' >> "$artifact_dir/compilation-inputs.before.errors.txt"
fi
printf '%s\n' "$snapshot_before" > "$artifact_dir/compilation-inputs.before.sha256"
fixture_before_status=0
if ! sha256sum -- "$FIXTURE" > "$artifact_dir/fixture.before.sha256" 2> "$artifact_dir/fixture.before.errors.txt"; then
    fixture_before_status=1
fi

mkdir -- "$artifact_dir/toolchain"
toolchain_status=0
capture_tool bash "$BASH_BIN" --version
capture_tool rustc "$RUSTC_BIN" --version --verbose
capture_tool cargo "$CARGO_BIN" --version --verbose
capture_tool llvm-config "$LLVM_CONFIG_BIN" --version --prefix --libdir --host-target
sha256sum -- "$BASH_BIN" "$RUSTC_BIN" "$CARGO_BIN" "$LLVM_CONFIG_BIN" \
    > "$artifact_dir/toolchain/binaries.sha256"
{
    printf 'LLVM_SYS_181_PREFIX=%q\n' "$LLVM_SYS_181_PREFIX"
    printf 'CARGO_BUILD_JOBS=1\n'
    printf 'bash_path=%q\n' "$BASH_BIN"
    printf 'rustc_path=%q\n' "$RUSTC_BIN"
    printf 'cargo_path=%q\n' "$CARGO_BIN"
    printf 'llvm_config_path=%q\n' "$LLVM_CONFIG_BIN"
    for variable in PATH RUSTC RUSTUP_TOOLCHAIN RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_HOME CARGO_TARGET_DIR LIBCLANG_PATH CC CXX CFLAGS LDFLAGS MIMI_ROUTE_CENSUS MIMI_VERBOSE; do
        if [[ -v "$variable" ]]; then
            printf '%s=%q\n' "$variable" "${!variable}"
        else
            printf '%s=<unset>\n' "$variable"
        fi
    done
} > "$artifact_dir/toolchain/environment.txt"

run_cargo_case() {
    local case_name="$1"
    shift
    local case_dir="$artifact_dir/$case_name"
    local status=0
    local -a cargo_arguments=("$@")
    mkdir -- "$case_dir"
    {
        printf 'cd %q && env LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 %q' \
            "$ROOT_DIR" "$LLVM_SYS_181_PREFIX" "$CARGO_BIN"
        printf ' %q' "${cargo_arguments[@]}"
        printf '\n'
    } > "$case_dir/command.txt"
    printf '%s\0' "$CARGO_BIN" "${cargo_arguments[@]}" > "$case_dir/command.argv.nul"
    {
        printf 'cwd=%s\nLLVM_SYS_181_PREFIX=%s\nCARGO_BUILD_JOBS=1\n' \
            "$ROOT_DIR" "$LLVM_SYS_181_PREFIX"
        cat "$artifact_dir/toolchain/environment.txt"
    } > "$case_dir/command.environment.txt"
    if (
        cd "$ROOT_DIR"
        env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" CARGO_BUILD_JOBS=1 \
            "$CARGO_BIN" "${cargo_arguments[@]}"
    ) 2>&1 | tee "$case_dir/command.log"; then
        status=0
    else
        status=$?
    fi
    printf '%s\n' "$status" > "$case_dir/command.exit"
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" \
        > "$case_dir/process.status"
}

run_cargo_case library \
    test --lib computed_key_get -- --nocapture --test-threads=1
run_cargo_case reference \
    test --test canonical_mir_ordinary_map_any_v2_computed_key_get_reference \
    "$REFERENCE_TEST_NAME" -- --exact --nocapture --test-threads=1
run_cargo_case cli-exact \
    test --test canonical_mir_ordinary_map_any_owner_v2_cli \
    "$CLI_TEST_NAME" -- --exact --nocapture --test-threads=1

manifest_after_status=0
write_source_manifest "$artifact_dir/compilation-inputs.after.manifest.sha256" || manifest_after_status=$?
snapshot_after=''
if manifest_hash_line="$(sha256sum -- "$artifact_dir/compilation-inputs.after.manifest.sha256" 2>> "$artifact_dir/compilation-inputs.after.errors.txt")"; then
    snapshot_after="${manifest_hash_line%% *}"
    if ! is_sha256 "$snapshot_after"; then
        manifest_after_status=1
        printf 'manifest_hash_malformed=compilation-inputs.after.manifest.sha256\n' >> "$artifact_dir/compilation-inputs.after.errors.txt"
    fi
else
    manifest_after_status=1
    printf 'manifest_hash_failed=compilation-inputs.after.manifest.sha256\n' >> "$artifact_dir/compilation-inputs.after.errors.txt"
fi
printf '%s\n' "$snapshot_after" > "$artifact_dir/compilation-inputs.after.sha256"
fixture_after_status=0
if ! sha256sum -- "$FIXTURE" > "$artifact_dir/fixture.after.sha256" 2> "$artifact_dir/fixture.after.errors.txt"; then
    fixture_after_status=1
fi
write_vcs_identity "$artifact_dir/vcs.after.txt"
git status --short --untracked-files=no > "$artifact_dir/git-status.after.txt"

if [ "$manifest_before_status" -ne 0 ] || [ "$manifest_after_status" -ne 0 ]; then
    validation_errors+=("source enumeration or hashing failed; see compilation-inputs.*.errors.txt")
fi
if [ "$fixture_before_status" -ne 0 ] || [ "$fixture_after_status" -ne 0 ]; then
    validation_errors+=("ComputedKeyGet fixture hashing failed")
fi
route_summary_status=0
if [ "$(cat "$artifact_dir/cli-exact/command.exit")" -eq 0 ]; then
    write_route_profile_summary \
        "$artifact_dir/cli-exact/command.log" \
        "$artifact_dir/route-profile-summary.txt" || route_summary_status=$?
else
    : > "$artifact_dir/route-profile-summary.txt"
    route_summary_status=1
fi
if [ "$route_summary_status" -ne 0 ]; then
    validation_errors+=("exact CLI test did not provide six valid ComputedKeyGet profile/digest/no-fallback rows")
fi
if ! cmp -s \
    "$artifact_dir/compilation-inputs.before.manifest.sha256" \
    "$artifact_dir/compilation-inputs.after.manifest.sha256"; then
    validation_errors+=("compilation inputs changed during capture")
fi
if ! cmp -s "$artifact_dir/fixture.before.sha256" "$artifact_dir/fixture.after.sha256"; then
    validation_errors+=("ComputedKeyGet fixture changed during capture")
fi
if ! cmp -s "$artifact_dir/vcs.before.txt" "$artifact_dir/vcs.after.txt"; then
    validation_errors+=("branch or HEAD changed during capture")
fi
if [ "$toolchain_status" -ne 0 ]; then
    validation_errors+=("a shell or toolchain version capture failed")
fi

if [ "$(cat "$artifact_dir/library/command.exit")" -ne 0 ]; then
    validation_errors+=("cargo test --lib computed_key_get failed")
elif ! grep -Eq '^running [1-9][0-9]* tests?$' "$artifact_dir/library/command.log" || \
    ! grep -Fq 'test result: ok.' "$artifact_dir/library/command.log"; then
    validation_errors+=("library filter did not produce a positive passing test summary")
fi
if [ "$(cat "$artifact_dir/reference/command.exit")" -ne 0 ]; then
    validation_errors+=("exact ComputedKeyGet Reference MIR test failed")
elif ! grep -Fq "test $REFERENCE_TEST_NAME ... ok" "$artifact_dir/reference/command.log"; then
    validation_errors+=("exact ComputedKeyGet Reference MIR test did not report a passing test")
fi
if [ "$(cat "$artifact_dir/cli-exact/command.exit")" -ne 0 ]; then
    validation_errors+=("exact ComputedKeyGet CLI test failed")
elif [[ "$(grep -Foc "test $CLI_TEST_NAME ..." "$artifact_dir/cli-exact/command.log")" -ne 1 ]] || \
    [[ "$(grep -Ec '^test result:' "$artifact_dir/cli-exact/command.log")" -ne 1 ]] || \
    [[ "$(grep -Ec '^test result: ok\. 1 passed; 0 failed; [0-9]+ ignored; ([0-9]+ measured; )?[0-9]+ filtered out; finished in [0-9]+([.][0-9]+)?s$' \
        "$artifact_dir/cli-exact/command.log")" -ne 1 ]]; then
    validation_errors+=("exact ComputedKeyGet CLI test name and single-test passing summary were not both present")
fi

if [ "${#validation_errors[@]}" -eq 0 ]; then
    printf 'validation=PASS\nprofile=ComputedKeyGet\nlevel=L1/L2\nglobal_map_any_v2_production_ready=false\nreference_test=%s\nroute_profile_summary=%s\n' \
        "$REFERENCE_TEST_NAME" \
        "$artifact_dir/route-profile-summary.txt" \
        > "$artifact_dir/artifact-validation.txt"
else
    {
        printf 'validation=FAIL\nprofile=ComputedKeyGet\nlevel=L1/L2\nglobal_map_any_v2_production_ready=false\nreference_test=%s\n' \
            "$REFERENCE_TEST_NAME"
        for error in "${validation_errors[@]}"; do
            printf 'reason=%s\n' "$error"
        done
    } > "$artifact_dir/artifact-validation.txt"
fi

(
    cd "$artifact_dir"
    find . -type f ! -name artifacts.sha256 ! -name artifacts.sha256.sha256 -print0 \
        | sort -z | xargs -0 -r sha256sum > artifacts.sha256
    sha256sum --check artifacts.sha256
    sha256sum artifacts.sha256 > artifacts.sha256.sha256
)

printf 'artifact_dir=%s\n' "$artifact_dir"
if [ "${#validation_errors[@]}" -ne 0 ]; then
    cat "$artifact_dir/artifact-validation.txt" >&2
    exit 1
fi
cat "$artifact_dir/artifact-validation.txt"
cat "$artifact_dir/route-profile-summary.txt"
