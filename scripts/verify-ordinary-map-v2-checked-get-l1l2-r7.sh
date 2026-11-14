#!/usr/bin/env bash
set -euo pipefail
set -o noclobber

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT_DIR"
ARTIFACT_ENV=MIMI_MAP_V2_CHECKED_GET_L1L2_ARTIFACT_DIR
TARGET_DIR_ENV=MIMI_MAP_V2_CHECKED_GET_CARGO_TARGET_DIR
PROFILE=map-any-owner-v2-checked-get-v1
CLI_TARGET=canonical_mir_ordinary_map_any_owner_v2_cli
REFERENCE_TARGET=canonical_mir_ordinary_map_any_v2_checked_get_branch_tail_reference
RUNNER=scripts/verify-ordinary-map-v2-checked-get-l1l2-r7.sh
CARGO_INCREMENTAL=0
export CARGO_INCREMENTAL

if [[ ! -v LLVM_SYS_181_PREFIX || -z "$LLVM_SYS_181_PREFIX" ]]; then
    LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper
fi
for tool in bash cargo rustc sha256sum realpath git find sort wc python3; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'CheckedGet L1/L2 capture requires %s\n' "$tool" >&2
        exit 1
    }
done
if [[ -v "$TARGET_DIR_ENV" && -n "${!TARGET_DIR_ENV}" ]]; then
    requested_target_dir="${!TARGET_DIR_ENV}"
    case "$requested_target_dir" in
        /*) ;;
        *) printf '%s must be an absolute Cargo target directory: %s\n' "$TARGET_DIR_ENV" "$requested_target_dir" >&2; exit 2 ;;
    esac
    cargo_target_dir="$(realpath -m -- "$requested_target_dir")"
    if [[ "$cargo_target_dir" != "$requested_target_dir" ]]; then
        printf '%s must already be the physical canonical path: %s\n' "$TARGET_DIR_ENV" "$requested_target_dir" >&2
        exit 2
    fi
    case "$cargo_target_dir/" in
        "$ROOT_DIR/"*) printf 'explicit CheckedGet Cargo target must be outside the source tree: %s\n' "$cargo_target_dir" >&2; exit 2 ;;
    esac
    if [[ -L "$cargo_target_dir" ]]; then
        printf 'explicit CheckedGet Cargo target must not be a symlink: %s\n' "$cargo_target_dir" >&2
        exit 2
    fi
    mkdir -p -- "$cargo_target_dir"
    if [[ ! -d "$cargo_target_dir" || -L "$cargo_target_dir" || "$(realpath -- "$cargo_target_dir")" != "$cargo_target_dir" ]]; then
        printf 'explicit CheckedGet Cargo target must resolve to a regular physical directory: %s\n' "$cargo_target_dir" >&2
        exit 2
    fi
    cargo_target_dir_policy=explicit-external-physical-directory
    cargo_target_dir_requested="$requested_target_dir"
else
    cargo_target_dir="$ROOT_DIR/target"
    cargo_target_dir_policy=workspace-target-default
    cargo_target_dir_requested='<unset>'
fi
cargo_target_dir_physical="$(realpath -m -- "$cargo_target_dir")"
[[ -d "$LLVM_SYS_181_PREFIX" ]] || {
    printf 'LLVM wrapper directory is unavailable: %s\n' "$LLVM_SYS_181_PREFIX" >&2
    exit 1
}

CARGO_BIN="$(command -v cargo)"
RUSTC_BIN="$(command -v rustc)"
BASH_BIN="$(command -v bash)"
SHA256SUM_BIN="$(command -v sha256sum)"
PYTHON3_BIN="$(command -v python3)"
GIT_BIN="$(command -v git)"
FIND_BIN="$(command -v find)"
SORT_BIN="$(command -v sort)"
REALPATH_BIN="$(command -v realpath)"
if [[ -x "$LLVM_SYS_181_PREFIX/bin/llvm-config" ]]; then
    LLVM_CONFIG_BIN="$LLVM_SYS_181_PREFIX/bin/llvm-config"
elif [[ -x "$LLVM_SYS_181_PREFIX/llvm-config" ]]; then
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
    requested="$ROOT_DIR/evidence/0.1.11-dev/gates/checked-get-current-20261102-r3-l1l2"
fi
case "$requested" in /*) ;; *) requested="$ROOT_DIR/$requested" ;; esac
if [[ -e "$requested" || -L "$requested" ]]; then
    printf 'refusing to overwrite existing CheckedGet L1/L2 artifact path: %s\n' "$requested" >&2
    exit 2
fi
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/scripts/"*|"$ROOT_DIR/target/"*)
        printf 'CheckedGet L1/L2 artifacts must be outside source/build trees: %s\n' "$artifact_dir" >&2
        exit 2 ;;
esac
case "$artifact_dir/" in
    "$cargo_target_dir/"*|"$cargo_target_dir_physical/"*) printf 'CheckedGet L1/L2 artifacts must be outside the Cargo target directory: %s\n' "$artifact_dir" >&2; exit 2 ;;
esac
case "$cargo_target_dir/" in
    "$artifact_dir/"*) printf 'CheckedGet L1/L2 Cargo target must be outside the artifact directory: %s\n' "$cargo_target_dir" >&2; exit 2 ;;
esac
case "$cargo_target_dir_physical/" in
    "$artifact_dir/"*) printf 'CheckedGet L1/L2 Cargo target must be outside the artifact directory: %s\n' "$cargo_target_dir_physical" >&2; exit 2 ;;
esac
mkdir -p -- "$(dirname -- "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"
scratch=""

record_runner_exit() {
    local status="$1"
    if [[ "$status" -ne 0 && -f "$artifact_dir/runner-exit.status" ]] \
        && grep -qx 'success=true' "$artifact_dir/runner-exit.status"; then
        printf 'exit_code=%s\nsuccess=false\n' "$status" > "$artifact_dir/runner-exit.status"
        python3 - "$artifact_dir/artifact-validation.txt" "$status" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text() if path.exists() else "validation=FAIL\nprofile=CheckedGet\nlevel=L1/L2\n"
text = text.replace("validation=PASS", "validation=FAIL", 1)
reason = f"reason=runner exited unexpectedly after recording success (exit={sys.argv[2]})\n"
if reason not in text:
    text += reason
path.write_text(text)
PY
        if [[ -f "$artifact_dir/artifact-manifest.status" ]]; then
            printf 'exit_code=3\nsuccess=false\n' > "$artifact_dir/artifact-manifest.status"
        fi
    fi
    if [[ ! -f "$artifact_dir/artifact-validation.txt" ]]; then
        printf 'validation=FAIL\nprofile=CheckedGet\nlevel=L1/L2\nreason=runner exited before the validator (exit=%s)\n' \
            "$status" > "$artifact_dir/artifact-validation.txt"
    fi
    if [[ ! -f "$artifact_dir/runner-exit.status" ]]; then
        printf 'exit_code=%s\nsuccess=false\n' "$status" > "$artifact_dir/runner-exit.status"
    fi
    if [[ -n "$scratch" && -d "$scratch" ]]; then
        rm -rf -- "$scratch"
    fi
}
trap 'runner_exit_status=$?; record_runner_exit "$runner_exit_status"; exit "$runner_exit_status"' EXIT

{
    printf 'cwd=%s\n' "$ROOT_DIR"
    printf 'runner=%s\n' "$RUNNER"
    printf 'cargo_target_dir_env=%s\ncargo_target_dir_requested=%q\ncargo_target_dir=%q\ncargo_target_dir_physical=%q\ncargo_target_dir_policy=%s\n' \
        "$TARGET_DIR_ENV" "$cargo_target_dir_requested" "$cargo_target_dir" "$cargo_target_dir_physical" "$cargo_target_dir_policy"
    printf 'command=cd %q && CARGO_INCREMENTAL=%q LLVM_SYS_181_PREFIX=%q %s=%q bash %q\n' \
        "$ROOT_DIR" "$CARGO_INCREMENTAL" "$LLVM_SYS_181_PREFIX" "$ARTIFACT_ENV" "$artifact_dir" "$ROOT_DIR/$RUNNER"
} > "$artifact_dir/runner-invocation.txt"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/mimi-checked-get-l1l2.XXXXXX")"

write_source_manifest() {
    local destination="$1" paths="$scratch/source.paths.nul" sorted="$scratch/source.sorted.nul"
    local path
    : >| "$paths"
    : >| "$destination"
    find src tests std -type f -print0 >| "$paths"
    for path in Cargo.toml Cargo.lock build.rs rust-toolchain rust-toolchain.toml \
        .cargo/config .cargo/config.toml "$RUNNER"; do
        if [[ -f "$path" ]]; then printf '%s\0' "$path" >> "$paths"; fi
    done
    sort -zu "$paths" -o "$sorted"
    while IFS= read -r -d '' path; do
        [[ -f "$path" ]] || { printf 'source disappeared before hashing: %s\n' "$path" >&2; return 1; }
        "$SHA256SUM_BIN" -- "$path" >> "$destination"
    done < "$sorted"
}

record_vcs() {
    local destination="$1"
    {
        printf 'branch='; git branch --show-current
        printf 'head='; git rev-parse HEAD
        printf 'tracked_worktree_status_begin\n'
        git status --short --untracked-files=no
        printf 'tracked_worktree_status_end\n'
    } > "$destination"
}

record_tool() {
    local label="$1" executable="$2"; shift 2
    local directory="$artifact_dir/toolchain/$label" status=0
    mkdir -p -- "$directory"
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
    fi
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" > "$directory/process.status"
    if [[ -f "$executable" ]]; then
        printf 'path=%s\n' "$executable" > "$directory/identity.txt"
        "$SHA256SUM_BIN" -- "$executable" >> "$directory/identity.txt"
    fi
    [[ "$status" -eq 0 ]]
}

write_command() {
    local directory="$1"; shift
    {
        printf 'cd %q && LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=%q CARGO_TARGET_DIR=%q ' \
            "$ROOT_DIR" "$LLVM_SYS_181_PREFIX" "$CARGO_INCREMENTAL" "$cargo_target_dir"
        printf '%q' "$@"
        printf '\n'
    } > "$directory/command.txt"
    printf '%s\0' "$@" > "$directory/command.argv.nul"
    {
        printf 'cwd=%s\nLLVM_SYS_181_PREFIX=%s\nCARGO_BUILD_JOBS=1\nCARGO_INCREMENTAL=%s\nCARGO_TARGET_DIR=%s\n' \
            "$ROOT_DIR" "$LLVM_SYS_181_PREFIX" "$CARGO_INCREMENTAL" "$cargo_target_dir"
        printf '%s=%q\ncargo_target_dir_physical=%q\ncargo_target_dir_policy=%s\n' \
            "$TARGET_DIR_ENV" "$cargo_target_dir_requested" "$cargo_target_dir_physical" "$cargo_target_dir_policy"
        for variable in PATH LLVM_SYS_181_PREFIX LD_LIBRARY_PATH RUSTC RUSTUP_TOOLCHAIN \
            RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_HOME CARGO_TARGET_DIR CARGO_INCREMENTAL \
            MIMI_ROUTE_CENSUS MIMI_VERBOSE; do
            if [[ -v "$variable" ]]; then printf '%s=%q\n' "$variable" "${!variable}"; else printf '%s=<unset>\n' "$variable"; fi
        done
    } > "$directory/command.environment.txt"
}

run_cargo_case() {
    local name="$1"; shift
    local directory="$artifact_dir/$name" status=0
    mkdir -- "$directory"
    write_command "$directory" "$CARGO_BIN" "$@"
    if (cd "$ROOT_DIR" && env LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" \
        CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL="$CARGO_INCREMENTAL" \
        CARGO_TARGET_DIR="$cargo_target_dir" "$CARGO_BIN" "$@") \
        > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    {
        printf '===== stdout =====\n'; cat "$directory/stdout.bin"
        printf '\n===== stderr =====\n'; cat "$directory/stderr.bin"
    } > "$directory/command.log"
    printf '%s\n' "$status" > "$directory/command.exit"
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" > "$directory/process.status"
    cat "$directory/command.log"
}

record_vcs "$artifact_dir/vcs.before.txt"
write_source_manifest "$artifact_dir/source-snapshot.before.manifest.sha256"
sha256sum -- "$artifact_dir/source-snapshot.before.manifest.sha256" | awk '{print $1}' \
    > "$artifact_dir/source-snapshot.before.sha256"
sha256sum -- tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_settled_branch_tail.mimi \
    tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_hit_bool.mimi \
    tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_multi_lookup.mimi \
    tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_multi_lookup_reversed.mimi \
    > "$artifact_dir/fixtures.before.sha256"

mkdir -- "$artifact_dir/toolchain"
tool_status=0
record_tool bash "$BASH_BIN" --version || tool_status=1
record_tool rustc "$RUSTC_BIN" --version --verbose || tool_status=1
record_tool cargo "$CARGO_BIN" --version --verbose || tool_status=1
record_tool llvm-config "$LLVM_CONFIG_BIN" --version --prefix --libdir --host-target || tool_status=1
record_tool sha256sum "$SHA256SUM_BIN" --version || tool_status=1
record_tool python3 "$PYTHON3_BIN" --version || tool_status=1
record_tool git "$GIT_BIN" --version || tool_status=1
record_tool find "$FIND_BIN" --version || tool_status=1
record_tool sort "$SORT_BIN" --version || tool_status=1
record_tool realpath "$REALPATH_BIN" --version || tool_status=1
printf 'exit_code=%s\n' "$tool_status" > "$artifact_dir/toolchain/toolchain-capture.exit"
{
    printf 'LLVM_SYS_181_PREFIX=%q\nCARGO_BUILD_JOBS=1\nCARGO_INCREMENTAL=%q\nCARGO_TARGET_DIR=%q\n' \
        "$LLVM_SYS_181_PREFIX" "$CARGO_INCREMENTAL" "$cargo_target_dir"
    printf '%s=%q\ncargo_target_dir_physical=%q\ncargo_target_dir_policy=%s\n' \
        "$TARGET_DIR_ENV" "$cargo_target_dir_requested" "$cargo_target_dir_physical" "$cargo_target_dir_policy"
    for variable in PATH LLVM_SYS_181_PREFIX LD_LIBRARY_PATH RUSTC RUSTUP_TOOLCHAIN \
        RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_HOME CARGO_TARGET_DIR CARGO_INCREMENTAL \
        MIMI_ROUTE_CENSUS MIMI_VERBOSE; do
        if [[ -v "$variable" ]]; then printf '%s=%q\n' "$variable" "${!variable}"; else printf '%s=<unset>\n' "$variable"; fi
    done
} > "$artifact_dir/toolchain/environment.txt"
sha256sum -- "$BASH_BIN" "$RUSTC_BIN" "$CARGO_BIN" "$LLVM_CONFIG_BIN" "$SHA256SUM_BIN" \
    "$PYTHON3_BIN" "$GIT_BIN" "$FIND_BIN" "$SORT_BIN" "$REALPATH_BIN" \
    > "$artifact_dir/toolchain/binaries.sha256"

# Build the CLI once so the independently captured six-path-per-fixture matrix can parse
# the route census emitted by the real executable (the integration test keeps
# its child-process output private by design).
run_cargo_case build-cli build --locked --manifest-path "$ROOT_DIR/Cargo.toml" --bin mimi

run_cargo_case library test --locked --manifest-path "$ROOT_DIR/Cargo.toml" --lib checked_get -- --nocapture --test-threads=1
run_cargo_case reference test --locked --manifest-path "$ROOT_DIR/Cargo.toml" --test "$REFERENCE_TARGET" checked_get -- --nocapture --test-threads=1
run_cargo_case cli-tests test --locked --manifest-path "$ROOT_DIR/Cargo.toml" --test "$CLI_TARGET" map_any_owner_v2_checked_get -- --nocapture --test-threads=1
run_cargo_case route-unit-boundaries test --locked --manifest-path "$ROOT_DIR/Cargo.toml" --bin mimi checked_get -- --nocapture --test-threads=1
run_cargo_case checker-bool-receipt test --locked --manifest-path "$ROOT_DIR/Cargo.toml" --lib ordinary_map_owned_any_v2_bool_unpack_requires_exact_get_any_set_receipt -- --nocapture --test-threads=1
run_cargo_case checker-unpack-api-boundaries test --locked --manifest-path "$ROOT_DIR/Cargo.toml" --test map_any_checked_unpack_contract checked_get_unpack -- --nocapture --test-threads=1
run_cargo_case cli-bool-values-negative test --locked --manifest-path "$ROOT_DIR/Cargo.toml" --test "$CLI_TARGET" map_any_owner_v2_bool_values_remains_fail_closed_without_legacy_retry -- --nocapture --test-threads=1
run_cargo_case mixed-owner-composition test --locked --manifest-path "$ROOT_DIR/Cargo.toml" --bin mimi scalar_ffi_composition_rejects_narrow_ordinary_map_any_v2_profiles -- --nocapture --test-threads=1

MIMI_BIN="$cargo_target_dir/debug/mimi"
if [[ -x "$MIMI_BIN" ]]; then
    sha256sum -- "$MIMI_BIN" > "$artifact_dir/mimi-binary.sha256"
else
    printf 'locked Cargo build did not produce executable %s\n' "$MIMI_BIN" \
        > "$artifact_dir/mimi-binary-unavailable.txt"
fi

run_cli_case() {
    local label="$1" action="$2" fixture="$3" mode="$4"; shift 4
    local directory="$artifact_dir/cli/$label/$action-$mode" status=0
    mkdir -p -- "$directory"
    local -a args=("$@")
    {
        printf 'cd %q && MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 %q' "$ROOT_DIR" "$MIMI_BIN"
        printf ' %q' "${args[@]}"
        printf '\n'
    } > "$directory/command.txt"
    printf '%s\0' "$MIMI_BIN" "${args[@]}" > "$directory/command.argv.nul"
    {
        printf 'cwd=%s\nMIMI_ROUTE_CENSUS=1\nMIMI_VERBOSE=1\nfixture=%s\nmode=%s\n' \
            "$ROOT_DIR" "$fixture" "$mode"
        for variable in PATH LLVM_SYS_181_PREFIX LD_LIBRARY_PATH RUSTUP_TOOLCHAIN RUSTFLAGS; do
            if [[ -v "$variable" ]]; then printf '%s=%q\n' "$variable" "${!variable}"; else printf '%s=<unset>\n' "$variable"; fi
        done
    } > "$directory/command.environment.txt"
    if (cd "$ROOT_DIR" && env MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 "$MIMI_BIN" "${args[@]}") \
        > "$directory/stdout.bin" 2> "$directory/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    printf '%s\n' "$status" > "$directory/command.exit"
    printf 'exit_code=%s\nsuccess=%s\n' "$status" \
        "$([[ "$status" -eq 0 ]] && printf true || printf false)" > "$directory/process.status"
    cat "$directory/stdout.bin" "$directory/stderr.bin" > "$directory/combined.log"
}

for fixture_case in settled-tail hit-bool multi-lookup multi-lookup-reversed; do
    case "$fixture_case" in
        settled-tail) fixture=tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_settled_branch_tail.mimi ;;
        hit-bool) fixture=tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_hit_bool.mimi ;;
        multi-lookup) fixture=tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_multi_lookup.mimi ;;
        multi-lookup-reversed) fixture=tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_multi_lookup_reversed.mimi ;;
    esac
    source="$ROOT_DIR/$fixture"
    mkdir -p "$artifact_dir/cli/$fixture_case"
    receipt_dir="$artifact_dir/cli/$fixture_case/read-only-receipt"
    mkdir -- "$receipt_dir"
    receipt_args=(mir --all --receipt "$source")
    {
        printf 'cd %q && %q' "$ROOT_DIR" "$MIMI_BIN"
        printf ' %q' "${receipt_args[@]}"
        printf '\n'
    } > "$receipt_dir/command.txt"
    printf '%s\0' "$MIMI_BIN" "${receipt_args[@]}" > "$receipt_dir/command.argv.nul"
    if (cd "$ROOT_DIR" && "$MIMI_BIN" "${receipt_args[@]}") \
        > "$receipt_dir/stdout.bin" 2> "$receipt_dir/stderr.bin"; then status=0; else status=$?; fi
    printf '%s\n' "$status" > "$receipt_dir/command.exit"
    cat "$receipt_dir/stdout.bin" "$receipt_dir/stderr.bin" > "$receipt_dir/combined.log"

    for mode in default explicit-mir; do
        if [[ "$mode" == default ]]; then explicit=(); else explicit=(--mir); fi
        run_cli_case "$fixture_case" run "$fixture" "$mode" run "${explicit[@]}" "$source"
        run_cli_case "$fixture_case" build "$fixture" "$mode" build "$source" --output \
            "$artifact_dir/cli/$fixture_case/build-$mode/program" "${explicit[@]}"
        run_cli_case "$fixture_case" verify "$fixture" "$mode" verify "${explicit[@]}" "$source"
    done
done

write_source_manifest "$artifact_dir/source-snapshot.after.manifest.sha256"
sha256sum -- "$artifact_dir/source-snapshot.after.manifest.sha256" | awk '{print $1}' \
    > "$artifact_dir/source-snapshot.after.sha256"
sha256sum -- tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_settled_branch_tail.mimi \
    tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_hit_bool.mimi \
    tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_multi_lookup.mimi \
    tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_multi_lookup_reversed.mimi \
    > "$artifact_dir/fixtures.after.sha256"
record_vcs "$artifact_dir/vcs.after.txt"

cat > "$artifact_dir/coverage.txt" <<EOF
profile=$PROFILE
cargo_incremental=$CARGO_INCREMENTAL
library_command=--lib checked_get
reference_target=$REFERENCE_TARGET
reference_filter=checked_get
reference_expected_tests=3
cli_target=$CLI_TARGET
cli_filter=map_any_owner_v2_checked_get
cli_expected_tests=6
library_expected_tests=5
route_unit_filter=checked_get
checker_bool_receipt_filter=ordinary_map_owned_any_v2_bool_unpack_requires_exact_get_any_set_receipt
checker_unpack_api_filter=checked_get_unpack
checker_unpack_api_expected_tests=4
cli_bool_values_negative=map_any_owner_v2_bool_values_remains_fail_closed_without_legacy_retry
mixed_owner_composition_filter=scalar_ffi_composition_rejects_narrow_ordinary_map_any_v2_profiles
library_test_names=exact_checked_get_receipt_authorizes_native_emission_without_opening_broad_v2,ordinary_map_owned_any_v2_bool_checked_get_receipt_is_exact_and_miss_bool_is_closed,ordinary_map_owned_any_v2_checked_get_multi_lookup_binds_each_receipt_and_rejects_tampering,ordinary_map_owned_any_v2_checked_get_i32_observation_and_string_label_drop_match_consumers,ordinary_map_owned_any_v2_checked_get_anchor_is_independent_and_fail_closed
cli_profile_cases=settled-tail,hit-bool,multi-lookup,multi-lookup-reversed
cli_paths_per_profile=default-run,explicit-mir-run,default-build,explicit-mir-build,default-verify,explicit-mir-verify
direct_cli_route_record_expected_count=24
global_map_any_v2_production_ready=false
EOF

validation_status=0
if python3 - "$artifact_dir" "$PROFILE" "$ROOT_DIR" "$tool_status" <<'PY'
import hashlib
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
profile = sys.argv[2]
repo = pathlib.Path(sys.argv[3])
tool_status = int(sys.argv[4])
errors = []

def read(path):
    return (root / path).read_text(errors="replace")

def check_cmd(label, expected_tests=None, exact_names=()):
    log = read(f"{label}/command.log")
    exit_code = int(read(f"{label}/command.exit").strip())
    if exit_code != 0:
        errors.append(f"{label} cargo exit was {exit_code}")
    summaries = re.findall(r"^test result: ok\. (\d+) passed; 0 failed; .* finished in [0-9.]+s$", log, re.M)
    if len(summaries) != 1:
        errors.append(f"{label} must have exactly one parsed passing test summary; observed={summaries}")
        count = None
    else:
        count = int(summaries[0])
        if count <= 0:
            errors.append(f"{label} passed zero tests")
        if expected_tests is not None and count != expected_tests:
            errors.append(f"{label} expected {expected_tests} passed tests, observed {count}")
    for name in exact_names:
        if len(re.findall(rf"^test (?:[A-Za-z0-9_]+::)*{re.escape(name)} \.\.\. ok$", log, re.M)) != 1:
            errors.append(f"{label} did not report exactly one passing test named {name}")
    return count

library_names = (
    "codegen::mir::tests::exact_checked_get_receipt_authorizes_native_emission_without_opening_broad_v2",
    "core::mir::route::tests::ordinary_map_owned_any_v2_bool_checked_get_receipt_is_exact_and_miss_bool_is_closed",
    "core::mir::route::tests::ordinary_map_owned_any_v2_checked_get_multi_lookup_binds_each_receipt_and_rejects_tampering",
    "interp::bytecode::mir::tests::ordinary_map_owned_any_v2_checked_get_i32_observation_and_string_label_drop_match_consumers",
    "interp::bytecode::mir::tests::ordinary_map_owned_any_v2_checked_get_anchor_is_independent_and_fail_closed",
)
library_count = check_cmd("library", 5, library_names)
reference_names = (
    "checked_get_i32_string_and_canonical_miss_unpack_execute_in_reference_and_bytecode",
    "checked_get_settled_branch_tail_has_a_receipted_route_and_executes_in_reference_mir",
    "checked_get_bool_has_a_receipted_route_and_copies_the_selected_payload",
)
reference_count = check_cmd("reference", 3, reference_names)
cli_names = (
    "map_any_owner_v2_checked_get_settled_branch_tail_uses_its_production_profile",
    "map_any_owner_v2_checked_get_bool_uses_its_production_profile",
    "map_any_owner_v2_checked_get_multi_lookup_keeps_each_lookup_receipt_independent",
    "map_any_owner_v2_checked_get_carried_result_diamond_stays_fail_closed",
    "map_any_owner_v2_checked_get_branch_tail_map_operation_stays_fail_closed",
    "map_any_owner_v2_checked_get_branch_tail_helper_call_stays_fail_closed",
)
cli_count = check_cmd("cli-tests", 6, cli_names)
route_unit_names = (
    "checked_get_static_keys_and_i32_string_label_payloads_pass_default_and_explicit_preflight",
    "checked_get_rejects_runtime_keys_type_mismatch_helpers_and_values",
    "checked_get_requires_exactly_one_explicit_pair_settlement",
    "checked_get_validator_rejects_forged_key_and_payload_selection_receipts",
)
route_unit_count = check_cmd("route-unit-boundaries", 4, route_unit_names)
checker_bool_count = check_cmd(
    "checker-bool-receipt", 1,
    ("ordinary_map_owned_any_v2_bool_unpack_requires_exact_get_any_set_receipt",),
)
checker_unpack_count = check_cmd(
    "checker-unpack-api-boundaries", 4,
    (
        "checked_get_unpack_intrinsics_require_exact_static_key_and_selected_type_receipts",
        "checked_get_unpack_uses_the_newest_set_for_the_exact_queried_key",
        "checked_get_unpack_intrinsics_reject_unrelated_any_and_wrong_arity",
        "checked_get_unpack_does_not_authorize_casts_values_or_shadowed_functions",
    ),
)
cli_bool_values_count = check_cmd(
    "cli-bool-values-negative", 1,
    ("map_any_owner_v2_bool_values_remains_fail_closed_without_legacy_retry",),
)
mixed_owner_composition_count = check_cmd(
    "mixed-owner-composition", 1,
    ("scalar_ffi_composition_rejects_narrow_ordinary_map_any_v2_profiles",),
)

readiness_source = (repo / "src/core/mir/route.rs").read_text()
if not re.search(r"ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY\s*:\s*bool\s*=\s*false\s*;", readiness_source):
    errors.append("global V2 readiness source constant is not explicitly false")

def test_body(source_text, test_name):
    match = re.search(r"#\[test\]\s*fn\s+" + re.escape(test_name) + r"\s*\(", source_text)
    if not match:
        return ""
    end = re.search(r"\n\s*#\[test\]", source_text[match.end():])
    return source_text[match.start():match.end() + end.start() if end else len(source_text)]

cli_source = (repo / "tests/canonical_mir_ordinary_map_any_owner_v2_cli.rs").read_text()
reference_source = (repo / "tests/canonical_mir_ordinary_map_any_v2_checked_get_branch_tail_reference.rs").read_text()
readiness_assertions = (
    (cli_source, "map_any_owner_v2_checked_get_settled_branch_tail_uses_its_production_profile",
     r"assert!\(\s*!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY"),
    (cli_source, "map_any_owner_v2_checked_get_bool_uses_its_production_profile",
     r"assert!\(\s*!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY"),
    (cli_source, "map_any_owner_v2_checked_get_multi_lookup_keeps_each_lookup_receipt_independent",
     r"assert!\(\s*!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY"),
    (reference_source, "checked_get_settled_branch_tail_has_a_receipted_route_and_executes_in_reference_mir",
     r"assert!\(\s*!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY"),
    (reference_source, "checked_get_bool_has_a_receipted_route_and_copies_the_selected_payload",
     r"assert!\(\s*!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY"),
)
for source_text, test_name, assertion_pattern in readiness_assertions:
    body = test_body(source_text, test_name)
    if not body or not re.search(assertion_pattern, body, re.S):
        errors.append(f"source-bound readiness=false assertion missing in test {test_name}")
if tool_status != 0:
    errors.append(f"toolchain capture had a nonzero exit aggregate: {tool_status}")

route_paths = (
    "default-run", "explicit-mir-run", "default-build", "explicit-mir-build",
    "default-verify", "explicit-mir-verify",
)
summary_rows = []
legacy_fallback_count = 0
for fixture_case in ("settled-tail", "hit-bool", "multi-lookup", "multi-lookup-reversed"):
    receipt = read(f"cli/{fixture_case}/read-only-receipt/stdout.bin")
    receipt_exit = int(read(f"cli/{fixture_case}/read-only-receipt/command.exit").strip())
    rows = receipt.splitlines()
    expected_fields = ("schema", "profile", "mir_digest", "type_desc_digest", "abi_digest", "ffi_digest", "ownership_digest", "flow_transition_digest", "root_owners")
    if receipt_exit != 0 or len(rows) != 10 or rows[0] != "mimi-mir-route-manifest-v1":
        errors.append(f"{fixture_case} read-only receipt did not have successful exact manifest framing")
        source_digest = ""
    else:
        fields = {}
        for expected, row in zip(expected_fields, rows[1:]):
            if "=" not in row:
                errors.append(f"{fixture_case} malformed read-only receipt row {row!r}")
                continue
            key, value = row.split("=", 1)
            if key != expected or key in fields:
                errors.append(f"{fixture_case} read-only receipt field order/set changed at {row!r}")
            fields[key] = value
        source_digest = fields.get("mir_digest", "")
        if fields.get("schema") != "mimi-mir-route-receipt-v1" or fields.get("profile") != "cli-mir-v1":
            errors.append(f"{fixture_case} read-only receipt schema/profile changed")
        if "function:main" not in fields.get("root_owners", ""):
            errors.append(f"{fixture_case} read-only receipt omits function:main")
        for name in expected_fields[2:8]:
            if not re.fullmatch(r"[0-9a-f]{64}", fields.get(name, "")):
                errors.append(f"{fixture_case} read-only receipt has malformed {name}")

    seen = set()
    shared_identity = None
    for path in route_paths:
        action, mode = path.split("-", 1)
        # The route path names use explicit-mir as one token; map the labels to recorded directories.
        if path in ("default-run", "explicit-mir-run"):
            action = "run"
            mode = "default" if path == "default-run" else "explicit-mir"
        elif path in ("default-build", "explicit-mir-build"):
            action = "build"
            mode = "default" if path == "default-build" else "explicit-mir"
        else:
            action = "verify"
            mode = "default" if path == "default-verify" else "explicit-mir"
        directory = f"cli/{fixture_case}/{action}-{mode}"
        log = read(f"{directory}/combined.log")
        status = int(read(f"{directory}/command.exit").strip())
        expected_status = 1 if action == "run" else 0
        if status != expected_status:
            errors.append(f"{fixture_case}/{path} expected CLI exit {expected_status}, observed {status}")
        legacy_found = bool(re.search(r"canonical route disposition:\s*legacy\b", log))
        if legacy_found:
            legacy_fallback_count += 1
            errors.append(f"{fixture_case}/{path} reported a Legacy fallback")
        route_lines = [line for line in log.splitlines() if line.startswith("MIMI_ROUTE_CENSUS route-admission ")]
        if len(route_lines) != 1:
            errors.append(f"{fixture_case}/{path} expected exactly one route-admission row, observed {len(route_lines)}")
            continue
        prefix = "MIMI_ROUTE_CENSUS route-admission "
        tokens = route_lines[0][len(prefix):].split()
        fields = {}
        for token in tokens:
            if token.count("=") != 1:
                errors.append(f"{fixture_case}/{path} malformed route token {token!r}")
                continue
            key, value = token.split("=", 1)
            if key in fields:
                errors.append(f"{fixture_case}/{path} repeated route field {key}")
            fields[key] = value
        expected_mode = "default" if mode == "default" else "explicit-mir"
        if action == "build":
            expected_route_order = [
                "consumer", "mode", "checker_profile", "build_route_profile",
                "mir_digest", "mir_route_identity_digest", "receipt_identity_digest",
                "build_route_identity_digest",
            ]
        else:
            expected_route_order = ["consumer", "mode", "checker_profile", "mir_digest", "receipt_identity_digest"]
        if [token.split("=", 1)[0] for token in tokens if token.count("=") == 1] != expected_route_order:
            errors.append(f"{fixture_case}/{path} route admission field order changed: {tokens}")
        if set(fields) != set(expected_route_order) or fields.get("consumer") != action or fields.get("mode") != expected_mode or fields.get("checker_profile") != profile:
            errors.append(f"{fixture_case}/{path} route admission fields/profile/mode differ: {fields}")
        digest = fields.get("mir_digest", "")
        identity = fields.get("receipt_identity_digest", "")
        if not re.fullmatch(r"[0-9a-f]{64}", digest) or digest != source_digest:
            errors.append(f"{fixture_case}/{path} MIR digest malformed or differs from source receipt")
        if not re.fullmatch(r"[0-9a-f]{64}", identity):
            errors.append(f"{fixture_case}/{path} route identity digest malformed")
        if action == "build":
            if fields.get("build_route_profile") != profile:
                errors.append(f"{fixture_case}/{path} build route profile differs from its Checker profile: {fields}")
            mir_identity = fields.get("mir_route_identity_digest", "")
            build_identity = fields.get("build_route_identity_digest", "")
            if not re.fullmatch(r"[0-9a-f]{64}", mir_identity):
                errors.append(f"{fixture_case}/{path} MIR route identity digest malformed")
            if not re.fullmatch(r"[0-9a-f]{64}", build_identity) or build_identity != identity:
                errors.append(f"{fixture_case}/{path} build route identity is malformed or differs from receipt identity")
        if shared_identity is None:
            shared_identity = identity
        elif identity != shared_identity:
            errors.append(f"{fixture_case}/{path} route identity differs across CLI consumers")
        dispositions = re.findall(r"^canonical route disposition: ([^\n]+)$", log, re.M)
        expected_disposition = "canonical" if mode == "default" else "explicit canonical"
        require_disposition = mode == "default" or action == "run"
        if require_disposition and len(dispositions) != 1:
            errors.append(f"{fixture_case}/{path} expected one canonical disposition, observed {dispositions}")
        if len(dispositions) > 1:
            errors.append(f"{fixture_case}/{path} emitted duplicate route dispositions: {dispositions}")
        if dispositions:
            disposition = re.fullmatch(
                r"(canonical|explicit canonical) \(" + re.escape(profile) + r"\) mir_digest=([0-9a-f]{64})",
                dispositions[0],
            )
            if not disposition or disposition.group(1) != expected_disposition or disposition.group(2) != digest:
                errors.append(f"{fixture_case}/{path} disposition does not bind expected profile/digest: {dispositions[0]}")
        if action == "build":
            native = [line for line in log.splitlines() if line.startswith("MIMI_ROUTE_CENSUS canonical-native-build ")]
            if len(native) != 1:
                errors.append(f"{fixture_case}/{path} expected one native-build route receipt, observed {len(native)}")
            else:
                build_tokens = native[0].split()[2:]
                build_fields = {}
                for token in build_tokens:
                    if token.count("=") != 1:
                        errors.append(f"{fixture_case}/{path} malformed native receipt token {token!r}")
                        continue
                    key, value = token.split("=", 1)
                    if key in build_fields:
                        errors.append(f"{fixture_case}/{path} repeated native receipt field {key}")
                    build_fields[key] = value
                build_order = [
                    "consumer", "route_id", "build_route_profile", "schema", "profile",
                    "mir_digest", "type_desc_digest", "abi_digest", "ffi_digest",
                    "ownership_digest", "flow_transition_digest", "root_owners",
                    "mir_route_identity_digest", "route_identity_digest",
                ]
                if [token.split("=", 1)[0] for token in build_tokens if token.count("=") == 1] != build_order:
                    errors.append(f"{fixture_case}/{path} native receipt field order/schema changed")
                expected_consumer = "default" if mode == "default" else "explicit-mir"
                if (
                    set(build_fields) != set(build_order)
                    or build_fields.get("consumer") != expected_consumer
                    or build_fields.get("route_id") != profile
                    or build_fields.get("build_route_profile") != fields.get("build_route_profile")
                    or build_fields.get("schema") != "mimi-mir-route-receipt-v1"
                    or build_fields.get("profile") != profile
                    or build_fields.get("mir_digest") != digest
                    or build_fields.get("mir_route_identity_digest") != fields.get("mir_route_identity_digest")
                    or build_fields.get("route_identity_digest") != identity
                    or build_fields.get("route_identity_digest") != fields.get("build_route_identity_digest")
                    or build_fields.get("root_owners") != "function:main"
                ):
                    errors.append(f"{fixture_case}/{path} native receipt not bound to its route admission: {build_fields}")
                for digest_field in ("mir_digest", "type_desc_digest", "abi_digest", "ffi_digest", "ownership_digest", "flow_transition_digest", "mir_route_identity_digest", "route_identity_digest"):
                    if not re.fullmatch(r"[0-9a-f]{64}", build_fields.get(digest_field, "")):
                        errors.append(f"{fixture_case}/{path} native receipt has malformed {digest_field}")
            binary = pathlib.Path(f"cli/{fixture_case}/build-{mode}/program")
            if not (root / binary).is_file() or not (root / binary).stat().st_mode & 0o111:
                errors.append(f"{fixture_case}/{path} did not create an executable native artifact")
        if action == "verify":
            provenance = [line for line in log.splitlines() if line.startswith("canonical MIR verifier provenance: ")]
            if len(provenance) != 1:
                errors.append(f"{fixture_case}/{path} expected one verifier provenance row, observed {len(provenance)}")
            else:
                provenance_tokens = provenance[0].split(": ", 1)[1].split()
                provenance_fields = dict(token.split("=", 1) for token in provenance_tokens if token.count("=") == 1)
                expected_provenance_order = ["profile", "mir_digest", "type_desc_digest", "abi_digest", "ffi_digest", "ownership_digest", "flow_transition_digest", "source_hash", "route_identity_digest"]
                if [token.split("=", 1)[0] for token in provenance_tokens if token.count("=") == 1] != expected_provenance_order:
                    errors.append(f"{fixture_case}/{path} verifier provenance schema/order changed")
                if set(provenance_fields) != set(expected_provenance_order) or provenance_fields.get("profile") != profile or provenance_fields.get("mir_digest") != digest or provenance_fields.get("route_identity_digest") != identity:
                    errors.append(f"{fixture_case}/{path} verifier provenance is not bound to route admission: {provenance_fields}")
                for digest_field in expected_provenance_order[1:]:
                    if not re.fullmatch(r"[0-9a-f]{64}", provenance_fields.get(digest_field, "")):
                        errors.append(f"{fixture_case}/{path} verifier provenance has malformed {digest_field}")
        if path in seen:
            errors.append(f"{fixture_case} duplicated route path {path}")
        seen.add(path)
        summary_rows.append(f"fixture={fixture_case} path={path} profile={profile} mir_digest={digest} receipt_identity_digest={identity} no_legacy_fallback={str(not legacy_found).lower()}")
    if seen != set(route_paths):
        errors.append(f"{fixture_case} route path census incomplete: {sorted(seen)}")

for before, after, label in (
    ("source-snapshot.before.manifest.sha256", "source-snapshot.after.manifest.sha256", "compilation source"),
    ("fixtures.before.sha256", "fixtures.after.sha256", "fixture"),
    ("vcs.before.txt", "vcs.after.txt", "VCS identity"),
):
    if (root / before).read_bytes() != (root / after).read_bytes():
        errors.append(f"{label} changed during capture")
for which in ("before", "after"):
    manifest = (root / f"source-snapshot.{which}.manifest.sha256").read_bytes()
    expected = hashlib.sha256(manifest).hexdigest()
    recorded = read(f"source-snapshot.{which}.sha256").strip()
    if expected != recorded:
        errors.append(f"{which} source snapshot digest does not bind its manifest")
source_manifest = (root / "source-snapshot.before.manifest.sha256").read_text().splitlines()
source_before = read("source-snapshot.before.sha256")
source_after = read("source-snapshot.after.sha256")

(root / "route-profile-summary.txt").write_text("\n".join(summary_rows) + "\n")
report = [
    "validation=" + ("PASS" if not errors else "FAIL"),
    "profile=CheckedGet",
    "level=L1/L2",
    f"route_profile={profile}",
    "global_map_any_v2_production_ready=false",
    f"library_checked_get_passed={library_count}",
    f"reference_passed={reference_count}/3",
    f"cli_checked_get_passed={cli_count}/6",
    f"route_unit_boundaries_passed={route_unit_count}/4",
    f"checker_bool_receipt_passed={checker_bool_count}/1",
    f"checker_unpack_api_boundaries_passed={checker_unpack_count}/4",
    f"cli_bool_values_fail_closed_passed={cli_bool_values_count}/1",
    f"mixed_owner_composition_passed={mixed_owner_composition_count}/1",
    f"direct_route_records={len(summary_rows)}/24",
    f"source_snapshot_files={len(source_manifest)}",
    f"source_snapshot_before_sha256={source_before}",
    f"source_snapshot_after_sha256={source_after}",
    f"legacy_fallback_count={legacy_fallback_count}",
]
report.extend("reason=" + error for error in errors)
(root / "artifact-validation.txt").write_text("\n".join(report) + "\n")
if errors:
    print("\n".join(report), file=sys.stderr)
    sys.exit(1)
print("\n".join(report))
PY

then
    validation_status=0
else
    validation_status=$?
fi

if [[ ! -f "$artifact_dir/artifact-validation.txt" ]]; then
    printf 'validation=FAIL\nprofile=CheckedGet\nlevel=L1/L2\nreason=validator failed before writing its report\n' \
        > "$artifact_dir/artifact-validation.txt"
fi

printf 'exit_code=%s\nsuccess=%s\n' "$validation_status" \
    "$([[ "$validation_status" -eq 0 ]] && printf true || printf false)" \
    > "$artifact_dir/runner-exit.status"

write_artifact_manifest() {
    (
        cd "$artifact_dir"
        if ! find . -type f \
            ! -name artifacts.sha256 \
            ! -name artifacts.sha256.sha256 \
            ! -name artifact-manifest.status \
            ! -name artifact-manifest-check.txt \
            -print0 | sort -z | xargs -0 -r "$SHA256SUM_BIN" > artifacts.sha256; then
            exit 2
        fi
        "$SHA256SUM_BIN" --check artifacts.sha256 > artifact-manifest-check.txt 2>&1
    )
}

artifact_manifest_status=0
if ! write_artifact_manifest; then
    artifact_manifest_status=1
    validation_status=1
    python3 - "$artifact_dir/artifact-validation.txt" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text()
text = text.replace("validation=PASS", "validation=FAIL", 1)
if "reason=artifact checksum manifest creation or validation failed" not in text:
    text += "reason=artifact checksum manifest creation or validation failed\n"
path.write_text(text)
PY
    printf 'exit_code=1\nsuccess=false\n' > "$artifact_dir/runner-exit.status"
    if ! write_artifact_manifest; then artifact_manifest_status=2; fi
fi
printf 'exit_code=%s\nsuccess=%s\n' "$artifact_manifest_status" \
    "$([[ "$artifact_manifest_status" -eq 0 ]] && printf true || printf false)" \
    > "$artifact_dir/artifact-manifest.status"
(
    cd "$artifact_dir"
    "$SHA256SUM_BIN" artifacts.sha256 artifact-manifest.status runner-exit.status \
        artifact-validation.txt > artifacts.sha256.sha256
    "$SHA256SUM_BIN" --check artifacts.sha256.sha256
)
if [[ "$artifact_manifest_status" -ne 0 ]]; then validation_status=1; fi
printf 'artifact_dir=%s\n' "$artifact_dir"
cat "$artifact_dir/artifact-validation.txt"
exit "$validation_status"
