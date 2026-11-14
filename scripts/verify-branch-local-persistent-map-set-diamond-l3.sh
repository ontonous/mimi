#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if declare -p LLVM_SYS_181_PREFIX >/dev/null 2>&1; then
    if [ -z "$LLVM_SYS_181_PREFIX" ]; then
        LLVM_SYS_181_PREFIX="/tmp/llvm-wrapper"
    fi
else
    LLVM_SYS_181_PREFIX="/tmp/llvm-wrapper"
fi
ARTIFACT_ENV="MIMI_BRANCH_LOCAL_SET_DIAMOND_L3_ARTIFACT_DIR"

if declare -p MIMI_BRANCH_LOCAL_SET_DIAMOND_L3_ARTIFACT_DIR >/dev/null 2>&1 \
    && [ -n "$MIMI_BRANCH_LOCAL_SET_DIAMOND_L3_ARTIFACT_DIR" ]; then
    requested_artifact_dir="$MIMI_BRANCH_LOCAL_SET_DIAMOND_L3_ARTIFACT_DIR"
    case "$requested_artifact_dir" in
        /*) ;;
        *) requested_artifact_dir="$ROOT_DIR/$requested_artifact_dir" ;;
    esac
else
    requested_artifact_dir="$ROOT_DIR/evidence/0.1.11-dev/gates/branch-local-persistent-map-set-diamond-l3-$(date -u +%Y%m%dT%H%M%SZ)-$$"
fi

if [ -e "$requested_artifact_dir" ] || [ -L "$requested_artifact_dir" ]; then
    printf 'refusing to overwrite existing branch-local Set diamond L3 artifacts: %s\n' "$requested_artifact_dir" >&2
    exit 2
fi
mkdir -p "$(dirname "$requested_artifact_dir")"
mkdir -- "$requested_artifact_dir"
ARTIFACT_DIR="$(realpath -- "$requested_artifact_dir")"
export MIMI_BRANCH_LOCAL_SET_DIAMOND_L3_ARTIFACT_DIR="$ARTIFACT_DIR"
export LLVM_SYS_181_PREFIX

source_snapshot_manifest() {
    {
        find src std -type f -print0
        printf '%s\0' Cargo.toml Cargo.lock build.rs .gitignore
        printf '%s\0' \
            tests/fixtures/mir_branch_local_persistent_map_set_diamond_true.mimi \
            tests/fixtures/mir_branch_local_persistent_map_set_diamond_false.mimi \
            tests/canonical_mir_branch_local_persistent_map_set_diamond_l3.rs \
            tests/support/memcheck_summary.rs \
            scripts/verify-branch-local-persistent-map-set-diamond-l3.sh
    } | LC_ALL=C sort -zu | while IFS= read -r -d '' source_path; do
        sha256sum -- "$source_path"
    done
}

finish() {
    status=$?
    trap - EXIT
    if [ -f "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256" ]; then
        if source_snapshot_manifest > "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256"; then
            sha256sum -- "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256" \
                > "$ARTIFACT_DIR/source-snapshot.after.sha256"
            if cmp -s "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256" \
                "$ARTIFACT_DIR/source-snapshot.after.manifest.sha256"; then
                printf 'stable=true\n' > "$ARTIFACT_DIR/source-snapshot.status"
            else
                printf 'stable=false\n' > "$ARTIFACT_DIR/source-snapshot.status"
                status=1
            fi
        else
            printf 'stable=false\nmanifest_capture_failed=true\n' \
                > "$ARTIFACT_DIR/source-snapshot.status"
            status=1
        fi
    fi
    printf 'exit_code=%s\n' "$status" > "$ARTIFACT_DIR/runner.exit"
    exit "$status"
}
trap finish EXIT

cd "$ROOT_DIR"
git branch --show-current > "$ARTIFACT_DIR/runner.branch.txt"
git rev-parse HEAD > "$ARTIFACT_DIR/runner.head.txt"
source_snapshot_manifest > "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256"
sha256sum -- "$ARTIFACT_DIR/source-snapshot.before.manifest.sha256" \
    > "$ARTIFACT_DIR/source-snapshot.before.sha256"
printf '%s\n' 'The regular route test exercises default and explicit native build/run, bytecode mimi run, and mimi verify for both fixture arms, checking route receipts, verifier provenance, and the exact no-contract empty-proof result.' 'The ignored strict L3 test repeats those route gates, disables default suppressions, and requires zero Memcheck errors, zero suppressed error contexts, and zero lost allocations.' > "$ARTIFACT_DIR/gate-scope.txt"
sha256sum tests/fixtures/mir_branch_local_persistent_map_set_diamond_true.mimi tests/fixtures/mir_branch_local_persistent_map_set_diamond_false.mimi tests/canonical_mir_branch_local_persistent_map_set_diamond_l3.rs tests/support/memcheck_summary.rs scripts/verify-branch-local-persistent-map-set-diamond-l3.sh > "$ARTIFACT_DIR/source-hashes.runner.sha256"

{
    printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_SYS_181_PREFIX"
    printf '%s=%s\n' "$ARTIFACT_ENV" "$ARTIFACT_DIR"
    printf 'MIMI_ROUTE_CENSUS=1 (set on each native build, bytecode run, and verifier command)\n'
    printf 'MIMI_VERBOSE=1 (set on each verifier command to expose consumed receipt provenance)\n'
    printf 'rustc_path=%s\n' "$(command -v rustc)"
    printf 'rustc_version='; rustc --version
    printf 'cargo_path=%s\n' "$(command -v cargo)"
    printf 'cargo_version='; cargo --version
    if command -v rustup >/dev/null 2>&1; then
        printf 'rustup_path=%s\n' "$(command -v rustup)"
        rustup_status=0
        active_toolchain="$(rustup show active-toolchain 2>&1)" || rustup_status=$?
        printf 'rustup_active_toolchain=%s\n' "$active_toolchain"
        printf 'rustup_active_toolchain_exit_code=%s\n' "$rustup_status"
    else
        printf 'rustup_path=missing\n'
    fi
    if command -v valgrind >/dev/null 2>&1; then
        printf 'valgrind_path=%s\n' "$(command -v valgrind)"
        printf 'valgrind_version='; valgrind --version
        printf '%s\n' 'valgrind_default_suppressions=disabled' 'valgrind_VALGRIND_OPTS=removed_from_invocations' 'valgrind_VALGRIND_LIB=removed_from_invocations'
    else
        printf 'valgrind_path=missing\n'
    fi
} > "$ARTIFACT_DIR/runner.environment.txt" 2> "$ARTIFACT_DIR/runner.environment.stderr"

if ! command -v valgrind >/dev/null 2>&1; then
    printf 'strict branch-local Set diamond L3 requires Valgrind; install it before running this gate\n' >&2
    exit 1
fi
if [ ! -d "$LLVM_SYS_181_PREFIX" ]; then
    printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
fi

test_target=canonical_mir_branch_local_persistent_map_set_diamond_l3
ordinary_test=branch_local_persistent_set_diamond_default_and_explicit_native_routes_pass
strict_test=branch_local_persistent_set_diamond_strict_memcheck_l3
ordinary_arguments=(test --manifest-path "$ROOT_DIR/Cargo.toml" --test "$test_target" "$ordinary_test" -- --exact --nocapture --test-threads=1)
strict_arguments=(test --manifest-path "$ROOT_DIR/Cargo.toml" --test "$test_target" "$strict_test" -- --exact --ignored --nocapture --test-threads=1)

write_command_record() {
    local label="$1"
    shift
    {
        printf 'LLVM_SYS_181_PREFIX=%q %s=%q ' "$LLVM_SYS_181_PREFIX" "$ARTIFACT_ENV" "$ARTIFACT_DIR"
        printf '%q ' cargo "$@"
        printf '\n'
    } > "$ARTIFACT_DIR/$label.command.txt"
    printf '%s\0' cargo "$@" > "$ARTIFACT_DIR/$label.argv.nul"
}

run_test() {
    local label="$1"
    shift
    local status=0
    write_command_record "$label" "$@"
    if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" MIMI_BRANCH_LOCAL_SET_DIAMOND_L3_ARTIFACT_DIR="$ARTIFACT_DIR" cargo "$@" > "$ARTIFACT_DIR/$label.stdout" 2> "$ARTIFACT_DIR/$label.stderr"; then
        status=0
    else
        status=$?
    fi
    printf 'exit_code=%s\n' "$status" > "$ARTIFACT_DIR/$label.exit"
    cat "$ARTIFACT_DIR/$label.stdout"
    cat "$ARTIFACT_DIR/$label.stderr" >&2
    return "$status"
}

ordinary_status=0
if run_test ordinary-native-routes "${ordinary_arguments[@]}"; then
    ordinary_status=0
else
    ordinary_status=$?
fi
strict_status=0
if run_test strict-memcheck-l3 "${strict_arguments[@]}"; then
    strict_status=0
else
    strict_status=$?
fi

ignore_status=0
gitignore_report="$ARTIFACT_DIR/native-gitignore.txt"
: > "$gitignore_report"
while IFS= read -r -d '' native_binary; do
    case "$native_binary" in
        "$ROOT_DIR"/*)
            relative_binary="${native_binary#"$ROOT_DIR"/}"
            if git check-ignore -v -- "$relative_binary" >> "$gitignore_report"; then
                :
            else
                printf 'not ignored: %s\n' "$relative_binary" >> "$gitignore_report"
                ignore_status=1
            fi
            ;;
        *)
            printf 'outside repository; cannot be staged: %s\n' "$native_binary" >> "$gitignore_report"
            ;;
    esac
done < <(find "$ARTIFACT_DIR" -type f -path '*/native/*/default-build/program' -print0 -o -type f -path '*/native/*/explicit-build/program' -print0)

if [ "$ordinary_status" -ne 0 ] || [ "$strict_status" -ne 0 ] || [ "$ignore_status" -ne 0 ]; then
    exit 1
fi
