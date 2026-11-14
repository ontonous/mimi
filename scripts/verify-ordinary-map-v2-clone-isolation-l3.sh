#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
requested_artifact_dir="${MIMI_MAP_V2_CLONE_ISOLATION_L3_ARTIFACT_DIR:?Set MIMI_MAP_V2_CLONE_ISOLATION_L3_ARTIFACT_DIR to a new evidence directory}"

for tool in cargo rustc valgrind sha256sum realpath git awk find sort xargs cmp head; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'CloneIsolation L3 evidence capture requires %s\n' "$tool" >&2
        exit 1
    fi
done
if [ ! -d "$LLVM_SYS_181_PREFIX" ]; then
    printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi
LLVM_SYS_181_PREFIX="$(realpath -- "$LLVM_SYS_181_PREFIX")"
export LLVM_SYS_181_PREFIX
MIMI_STDLIB="$ROOT_DIR/std"
export MIMI_STDLIB
export CARGO_INCREMENTAL=0

case "$requested_artifact_dir" in
    /*) ;;
    *) requested_artifact_dir="$ROOT_DIR/$requested_artifact_dir" ;;
esac
if [ -e "$requested_artifact_dir" ] || [ -L "$requested_artifact_dir" ]; then
    printf 'refusing to overwrite existing CloneIsolation L3 evidence: %s\n' "$requested_artifact_dir" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested_artifact_dir")"
artifact_relative="${artifact_dir#"$ROOT_DIR"/}"
case "$artifact_relative" in
    src|src/*|tests|tests/*|std|std/*|scripts|scripts/*|target|target/*|Cargo.toml|Cargo.lock|build.rs)
        printf 'CloneIsolation L3 evidence must be outside source and build trees: %s\n' "$artifact_dir" >&2
        exit 2
        ;;
esac
mkdir -p -- "$(dirname -- "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
export MIMI_MAP_V2_CLONE_ISOLATION_L3_ARTIFACT_DIR="$artifact_dir"

source_snapshot_manifest() {
    (
        cd -- "$ROOT_DIR"
        {
            find src tests std -type f -print0
            printf '%s\0' Cargo.toml Cargo.lock scripts/verify-ordinary-map-v2-clone-isolation-l3.sh
            for configuration in \
                .cargo/config.toml .cargo/config rust-toolchain.toml rust-toolchain; do
                if [ -f "$configuration" ]; then
                    printf '%s\0' "$configuration"
                fi
            done
            if [ -f build.rs ]; then
                printf '%s\0' build.rs
            fi
        } | sort -zu | while IFS= read -r -d '' source_path; do
            sha256sum -- "$source_path"
        done
    )
}

safe_environment_snapshot() {
    local name
    for name in \
        PATH LD_LIBRARY_PATH LD_PRELOAD LLVM_SYS_181_PREFIX RUSTFLAGS RUSTDOCFLAGS \
        CARGO_ENCODED_RUSTFLAGS CARGO_HOME RUSTUP_HOME RUSTC RUSTDOC RUSTC_WRAPPER \
        CARGO_TARGET_DIR CARGO_INCREMENTAL CARGO_BUILD_TARGET CARGO_BUILD_JOBS \
        CARGO_PROFILE_DEV_DEBUG CARGO_PROFILE_TEST_DEBUG CC CXX AR CFLAGS CXXFLAGS \
        CPPFLAGS LDFLAGS TMPDIR TMP TEMP LANG LC_ALL TZ VALGRIND_OPTS MIMI_ROUTE_CENSUS \
        MIMI_STDLIB; do
        if [ "${!name+x}" = x ]; then
            printf '%s=%q\n' "$name" "${!name}"
        else
            printf '%s=<unset>\n' "$name"
        fi
    done
}

record_cargo_configuration() {
    local cargo_home configuration
    cargo_home="${CARGO_HOME:-${HOME:-}}"
    if [ -n "$cargo_home" ] && [ -z "${CARGO_HOME:-}" ]; then
        cargo_home="$cargo_home/.cargo"
    fi
    {
        if [ -n "$cargo_home" ]; then
            printf 'effective_cargo_home=%s\n' "$(realpath -m -- "$cargo_home")"
        else
            printf 'effective_cargo_home=<unavailable>\n'
        fi
        for configuration in \
            "$ROOT_DIR/.cargo/config.toml" "$ROOT_DIR/.cargo/config" \
            "${cargo_home:+$cargo_home/config.toml}" "${cargo_home:+$cargo_home/config}"; do
            [ -n "$configuration" ] || continue
            if [ -f "$configuration" ]; then
                printf 'config_path=%s\n' "$(realpath -- "$configuration")"
                printf 'config_sha256=%s\n' "$(sha256sum -- "$configuration" | awk '{print $1}')"
            else
                printf 'config_path=%s\nconfig_sha256=<absent>\n' "$configuration"
            fi
        done
    } > "$artifact_dir/cargo-configuration.txt"
}

cd -- "$ROOT_DIR"
source_snapshot_manifest > "$artifact_dir/source-snapshot.before.manifest.sha256"
sha256sum -- "$artifact_dir/source-snapshot.before.manifest.sha256" \
    | awk '{print $1}' > "$artifact_dir/source-snapshot.before.sha256"
git -C "$ROOT_DIR" rev-parse HEAD > "$artifact_dir/git-head.txt"
cp -- "$artifact_dir/git-head.txt" "$artifact_dir/git-head.before.txt"
git -C "$ROOT_DIR" status --porcelain=v1 --untracked-files=normal \
    > "$artifact_dir/git-status.before.txt"
safe_environment_snapshot > "$artifact_dir/suite.environment.txt"
record_cargo_configuration

test_name=ordinary_map_owned_any_v2_clone_isolation_both_drop_orders_are_strict_memcheck_clean
cargo_arguments=(
    test
    --manifest-path "$ROOT_DIR/Cargo.toml"
    --test canonical_mir_ordinary_map_any_v2_clone_isolation_memcheck_l3
    "$test_name"
    --
    --exact
    --ignored
    --nocapture
    --test-threads=1
)
{
    printf 'LLVM_SYS_181_PREFIX=%q MIMI_STDLIB=%q CARGO_INCREMENTAL=0 MIMI_MAP_V2_CLONE_ISOLATION_L3_ARTIFACT_DIR=%q ' \
        "$LLVM_SYS_181_PREFIX" "$MIMI_STDLIB" "$artifact_dir"
    printf '%q ' cargo "${cargo_arguments[@]}"
    printf '\n'
} > "$artifact_dir/suite.command.txt"
printf '%s\0' cargo "${cargo_arguments[@]}" > "$artifact_dir/suite.argv.nul"
{
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
    for tool in cargo rustc valgrind sha256sum; do
        tool_path="$(command -v "$tool")"
        tool_resolved="$(realpath -- "$tool_path")"
        printf '%s_path=%s\n' "$tool" "$tool_path"
        printf '%s_resolved=%s\n' "$tool" "$tool_resolved"
        printf '%s_sha256=%s\n' "$tool" "$(sha256sum -- "$tool_resolved" | awk '{print $1}')"
        case "$tool" in
            rustc) printf 'rustc_version='; rustc --version; rustc --version --verbose ;;
            cargo) printf 'cargo_version='; cargo --version ;;
            valgrind) printf 'valgrind_version='; valgrind --version ;;
            sha256sum) printf 'sha256sum_version='; sha256sum --version | head -n 1 ;;
        esac
    done
    if command -v rustup >/dev/null 2>&1; then
        printf 'rustup_active_toolchain='; rustup show active-toolchain
    else
        printf 'rustup_active_toolchain=<rustup-unavailable>\n'
    fi
    llvm_config="$LLVM_SYS_181_PREFIX/bin/llvm-config"
    if [ ! -x "$llvm_config" ]; then
        llvm_config="$LLVM_SYS_181_PREFIX/llvm-config"
    fi
    if [ ! -x "$llvm_config" ]; then
        printf 'LLVM wrapper has no executable llvm-config entrypoint\n' >&2
        exit 1
    fi
    printf 'llvm_config_entrypoint=%s\n' "$llvm_config"
    printf 'llvm_config_resolved=%s\n' "$(realpath -- "$llvm_config")"
    printf 'llvm_version=%s\n' "$("$llvm_config" --version)"
    printf 'llvm_prefix=%s\n' "$("$llvm_config" --prefix)"
    printf 'llvm_libdir=%s\n' "$("$llvm_config" --libdir)"
    printf 'llvm_config_sha256=%s\n' "$(sha256sum -- "$(realpath -- "$llvm_config")" | awk '{print $1}')"
    llvm_library="$LLVM_SYS_181_PREFIX/lib/libLLVM.so"
    if [ -e "$llvm_library" ]; then
        printf 'llvm_library_resolved=%s\n' "$(realpath -- "$llvm_library")"
        printf 'llvm_library_sha256=%s\n' "$(sha256sum -- "$(realpath -- "$llvm_library")" | awk '{print $1}')"
    else
        printf 'llvm_library=%s\n' '<not-present-in-wrapper>'
    fi
} > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"

test_status=0
if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
    MIMI_MAP_V2_CLONE_ISOLATION_L3_ARTIFACT_DIR="$artifact_dir" \
    cargo "${cargo_arguments[@]}" \
        > "$artifact_dir/suite.stdout" \
        2> "$artifact_dir/suite.stderr"; then
    test_status=0
else
    test_status=$?
fi
printf '%s\n' "$test_status" > "$artifact_dir/acceptance-test.exit"

source_snapshot_manifest > "$artifact_dir/source-snapshot.after.manifest.sha256"
sha256sum -- "$artifact_dir/source-snapshot.after.manifest.sha256" \
    | awk '{print $1}' > "$artifact_dir/source-snapshot.after.sha256"
git -C "$ROOT_DIR" rev-parse HEAD > "$artifact_dir/git-head.after.txt"
git -C "$ROOT_DIR" status --porcelain=v1 --untracked-files=normal \
    > "$artifact_dir/git-status.after.txt"
snapshot_status=0
if ! cmp -s "$artifact_dir/source-snapshot.before.manifest.sha256" \
    "$artifact_dir/source-snapshot.after.manifest.sha256"; then
    snapshot_status=1
fi
if ! cmp -s "$artifact_dir/git-head.before.txt" "$artifact_dir/git-head.after.txt"; then
    snapshot_status=1
fi
if ! cmp -s "$artifact_dir/git-head.txt" "$artifact_dir/git-head.before.txt"; then
    snapshot_status=1
fi
for phase in before after; do
    expected_snapshot_digest="$(cat "$artifact_dir/source-snapshot.$phase.sha256")"
    observed_snapshot_digest="$(sha256sum -- "$artifact_dir/source-snapshot.$phase.manifest.sha256" | awk '{print $1}')"
    if [ "$expected_snapshot_digest" != "$observed_snapshot_digest" ]; then
        snapshot_status=1
    fi
done

validation_status=0
for required in \
    "$artifact_dir/git-head.txt" \
    "$artifact_dir/git-head.before.txt" \
    "$artifact_dir/git-head.after.txt" \
    "$artifact_dir/git-status.before.txt" \
    "$artifact_dir/git-status.after.txt" \
    "$artifact_dir/source-snapshot.before.manifest.sha256" \
    "$artifact_dir/source-snapshot.before.sha256" \
    "$artifact_dir/source-snapshot.after.manifest.sha256" \
    "$artifact_dir/source-snapshot.after.sha256" \
    "$artifact_dir/suite.environment.txt" \
    "$artifact_dir/cargo-configuration.txt" \
    "$artifact_dir/suite.command.txt" \
    "$artifact_dir/suite.argv.nul" \
    "$artifact_dir/toolchain.txt" \
    "$artifact_dir/toolchain.stderr" \
    "$artifact_dir/suite.stdout" \
    "$artifact_dir/suite.stderr" \
    "$artifact_dir/native/mimi-binary-identity.txt" \
    "$artifact_dir/native/mimi-binary-sha256/command.argv.json" \
    "$artifact_dir/native/mimi-binary-sha256/stdout.bin" \
    "$artifact_dir/native/mimi-binary-sha256/stderr.bin" \
    "$artifact_dir/native/mimi-binary-sha256/process.status" \
    "$artifact_dir/native/run-summary.txt" \
    "$artifact_dir/acceptance-test.exit"; do
    if [[ "$required" == */stdout.bin || "$required" == */stderr.bin \
        || "$required" == "$artifact_dir/toolchain.stderr" ]]; then
        if [ ! -f "$required" ]; then
            printf 'CloneIsolation L3 suite evidence is missing: %s\n' "$required" >&2
            validation_status=1
        fi
    elif [ ! -s "$required" ]; then
        printf 'CloneIsolation L3 suite evidence is missing: %s\n' "$required" >&2
        validation_status=1
    fi
done
if [ "$test_status" -eq 0 ]; then
    summary="$artifact_dir/native/run-summary.txt"
    if [ ! -s "$summary" ]; then
        printf 'successful CloneIsolation L3 test left no run summary\n' >&2
        validation_status=1
    else
        for expected in \
            'route_profile=map-any-owner-v2-clone-isolation-v1' \
            'global_map_any_v2_production_ready=false' \
            'fixture_order_count=2' \
            'native_build_count=4' \
            'memcheck_run_count=4' \
            'memcheck_still_reachable=0 bytes in 0 blocks' \
            'source_first_default_checked=true' \
            'source_first_explicit_mir_checked=true' \
            'clone_first_default_checked=true' \
            'clone_first_explicit_mir_checked=true'; do
            if ! grep -Fxq "$expected" "$summary"; then
                printf 'CloneIsolation L3 summary lacks %s\n' "$expected" >&2
                validation_status=1
            fi
        done
    fi
    for fixture_order in source-first clone-first; do
        case_dir="$artifact_dir/native/$fixture_order"
        for required in \
            "$case_dir/source.fixture.mimi" \
            "$case_dir/source-identity.txt" \
            "$case_dir/route-control.txt"; do
            if [ ! -s "$required" ]; then
                printf 'CloneIsolation L3 fixture evidence is missing or empty: %s\n' "$required" >&2
                validation_status=1
            fi
        done
        case "$fixture_order" in
            source-first) fixture_hash_dir=source-first-fixture-sha256 ;;
            clone-first) fixture_hash_dir=clone-first-fixture-sha256 ;;
        esac
        for required in \
            "$case_dir/$fixture_hash_dir/command.argv.json" \
            "$case_dir/$fixture_hash_dir/stdout.bin" \
            "$case_dir/$fixture_hash_dir/stderr.bin" \
            "$case_dir/$fixture_hash_dir/process.status"; do
            if [[ "$required" == */stdout.bin || "$required" == */stderr.bin ]]; then
                if [ ! -f "$required" ]; then
                    printf 'CloneIsolation L3 fixture hash evidence is missing: %s\n' "$required" >&2
                    validation_status=1
                fi
            elif [ ! -s "$required" ]; then
                printf 'CloneIsolation L3 fixture hash evidence is missing or empty: %s\n' "$required" >&2
                validation_status=1
            fi
        done
        if [ -f "$case_dir/route-control.txt" ] && \
            ! grep -Fxq 'profile=map-any-owner-v2-clone-isolation-v1' "$case_dir/route-control.txt"; then
            printf 'CloneIsolation L3 route control has the wrong profile: %s\n' "$case_dir/route-control.txt" >&2
            validation_status=1
        fi
        for mode in default explicit-mir; do
            mode_dir="$case_dir/$mode"
            for required in \
                "$mode_dir/build/program" \
                "$mode_dir/build/stdout.bin" \
                "$mode_dir/build/stderr.bin" \
                "$mode_dir/build/process.status" \
                "$mode_dir/build/command.argv.json" \
                "$mode_dir/build/command.environment.txt" \
                "$mode_dir/build/binary-identity.txt" \
                "$mode_dir/build/native-binary-sha256/command.argv.json" \
                "$mode_dir/build/native-binary-sha256/stdout.bin" \
                "$mode_dir/build/native-binary-sha256/stderr.bin" \
                "$mode_dir/build/native-binary-sha256/process.status" \
                "$mode_dir/memcheck/valgrind.log" \
                "$mode_dir/memcheck/memcheck-summary.txt" \
                "$mode_dir/memcheck/process.status" \
                "$mode_dir/memcheck/command.environment.txt" \
                "$mode_dir/memcheck/command.argv.json" \
                "$mode_dir/memcheck/stdout.bin" \
                "$mode_dir/memcheck/stderr.bin" \
                "$mode_dir/memcheck/expected-native-result.txt" \
                "$mode_dir/memcheck/expected-native-stdout.bin"; do
                if [[ "$required" == */stdout.bin || "$required" == */stderr.bin ]]; then
                    if [ ! -f "$required" ]; then
                        printf 'CloneIsolation L3 evidence is missing: %s\n' "$required" >&2
                        validation_status=1
                    fi
                elif [ ! -s "$required" ]; then
                    printf 'CloneIsolation L3 evidence is missing or empty: %s\n' "$required" >&2
                    validation_status=1
                fi
            done
            memcheck_summary="$mode_dir/memcheck/memcheck-summary.txt"
            if [ -f "$memcheck_summary" ]; then
                for expected in \
                    'definitely lost: 0 bytes in 0 blocks' \
                    'indirectly lost: 0 bytes in 0 blocks' \
                    'possibly lost: 0 bytes in 0 blocks' \
                    'still reachable: 0 bytes in 0 blocks' \
                    'ERROR SUMMARY: 0 errors'; do
                    if ! grep -Fxq "$expected" "$memcheck_summary"; then
                        printf 'strict CloneIsolation Memcheck summary lacks "%s": %s\n' \
                            "$expected" "$memcheck_summary" >&2
                        validation_status=1
                    fi
                done
            fi
        done
    done
fi

acceptance_status="$test_status"
if [ "$snapshot_status" -ne 0 ] && [ "$acceptance_status" -eq 0 ]; then
    acceptance_status=97
fi
if [ "$validation_status" -ne 0 ] && [ "$acceptance_status" -eq 0 ]; then
    acceptance_status=97
fi
if [ "$acceptance_status" -eq 0 ]; then
    printf 'PASS exact CloneIsolation receipts for both drop orders, four native builds, and strict zero-loss/zero-reachable Memcheck reports\n' \
        > "$artifact_dir/artifact-validation.txt"
else
    printf 'INCOMPLETE/FAIL test_exit=%s source_snapshot_status=%s evidence_validation_status=%s\n' \
        "$test_status" "$snapshot_status" "$validation_status" \
        > "$artifact_dir/artifact-validation.txt"
fi

if [ -e "$artifact_dir/files.sha256" ] || [ -e "$artifact_dir/files.sha256.sha256" ]; then
    printf 'refusing to overwrite existing CloneIsolation L3 checksum files\n' >&2
    exit 2
fi
(
    cd -- "$artifact_dir"
    find . -type f ! -name files.sha256 ! -name files.sha256.sha256 -print0 \
        | sort -z | xargs -0 sha256sum > files.sha256
    sha256sum --check files.sha256
    sha256sum files.sha256 > files.sha256.sha256
)

printf 'acceptance_test_exit=%s\n' "$acceptance_status"
exit "$acceptance_status"
