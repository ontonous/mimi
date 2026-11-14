#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
requested_artifact_dir="${MIMI_MAP_V2_KEYS_SNAPSHOT_L3_ARTIFACT_DIR:?set a fresh evidence directory}"

if ! command -v valgrind >/dev/null 2>&1; then
    printf 'KeysSnapshot L3 requires Valgrind\n' >&2
    exit 1
fi
if [ ! -d "$LLVM_SYS_181_PREFIX" ]; then
    printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi

case "$requested_artifact_dir" in
    /*) ;;
    *) requested_artifact_dir="$ROOT_DIR/$requested_artifact_dir" ;;
esac
if [ -e "$requested_artifact_dir" ] || [ -L "$requested_artifact_dir" ]; then
    printf 'refusing to overwrite KeysSnapshot L3 evidence: %s\n' "$requested_artifact_dir" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested_artifact_dir")"
artifact_relative="${artifact_dir#"$ROOT_DIR"/}"
case "$artifact_relative" in
    src|src/*|tests|tests/*|std|std/*)
        printf 'KeysSnapshot L3 evidence must be outside source trees: %s\n' "$artifact_dir" >&2
        exit 2
        ;;
esac
mkdir -p "$(dirname "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"

cd "$ROOT_DIR"
source_snapshot_manifest() {
    {
        find src tests std -type f -print0
        printf '%s\0' Cargo.toml Cargo.lock scripts/verify-ordinary-map-owned-any-v2-keys-snapshot-l3.sh
        if [ -f build.rs ]; then
            printf '%s\0' build.rs
        fi
    } | sort -zu | xargs -0 sha256sum
}

source_snapshot_manifest > "$artifact_dir/source-snapshot.before.manifest.sha256"
source_snapshot_before="$(sha256sum "$artifact_dir/source-snapshot.before.manifest.sha256" | awk '{print $1}')"
printf '%s\n' "$source_snapshot_before" > "$artifact_dir/source-snapshot.before.sha256"

test_name=ordinary_map_owned_any_v2_keys_snapshot_default_and_explicit_builds_are_strict_memcheck_clean
cargo_arguments=(
    test
    --manifest-path "$ROOT_DIR/Cargo.toml"
    --test canonical_mir_ordinary_map_any_owner_v2_memcheck_l3
    "$test_name"
    --
    --exact
    --ignored
    --nocapture
    --test-threads=1
)
{
    printf 'LLVM_SYS_181_PREFIX=%q MIMI_REQUIRE_VALGRIND=1 MIMI_MAP_V2_L3_ARTIFACT_DIR=%q ' \
        "$LLVM_SYS_181_PREFIX" "$artifact_dir"
    printf '%q ' cargo "${cargo_arguments[@]}"
    printf '\n'
} > "$artifact_dir/command.txt"
printf '%s\0' cargo "${cargo_arguments[@]}" > "$artifact_dir/command.argv.nul"
{
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
    printf 'MIMI_REQUIRE_VALGRIND=1\n'
    printf 'MIMI_MAP_V2_L3_ARTIFACT_DIR=%s\n' "$artifact_dir"
    printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR:-<default>}"
    printf 'rustc_path=%s\n' "$(command -v rustc)"
    printf 'rustc_version='; rustc --version
    printf 'cargo_path=%s\n' "$(command -v cargo)"
    printf 'cargo_version='; cargo --version
    printf 'valgrind_path=%s\n' "$(command -v valgrind)"
    printf 'valgrind_version='; valgrind --version
} > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"

if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
    MIMI_REQUIRE_VALGRIND=1 \
    MIMI_MAP_V2_L3_ARTIFACT_DIR="$artifact_dir" \
    cargo "${cargo_arguments[@]}" > "$artifact_dir/test.stdout" 2> "$artifact_dir/test.stderr"; then
    test_status=0
else
    test_status=$?
fi
printf '%s\n' "$test_status" > "$artifact_dir/acceptance-test.exit"

source_snapshot_manifest > "$artifact_dir/source-snapshot.after.manifest.sha256"
source_snapshot_after="$(sha256sum "$artifact_dir/source-snapshot.after.manifest.sha256" | awk '{print $1}')"
printf '%s\n' "$source_snapshot_after" > "$artifact_dir/source-snapshot.after.sha256"

validation_status=0
if [ "$source_snapshot_before" != "$source_snapshot_after" ]; then
    printf 'source tree changed during KeysSnapshot L3\n' >&2
    validation_status=98
fi
native_dir="$artifact_dir/native"
summary="$native_dir/run-summary.txt"
if [ "$test_status" -ne 0 ] \
    || [ ! -s "$summary" ] \
    || ! grep -Fxq 'route_profile=map-any-owner-v2-keys-snapshot-v1' "$summary" \
    || ! grep -Fxq 'global_map_any_v2_production_ready=false' "$summary" \
    || ! grep -Fxq 'native_build_count=2' "$summary" \
    || ! grep -Fxq 'memcheck_run_count=2' "$summary" \
    || ! grep -Fxq 'legacy_fallback_count=0' "$summary"; then
    validation_status=1
fi

if [ -s "$summary" ]; then
    case_dir="$native_dir/case-00-keys-snapshot"
    source_file="$ROOT_DIR/tests/fixtures/mir_ordinary_map_any_owner_v2_keys_snapshot.mimi"
    source_sha="$(sha256sum "$source_file" | awk '{print $1}')"
    receipt_digest="$(sed -n 's/^mir_digest=//p' "$case_dir/receipt/stdout.bin" 2>/dev/null || true)"
    if [ "$(sed -n 's/^target_source_sha256=//p' "$summary")" != "$source_sha" ] \
        || [ "$(sed -n 's/^target_mir_digest=//p' "$summary")" != "$receipt_digest" ] \
        || ! cmp -s "$source_file" "$case_dir/source.fixture.mimi"; then
        validation_status=1
    fi
    for mode in default explicit; do
        memcheck_dir="$case_dir/$mode-memcheck"
        observed_reachable="$(sed -nE 's/^still reachable: ([0-9,]+) bytes in ([0-9,]+) blocks.*/\1 \2/p' \
            "$memcheck_dir/memcheck-summary.txt" 2>/dev/null | tr -d ',')"
        recorded_reachable_bytes="$(sed -n "s/^${mode}_still_reachable_bytes=//p" "$summary")"
        recorded_reachable_blocks="$(sed -n "s/^${mode}_still_reachable_blocks=//p" "$summary")"
        if ! grep -Fxq 'exit_code=0' "$case_dir/$mode-memcheck/process.status" \
            || ! grep -Fq 'ERROR SUMMARY: 0 errors' "$memcheck_dir/memcheck-summary.txt" \
            || ! grep -Fq 'definitely lost: 0 bytes in 0 blocks' "$memcheck_dir/memcheck-summary.txt" \
            || ! grep -Fq 'indirectly lost: 0 bytes in 0 blocks' "$memcheck_dir/memcheck-summary.txt" \
            || ! grep -Fq 'possibly lost: 0 bytes in 0 blocks' "$memcheck_dir/memcheck-summary.txt" \
            || ! grep -Fxq 'expected_exit_code=0' "$memcheck_dir/expected-native-result.txt" \
            || ! cmp -s "$memcheck_dir/expected-native-stdout.bin" "$memcheck_dir/stdout.bin" \
            || [ "$(grep -Ec '^still reachable: [0-9,]+ bytes in [0-9,]+ blocks' "$memcheck_dir/memcheck-summary.txt" 2>/dev/null || true)" -ne 1 ] \
            || [ "$observed_reachable" != "$recorded_reachable_bytes $recorded_reachable_blocks" ]; then
            validation_status=1
        fi
        for required_argument in \
            '"--leak-check=full"' \
            '"--show-leak-kinds=all"' \
            '"--errors-for-leak-kinds=definite,indirect,possible"' \
            '"--error-exitcode=97"' \
            '"--log-file='; do
            if ! grep -Fq "$required_argument" "$memcheck_dir/command.argv.json"; then
                validation_status=1
            fi
        done
    done
    if [ -z "$receipt_digest" ] \
        || ! grep -Fq "canonical route disposition: canonical (map-any-owner-v2-keys-snapshot-v1) mir_digest=$receipt_digest" "$case_dir/default-build/stderr.bin" \
        || ! grep -Fq " function(s), 0 fell back; mir_digest=$receipt_digest" "$case_dir/explicit-build/stderr.bin" \
        || grep -Fq 'canonical route disposition: legacy' "$case_dir/default-build/stderr.bin" \
        || grep -Fq 'canonical route disposition: legacy' "$case_dir/explicit-build/stderr.bin"; then
        validation_status=1
    fi
fi

if [ "$test_status" -eq 0 ] && [ "$validation_status" -eq 0 ]; then
    printf 'PASS KeysSnapshot default/explicit native builds, strict Memcheck, exact route receipt, and source snapshot\n' \
        > "$artifact_dir/artifact-validation.txt"
else
    printf 'FAIL acceptance_test_exit=%s validation_status=%s\n' "$test_status" "$validation_status" \
        > "$artifact_dir/artifact-validation.txt"
fi

(
    cd "$artifact_dir"
    find . -type f ! -name files.sha256 ! -name files.sha256.tmp ! -name files.sha256.sha256 \
        -print0 | sort -z | xargs -0 sha256sum > files.sha256.tmp
    mv -- files.sha256.tmp files.sha256
    sha256sum --check files.sha256 >/dev/null
    sha256sum files.sha256 > files.sha256.sha256
)

if [ "$test_status" -ne 0 ]; then
    exit "$test_status"
fi
if [ "$validation_status" -ne 0 ]; then
    exit 97
fi
