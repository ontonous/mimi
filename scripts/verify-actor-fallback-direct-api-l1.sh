#!/usr/bin/env bash
# Execute the missing direct Rust API gate for the two original Actor
# fallback fixtures. Run this alongside verify-actor-two-fixture-current.sh;
# this narrow gate does not substitute for the CLI, census, or Memcheck gates.
set -uo pipefail
set -o noclobber

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
SCRIPT_PATH="$ROOT_DIR/scripts/verify-actor-fallback-direct-api-l1.sh"
TEST_ID="original_nested_actor_capture_and_shadow_use_shared_canonical_production_routes"
ARTIFACT_REQUESTED="${MIMI_ACTOR_DIRECT_API_ARTIFACT_DIR:-}"
TARGET_REQUESTED="${MIMI_ACTOR_DIRECT_API_TARGET_DIR:-}"
TARGET_CANONICAL=""
ACTOR_LLVM_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
ARTIFACT_DIR=""
GATE_STATUS=1

declare -A PINNED_SHA256=(
    [tests/real_world/actor_nested_func_capture.mimi]=12ad7930f779d1454cd90919c55d76e9a126c5dc8cabfdf748b5c99992f2483f
    [tests/real_world/actor_nested_func_shadow.mimi]=28144bad3ccf5018250ebf3ebaf8f5ad88006ba18cfbad5388fbd515a3fa0f7e
    [tests/canonical_actor_nested_fallback_production_route.rs]=a295963d4d9a9c68803bb95921a79b6e3fa618a32b1efefc92d349b6c27faee9
    [tests/real_world_cli.rs]=4a53de436667fc76a2bbc63cdffe7c79dfa3f8b5cec2ad1b69ea1bb6ac5362b4
    [scripts/verify-actor-two-fixture-current.sh]=0d07cf3826df2ffb99e5e336798a6d2452614e5b2a268c3cc9ac501addcfeaae
    [scripts/full_route_census.py]=cb0422e1670bae47a90d3a2a04d973976435ed6bcd18ffcadd79369a5d6bbfcc
    [scripts/actor_census_build_receipt.py]=37e4118b02b0432206827bcf114a7ace37acc5df2ac5b15f9f60274495355477
    [tests/scripts/test_actor_two_fixture_runner_contract.py]=08e8d7d82bc2f8acd0ff22a938fc7159b9f41b77ab9ecb1e609c30f829635107
    [tests/scripts/test_full_route_census.py]=413cbdf37bf4305da4c80271e1876f4f8854a8f95b48c77cd9abb1216c98a251
    [tests/scripts/test_actor_census_build_receipt.py]=7eb2a13e9c8280f6780b69de86b628e3aeb1ff24327eb649b6c8fa5ddff1abd7
)

fail_early() {
    printf 'FAIL %s\n' "$*" >&2
    exit 2
}

for required_tool in awk cmp date env find grep realpath rg sha256sum sort xargs rustup; do
    command -v "$required_tool" >/dev/null 2>&1 || \
        fail_early "Actor direct API gate requires $required_tool"
done

fail_gate() {
    local reason="$1"
    GATE_STATUS=1
    printf 'FAIL actor direct API L1: %s; partial evidence retained\n' "$reason" \
        >| "$ARTIFACT_DIR/result.txt"
    finalize_artifacts || true
    exit 1
}

finalize_artifacts() {
    (
        cd "$ARTIFACT_DIR" || exit 1
        find . -type f ! -name files.sha256 ! -name files.sha256.validation.txt \
            -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum >| files.sha256
        sha256sum --check files.sha256 >| files.sha256.validation.txt 2>&1
    )
}

on_exit() {
    local exit_status="$1"
    trap - EXIT
    if [[ -n "$ARTIFACT_DIR" && -d "$ARTIFACT_DIR" ]]; then
        if [[ ! -f "$ARTIFACT_DIR/result.txt" ]]; then
            printf 'FAIL actor direct API L1: runner exited before acceptance; partial evidence retained\n' \
                >| "$ARTIFACT_DIR/result.txt"
        elif [[ "$exit_status" -ne 0 ]] && grep -q '^status=PASS$' "$ARTIFACT_DIR/result.txt"; then
            printf 'FAIL actor direct API L1: runner exited with status %s after acceptance checks; partial evidence retained\n' \
                "$exit_status" >| "$ARTIFACT_DIR/result.txt"
        fi
        if ! finalize_artifacts; then
            exit_status=1
            printf 'FAIL actor direct API L1: final evidence manifest failed validation; partial evidence retained\n' \
                >| "$ARTIFACT_DIR/result.txt"
            finalize_artifacts || true
        fi
    fi
    exit "$exit_status"
}

if [[ -z "$ARTIFACT_REQUESTED" ]]; then
    fail_early "MIMI_ACTOR_DIRECT_API_ARTIFACT_DIR must name a fresh external evidence directory"
fi
if [[ "$ARTIFACT_REQUESTED" != /* ]]; then
    fail_early "MIMI_ACTOR_DIRECT_API_ARTIFACT_DIR must be absolute"
fi
if [[ -z "$TARGET_REQUESTED" || "$TARGET_REQUESTED" != /* ]]; then
    fail_early "MIMI_ACTOR_DIRECT_API_TARGET_DIR must name an absolute external Cargo target directory"
fi
TARGET_CANONICAL="$(realpath -m -- "$TARGET_REQUESTED")" || \
    fail_early "could not canonicalize the requested Cargo target directory"
if [[ "$TARGET_CANONICAL" == "$ROOT_DIR" || "$TARGET_CANONICAL" == "$ROOT_DIR/"* ]]; then
    fail_early "Cargo target directory must be outside the repository"
fi
if [[ ! -d "$ACTOR_LLVM_PREFIX" || -L "$ACTOR_LLVM_PREFIX" ]]; then
    fail_early "LLVM_SYS_181_PREFIX must name a real, non-symlink directory"
fi

ARTIFACT_DIR="$(realpath -m -- "$ARTIFACT_REQUESTED")" || \
    fail_early "could not canonicalize the requested artifact directory"
if [[ "$ARTIFACT_DIR" == "$ROOT_DIR" || "$ARTIFACT_DIR" == "$ROOT_DIR/"* ]]; then
    fail_early "Actor direct API evidence must remain outside the repository"
fi
if [[ -e "$ARTIFACT_DIR" || -L "$ARTIFACT_DIR" ]]; then
    fail_early "refusing to overwrite existing Actor direct API evidence: $ARTIFACT_DIR"
fi
case "$TARGET_CANONICAL/" in
    "$ARTIFACT_DIR/"*|"$ROOT_DIR/"*)
        fail_early "Cargo target directory overlaps Actor evidence or the repository" ;;
esac
case "$ARTIFACT_DIR/" in
    "$TARGET_CANONICAL/"*)
        fail_early "Actor evidence directory must not be inside the Cargo target directory" ;;
esac
mkdir -p -- "$(dirname "$ARTIFACT_DIR")" || \
    fail_early "cannot create Actor direct API evidence parent"
mkdir -- "$ARTIFACT_DIR" || fail_early "cannot reserve fresh evidence directory"
ARTIFACT_DIR="$(realpath -- "$ARTIFACT_DIR")" || exit 2
trap 'on_exit $?' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

write_hash_snapshot() {
    local output="$1" relative expected actual
    : > "$output" || return 1
    while IFS= read -r relative; do
        [[ -n "$relative" ]] || continue
        expected="${PINNED_SHA256[$relative]}"
        if [[ ! -f "$ROOT_DIR/$relative" || -L "$ROOT_DIR/$relative" ]]; then
            printf 'FAIL required pinned input is absent, non-regular, or symlinked: %s\n' \
                "$relative" >> "$output"
            return 1
        fi
        actual="$(sha256sum -- "$ROOT_DIR/$relative" | awk 'NR == 1 { print $1 }')" || return 1
        printf '%s expected=%s actual=%s\n' "$relative" "$expected" "$actual" >> "$output"
        if [[ "$actual" != "$expected" ]]; then
            printf 'FAIL pinned input hash mismatch: %s\n' "$relative" >> "$output"
            return 1
        fi
    done < <(printf '%s\n' "${!PINNED_SHA256[@]}" | LC_ALL=C sort)
}

script_sha_before="$(sha256sum -- "$SCRIPT_PATH" | awk 'NR == 1 { print $1 }')" || \
    fail_gate "could not hash this direct API gate script"
printf 'started_utc=%s\nrepository=%s\nscript_path=%s\nscript_sha256_before=%s\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$ROOT_DIR" "$SCRIPT_PATH" \
    "$script_sha_before" > "$ARTIFACT_DIR/environment.txt"
printf 'test_id=%s\nLLVM_SYS_181_PREFIX=%s\nMIMI_ACTOR_DIRECT_API_TARGET_DIR=%s\n' \
    "$TEST_ID" "$ACTOR_LLVM_PREFIX" "$TARGET_REQUESTED" \
    >> "$ARTIFACT_DIR/environment.txt"

if ! write_hash_snapshot "$ARTIFACT_DIR/source-pins.before.txt"; then
    fail_gate "source/test/script hash lock failed before execution"
fi

# The hardcoded input pins also protect the static assertions on which this
# missing direct lane relies. Fail closed if any named consumer contract has
# been changed without a fresh review and pin update.
for required_fragment in \
    'MirReferenceInterpreter::new' \
    'compile_mir_program_with_route_receipt' \
    'for explicit_mir in [false, true]' \
    'for (command_index, command) in ["run", "build", "verify"]' \
    'MIR_ACTOR_SPAWN' \
    'MIR_ACTOR_CALL' \
    '!opcodes.contains("MIR_ACTOR_FIELD_STORE")' \
    'canonical_actor_original_default_and_explicit_native_bins_pass_strict_memcheck_l3' \
    'record_gate actor-strict-l3' \
    'require_l3_memcheck_reports' \
    'capture-default|Memcheck actor_nested_func_capture.mimi (default route) report:' \
    'capture-explicit|Memcheck actor_nested_func_capture.mimi (explicit --mir) report:' \
    'shadow-default|Memcheck actor_nested_func_shadow.mimi (default route) report:' \
    'shadow-explicit|Memcheck actor_nested_func_shadow.mimi (explicit --mir) report:' \
    'full_route_census.py' \
    '--check' \
    "grep -qx 'actor_fixture_rows=CLOSED'"; do
    case "$required_fragment" in
        MirReferenceInterpreter*|compile_mir*|for\ *) source_path="tests/canonical_actor_nested_fallback_production_route.rs" ;;
        !opcodes.contains*|MIR_ACTOR_*) source_path="tests/real_world_cli.rs" ;;
        canonical_actor_original*) source_path="tests/real_world_cli.rs" ;;
        *) source_path="scripts/verify-actor-two-fixture-current.sh" ;;
    esac
    if ! rg -Fq -- "$required_fragment" "$ROOT_DIR/$source_path"; then
        fail_gate "pinned contract marker disappeared from $source_path: $required_fragment"
    fi
done

if ! command -v rustup >/dev/null 2>&1 || ! command -v env >/dev/null 2>&1; then
    fail_gate "rustup and env are required to select the recorded Cargo toolchain"
fi
if ! command -v rg >/dev/null 2>&1; then
    fail_gate "rg is required for the read-only coverage-marker preflight"
fi
cargo_path="$(rustup which cargo)" || fail_gate "rustup could not resolve Cargo"
cargo_path="$(realpath -e -- "$cargo_path")" || fail_gate "selected Cargo is unavailable"
if [[ "${cargo_path##*/}" != cargo || ! -f "$cargo_path" || ! -x "$cargo_path" ]]; then
    fail_gate "selected Cargo is not a canonical executable named cargo"
fi

route_unset_args=()
while IFS= read -r env_name; do
    [[ "$env_name" == MIMI_ROUTE_CENSUS* ]] && route_unset_args+=(-u "$env_name")
done < <(compgen -e)
cargo_env=(env "${route_unset_args[@]}" \
    "LLVM_SYS_181_PREFIX=$ACTOR_LLVM_PREFIX" \
    RUST_MIN_STACK=8388608 \
    CARGO_BUILD_JOBS=1 \
    CARGO_TERM_COLOR=never \
    "CARGO_TARGET_DIR=$TARGET_CANONICAL")
cargo_args=(test --locked --test canonical_actor_nested_fallback_production_route \
    "$TEST_ID" -- --exact --nocapture --test-threads=1)
{
    printf 'command='
    printf '%q ' "${cargo_env[@]}" "$cargo_path" "${cargo_args[@]}"
    printf '\n'
} > "$ARTIFACT_DIR/command.txt"
printf '%s\0' "$cargo_path" "${cargo_args[@]}" > "$ARTIFACT_DIR/argv.nul"
cd "$ROOT_DIR" || fail_gate "cannot enter repository root"
if "${cargo_env[@]}" "$cargo_path" "${cargo_args[@]}" \
    > "$ARTIFACT_DIR/stdout.bin" 2> "$ARTIFACT_DIR/stderr.bin"; then
    cargo_status=0
else
    cargo_status=$?
fi
printf 'exit_code=%s\n' "$cargo_status" > "$ARTIFACT_DIR/exit.txt"
{
    printf '===== stdout =====\n'
    cat "$ARTIFACT_DIR/stdout.bin"
    printf '\n===== stderr =====\n'
    cat "$ARTIFACT_DIR/stderr.bin"
} > "$ARTIFACT_DIR/combined.log"

if [[ "$cargo_status" -ne 0 ]]; then
    fail_gate "exact direct API integration test exited $cargo_status"
fi
if [[ "$(grep -Ec '^test result:' "$ARTIFACT_DIR/combined.log")" -ne 1 ]] ||
    ! grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out; finished in [0-9]+([.][0-9]+)?s$' \
        "$ARTIFACT_DIR/combined.log" ||
    [[ "$(grep -Ec "^test ${TEST_ID} \.\.\. ok$" "$ARTIFACT_DIR/combined.log")" -ne 1 ]]; then
    fail_gate "Cargo output did not contain exactly one successful selected direct API test"
fi

if ! write_hash_snapshot "$ARTIFACT_DIR/source-pins.after.txt"; then
    fail_gate "source/test/script hashes changed during direct API execution"
fi
script_sha_after="$(sha256sum -- "$SCRIPT_PATH" | awk 'NR == 1 { print $1 }')" || \
    fail_gate "could not hash this direct API gate script after execution"
if [[ "$script_sha_before" != "$script_sha_after" ]]; then
    fail_gate "direct API gate script changed during execution"
fi
if ! cmp -s "$ARTIFACT_DIR/source-pins.before.txt" "$ARTIFACT_DIR/source-pins.after.txt"; then
    fail_gate "locked source identities differ before and after execution"
fi
printf 'script_sha256_after=%s\n' "$script_sha_after" >> "$ARTIFACT_DIR/environment.txt"
GATE_STATUS=0
printf 'gate=actor-direct-api-l1\nstatus=PASS\ntest_id=%s\n' "$TEST_ID" \
    > "$ARTIFACT_DIR/result.txt"
if ! finalize_artifacts; then
    GATE_STATUS=1
    printf 'FAIL actor direct API L1: artifact hash manifest failed validation; partial evidence retained\n' \
        >| "$ARTIFACT_DIR/result.txt"
    finalize_artifacts || true
    exit 1
fi
printf 'artifact_dir=%s\nrun_status=%s\n' "$ARTIFACT_DIR" "$GATE_STATUS"
exit "$GATE_STATUS"
