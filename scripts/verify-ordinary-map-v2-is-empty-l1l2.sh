#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
requested_artifact_dir="${MIMI_MAP_V2_IS_EMPTY_L1L2_ARTIFACT_DIR:?Set MIMI_MAP_V2_IS_EMPTY_L1L2_ARTIFACT_DIR to a new evidence directory}"
LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"

for required_tool in cargo rustc; do
    if ! command -v "$required_tool" >/dev/null 2>&1; then
        printf 'IsEmpty L1/L2 evidence capture requires %s\n' "$required_tool" >&2
        exit 1
    fi
done
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
    printf 'refusing to overwrite existing IsEmpty L1/L2 evidence: %s\n' "$requested_artifact_dir" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested_artifact_dir")"
case "$artifact_dir" in
    "$ROOT_DIR"/*) ;;
    *)
        printf 'IsEmpty L1/L2 evidence must be inside this repository: %s\n' "$artifact_dir" >&2
        exit 2
        ;;
esac
artifact_relative="${artifact_dir#"$ROOT_DIR"/}"
case "$artifact_relative" in
    .git|.git/*|.cargo|.cargo/*|src|src/*|tests|tests/*|std|std/*|scripts|scripts/*|Cargo.toml|Cargo.lock|build.rs)
        printf 'IsEmpty L1/L2 evidence must be outside source and Git metadata: %s\n' "$artifact_dir" >&2
        exit 2
        ;;
esac
mkdir -p "$(dirname -- "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"

source_snapshot_manifest() {
    (
        cd "$ROOT_DIR"
        {
            find src tests std -type f -print0
            if [ -d .cargo ]; then
                find .cargo -type f -print0
            fi
            is_empty_l1l2_cargo_config_home="${CARGO_HOME:-${HOME:-}/.cargo}"
            if [ -n "$is_empty_l1l2_cargo_config_home" ]; then
                for cargo_config in \
                    "$is_empty_l1l2_cargo_config_home/config" \
                    "$is_empty_l1l2_cargo_config_home/config.toml"; do
                    if [ -f "$cargo_config" ]; then
                        printf '%s\0' "$cargo_config"
                    fi
                done
            fi
            printf '%s\0' Cargo.toml Cargo.lock scripts/verify-ordinary-map-v2-is-empty-l1l2.sh
            if [ -f build.rs ]; then
                printf '%s\0' build.rs
            fi
        } | sort -zu | while IFS= read -r -d '' source_path; do
            sha256sum -- "$source_path"
        done
    )
}

cd "$ROOT_DIR"
export LLVM_SYS_181_PREFIX CARGO_BUILD_JOBS
export CARGO_TERM_COLOR=never
if [[ -v MIMI_TEST_FORCE_NO_Z3 ]]; then
    caller_forced_no_z3=true
else
    caller_forced_no_z3=false
fi
# This profile's verifier gate must exercise the real linked Z3 runtime. The
# debug-only test hook is useful for no-solver coverage elsewhere, but must not
# silently turn this required gate into a skipped test.
unset MIMI_TEST_FORCE_NO_Z3
source_snapshot_manifest > "$artifact_dir/source-snapshot.before.manifest.sha256"
sha256sum "$artifact_dir/source-snapshot.before.manifest.sha256" \
    | awk '{print $1}' > "$artifact_dir/source-snapshot.before.sha256"
git rev-parse HEAD > "$artifact_dir/head.txt"
git branch --show-current > "$artifact_dir/branch.txt"
git config user.name > "$artifact_dir/git-user-name.txt"
git config user.email > "$artifact_dir/git-user-email.txt"
{
    printf 'cwd=%s\nLLVM_SYS_181_PREFIX=%s\nCARGO_BUILD_JOBS=%s\nCARGO_TERM_COLOR=%s\nMIMI_TEST_FORCE_NO_Z3=unset\ncaller_MIMI_TEST_FORCE_NO_Z3_was_set=%s\n' \
        "$ROOT_DIR" "$LLVM_SYS_181_PREFIX" "$CARGO_BUILD_JOBS" "$CARGO_TERM_COLOR" "$caller_forced_no_z3"
    for name in CARGO_HOME CARGO_INCREMENTAL CARGO_TARGET_DIR RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER CC CXX AR CFLAGS CXXFLAGS CPPFLAGS LD_LIBRARY_PATH LD_PRELOAD PATH HOME TMPDIR LANG LC_ALL RUST_LOG; do
        if [[ -v $name ]]; then
            printf '%s=%q\n' "$name" "${!name}"
        else
            printf '%s=unset\n' "$name"
        fi
    done
    is_empty_l1l2_cargo_home="${CARGO_HOME:-${HOME:-}/.cargo}"
    if [ -n "$is_empty_l1l2_cargo_home" ] && [ -d "$is_empty_l1l2_cargo_home" ]; then
        for config in "$is_empty_l1l2_cargo_home/config" "$is_empty_l1l2_cargo_home/config.toml"; do
            if [ -f "$config" ]; then
                printf 'cargo_config='; realpath -- "$config"
                sha256sum -- "$config"
            fi
        done
    fi
    printf 'cargo_path=%s\n' "$(command -v cargo)"
    printf 'cargo_version='; cargo --version
    printf 'rustc_path=%s\n' "$(command -v rustc)"
    printf 'rustc_version='; rustc --version
    printf 'llvm_config_path=%s\n' "$LLVM_SYS_181_PREFIX/bin/llvm-config"
    printf 'llvm_version='; "$LLVM_SYS_181_PREFIX/bin/llvm-config" --version
    if command -v z3 >/dev/null 2>&1; then
        printf 'z3_path=%s\n' "$(command -v z3)"
        printf 'z3_version='; z3 --version
    else
        printf 'z3_path=unavailable\nz3_version=unavailable\n'
    fi
} > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"

# The CLI tests explicitly opt into route census. Any other inherited Mimi
# control variable can affect routing, optimization, runtime checks, or output
# files, so record which names were present and run this gate with them unset.
mapfile -t caller_mimi_environment_names < <(
    compgen -e | LC_ALL=C sort | awk 'index($0, "MIMI_") == 1'
)
{
    printf 'effective_mimi_environment=unset_except_per_command_MIMI_ROUTE_CENSUS\n'
    for name in "${caller_mimi_environment_names[@]}"; do
        printf 'caller_%s=was_set_and_sanitized\n' "$name"
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
    printf 'MIMI_TEST_FORCE_NO_Z3=unset\n'
} > "$artifact_dir/mimi-environment.txt"
for name in "${caller_mimi_environment_names[@]}"; do
    unset "$name"
done
unset LD_PRELOAD

failed=0
run_stage() {
    local name="$1"
    shift
    local stage_dir="$artifact_dir/$name"
    mkdir -- "$stage_dir"
    {
        printf 'LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=%q CARGO_TERM_COLOR=%q ' \
            "$LLVM_SYS_181_PREFIX" "$CARGO_BUILD_JOBS" "$CARGO_TERM_COLOR"
        printf '%q ' "$@"
        printf '\n'
    } > "$stage_dir/command.txt"
    printf '%s\0' "$@" > "$stage_dir/argv.nul"
    local status=0
    if "$@" > "$stage_dir/stdout" 2> "$stage_dir/stderr"; then
        status=0
    else
        status=$?
    fi
    printf '%s\n' "$status" > "$stage_dir/exit"
    if [ "$status" -ne 0 ]; then
        failed=1
    fi
    printf '%s exit=%s\n' "$name" "$status"
}

expect_one_pass() {
    local name="$1"
    if ! grep -hFq 'test result: ok. 1 passed; 0 failed; 0 ignored;' \
        "$artifact_dir/$name/stdout" "$artifact_dir/$name/stderr"; then
        printf 'IsEmpty L1/L2 stage did not report exactly one passing test: %s\n' "$name" >&2
        failed=1
    fi
}

run_stage format cargo fmt --all -- --check
run_stage route-and-source-negative cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --lib ordinary_map_owned_any_v2_is_empty_has_an_exact_non_global_route \
    -- --nocapture --test-threads=1
expect_one_pass route-and-source-negative
run_stage observer-v1-preflight-regression cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --bin mimi ordinary_map_owner_observer_v1_uses_its_exact_receipt_for_default_and_explicit_routes \
    -- --nocapture --test-threads=1
expect_one_pass observer-v1-preflight-regression
run_stage map-set-drop-control-route cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --lib ordinary_map_owned_any_v2_map_set_drop_control_is_exact_and_isolated \
    -- --nocapture --test-threads=1
expect_one_pass map-set-drop-control-route
run_stage set-payload-temporary-drop cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --lib ordinary_map_owned_any_v2_set_drops_record_payload_clone_before_source \
    -- --nocapture --test-threads=1
expect_one_pass set-payload-temporary-drop
run_stage type-catalog-profile cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --lib checked_v2_is_empty_profile_materializes_only_its_required_descriptors \
    -- --nocapture --test-threads=1
expect_one_pass type-catalog-profile
run_stage verifier-capability-boundary cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --lib is_empty_requires_the_exact_checker_and_mir_route_across_capability_and_raw_apis \
    -- --nocapture --test-threads=1
expect_one_pass verifier-capability-boundary
run_stage checked-single-and-dual-verifier cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --lib checked_single_and_dual_verifiers_keep_the_narrow_is_empty_route \
    -- --nocapture --test-threads=1
expect_one_pass checked-single-and-dual-verifier
if ! grep -hFxq 'MIMI_IS_EMPTY_VERIFIER z3_available=true outcome=executed' \
    "$artifact_dir/checked-single-and-dual-verifier/stdout" \
    "$artifact_dir/checked-single-and-dual-verifier/stderr"; then
    printf 'IsEmpty verifier gate did not prove that real Z3 execution occurred\n' >&2
    failed=1
fi
run_stage all-cli-consumers cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --test canonical_mir_ordinary_map_any_is_empty_cli \
    ordinary_map_any_v2_is_empty_has_one_receipt_across_all_cli_consumers \
    -- --nocapture --test-threads=1
expect_one_pass all-cli-consumers
run_stage no-query-control-all-cli-consumers cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
    --test canonical_mir_ordinary_map_any_is_empty_cli \
    ordinary_map_owned_any_v2_map_set_drop_control_reaches_all_cli_consumers \
    -- --nocapture --test-threads=1
expect_one_pass no-query-control-all-cli-consumers
run_stage patch-whitespace git diff --check

source_snapshot_manifest > "$artifact_dir/source-snapshot.after.manifest.sha256"
sha256sum "$artifact_dir/source-snapshot.after.manifest.sha256" \
    | awk '{print $1}' > "$artifact_dir/source-snapshot.after.sha256"
snapshot_status=0
if ! cmp -s "$artifact_dir/source-snapshot.before.manifest.sha256" \
    "$artifact_dir/source-snapshot.after.manifest.sha256"; then
    snapshot_status=1
    failed=1
fi

if [ "$failed" -eq 0 ]; then
    printf 'profile=map-any-owner-v2-is-empty-v1\nexit_code=0\nsource_snapshot_identical=true\nglobal_map_any_v2_production_ready=false\n' \
        > "$artifact_dir/artifact-validation.txt"
else
    printf 'profile=map-any-owner-v2-is-empty-v1\nexit_code=1\nsource_snapshot_identical=%s\nglobal_map_any_v2_production_ready=false\n' \
        "$([ "$snapshot_status" -eq 0 ] && printf true || printf false)" \
        > "$artifact_dir/artifact-validation.txt"
fi

(
    cd "$artifact_dir"
    find . -type f ! -name files.sha256 ! -name files.sha256.sha256 -print0 \
        | sort -z | xargs -0 sha256sum > files.sha256
    sha256sum --check files.sha256
    sha256sum files.sha256 > files.sha256.sha256
)

printf 'acceptance_test_exit=%s\n' "$failed"
exit "$failed"
