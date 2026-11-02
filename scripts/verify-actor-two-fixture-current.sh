#!/usr/bin/env bash
# Narrow current-source gate for the two original Actor nested-helper fixtures.
# The full route census is retained as a source-bound collect/generate/strict
# replay; its strict global gate and the two exact Actor rows are both checked.
# This does not run or alter the broad FFI test gate; the full census retains
# all candidate outcomes and retains them without filtering rows.
set -uo pipefail
set -o noclobber

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_RELATIVE="scripts/verify-actor-two-fixture-current.sh"
# The portable contract is defined by the versioned runner, census/helper, and
# Python contract tests listed below. AGENTS.md and the two hand-off documents
# are non-contract review context: record them when present, but never require
# them or include them in source identity for clean-checkout replay.
LOCAL_CONTEXT_PATHS=(
    AGENTS.md
    devdocs/v0.41/actor-closure-0.1.11.md
    'devdocs/V0.2/09-0.1.11交接门禁.md'
)
LLVM_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
export LLVM_SYS_181_PREFIX="$LLVM_PREFIX"
# The production CLI uses the host's primary thread, which has the normal
# 8 MiB Linux stack here. Rust's test harness creates smaller worker threads
# by default; keep route-construction tests on the same stack budget.
TEST_THREAD_STACK_BYTES=8388608
ARTIFACT_REQUESTED="${MIMI_ACTOR_TWO_FIXTURE_ARTIFACT_DIR:-}"
EXPECTATIONS_REQUESTED="${MIMI_ACTOR_TWO_FIXTURE_EXPECTATIONS:-}"
GATE_FAILURES=0
SNAPSHOT_FAILURE=0
ACTOR_CENSUS_CLOSED=0
MIMI_BUILD_PASSED=0
ARTIFACT_DIR=""
ISOLATED_MIMI_DIR=""
ISOLATED_MIMI_PATH=""

fail_early() {
    printf '%s\n' "$*" >&2
    exit 2
}

for required_tool in cargo rustc rustup python3 sha256sum sort find xargs cmp awk grep realpath date git uname env; do
    command -v "$required_tool" >/dev/null 2>&1 || fail_early "Actor two-fixture replay requires $required_tool"
done
NATIVE_HOST_TARGET="$(rustc -vV | awk '$1 == "host:" { print $2; exit }')"
[[ -n "$NATIVE_HOST_TARGET" ]] || fail_early "could not resolve the active Rust host target"
if [[ ${MIMI_ACTOR_TWO_FIXTURE_TARGET_DIR+x} ]]; then
    [[ -n "$MIMI_ACTOR_TWO_FIXTURE_TARGET_DIR" ]] || \
        fail_early "MIMI_ACTOR_TWO_FIXTURE_TARGET_DIR must not be empty"
    [[ "$MIMI_ACTOR_TWO_FIXTURE_TARGET_DIR" == /* ]] || \
        fail_early "MIMI_ACTOR_TWO_FIXTURE_TARGET_DIR must be an absolute path"
    TARGET_DIR_REQUESTED="$MIMI_ACTOR_TWO_FIXTURE_TARGET_DIR"
else
    TARGET_DIR_REQUESTED="$ROOT_DIR/target"
fi
EFFECTIVE_TARGET_DIR="$(realpath -m -- "$TARGET_DIR_REQUESTED")" || \
    fail_early "could not normalize Cargo target directory: $TARGET_DIR_REQUESTED"

if [[ -z "$ARTIFACT_REQUESTED" ]]; then
    ARTIFACT_REQUESTED="evidence/0.1.11-dev/gates/actor-two-fixture-current-$(date -u +%Y%m%dT%H%M%SZ)-$$"
fi
case "$ARTIFACT_REQUESTED" in
    /*) artifact_candidate="$ARTIFACT_REQUESTED" ;;
    *) artifact_candidate="$ROOT_DIR/$ARTIFACT_REQUESTED" ;;
esac
if [[ -e "$artifact_candidate" || -L "$artifact_candidate" ]]; then
    fail_early "refusing to overwrite existing Actor two-fixture artifact path: $artifact_candidate"
fi
ARTIFACT_DIR="$(realpath -m -- "$artifact_candidate")"
case "$ARTIFACT_DIR/" in
    "$ROOT_DIR/evidence/0.1.11-dev/gates/"*) ;;
    "$ROOT_DIR/"*) fail_early "in-repository evidence must be under evidence/0.1.11-dev/gates: $ARTIFACT_DIR" ;;
esac
mkdir -p -- "$(dirname "$ARTIFACT_DIR")" || fail_early "cannot create artifact parent directory"
mkdir -- "$ARTIFACT_DIR" || fail_early "cannot create new artifact directory: $ARTIFACT_DIR"
ARTIFACT_DIR="$(realpath -- "$ARTIFACT_DIR")"
mkdir -- "$ARTIFACT_DIR/commands" || exit 2

cd "$ROOT_DIR" || exit 2

write_failure_state() {
    printf 'FAIL %s; partial evidence retained\n' "$1" >| "$ARTIFACT_DIR/result.txt"
}

finish_artifact() {
    local requested_status="$1"
    local manifest_status=0
    if [[ ! -e "$ARTIFACT_DIR/result.txt" ]]; then
        write_failure_state "runner stopped before all gates completed"
    fi
    (
        cd "$ARTIFACT_DIR" || exit 1
        find . -type f ! -name files.sha256 ! -name files.sha256.sha256 \
            ! -name files.sha256.validation.txt -print0 \
            | sort -z | xargs -0 -r sha256sum > files.sha256
        sha256sum --check files.sha256
        sha256sum files.sha256 > files.sha256.sha256
        sha256sum --check files.sha256.sha256
    ) > "$ARTIFACT_DIR/files.sha256.validation.txt" 2>&1 || manifest_status=$?
    if [[ "$manifest_status" -ne 0 ]]; then
        printf 'FAIL artifact manifest generation or verification failed\n' >| "$ARTIFACT_DIR/result.txt"
        requested_status=1
    fi
    return "$requested_status"
}

on_exit() {
    local status="$1"
    trap - EXIT
    finish_artifact "$status"
    exit $?
}
trap 'on_exit $?' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

capture_environment() {
    local context_path
    {
        printf 'started_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf 'repository=%s\n' "$ROOT_DIR"
        printf 'git_head=%s\n' "$(git rev-parse HEAD 2>/dev/null || printf '<unavailable>')"
        printf 'git_branch=%s\n' "$(git symbolic-ref --quiet --short HEAD 2>/dev/null || printf '<detached-or-unavailable>')"
        printf 'host=%s\n' "$(uname -a)"
        printf 'native_host_target=%s\n' "$NATIVE_HOST_TARGET"
        printf 'LLVM_SYS_181_PREFIX=%q\n' "$LLVM_PREFIX"
        printf 'effective.RUST_MIN_STACK=%s\n' "$TEST_THREAD_STACK_BYTES"
        if [[ -e "$LLVM_PREFIX" ]]; then
            printf 'LLVM_SYS_181_PREFIX_realpath=%s\n' "$(realpath -- "$LLVM_PREFIX")"
        else
            printf 'LLVM_SYS_181_PREFIX_realpath=<missing>\n'
        fi
        printf 'effective.CARGO_BUILD_JOBS=1\n'
        printf 'effective.CARGO_TERM_COLOR=never\n'
        printf 'effective.CARGO_TARGET_DIR=%q\n' "$EFFECTIVE_TARGET_DIR"
        printf 'local_context_policy=optional_review_context_excluded_from_portable_source_identity\n'
        for context_path in "${LOCAL_CONTEXT_PATHS[@]}"; do
            if [[ -L "$context_path" ]]; then
                printf 'local_context.%s=symlink_excluded\n' "$context_path"
            elif [[ -f "$context_path" ]]; then
                printf 'local_context.%s=present_sha256=' "$context_path"
                sha256sum -- "$context_path" | awk '{ print $1 }'
            else
                printf 'local_context.%s=absent\n' "$context_path"
            fi
        done
        for name in CARGO_HOME CARGO_TARGET_DIR CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_TARGET RUST_MIN_STACK \
            CARGO_BUILD_JOBS CARGO_INCREMENTAL CARGO_NET_OFFLINE CARGO_TERM_COLOR \
            CARGO_PROFILE_DEV_OPT_LEVEL CARGO_PROFILE_TEST_OPT_LEVEL \
            CARGO_PROFILE_DEV_DEBUG CARGO_PROFILE_TEST_DEBUG \
            RUSTFLAGS RUSTDOCFLAGS RUSTUP_TOOLCHAIN RUSTC RUSTC_WRAPPER \
            RUSTC_WORKSPACE_WRAPPER RUSTDOC CC CFLAGS CXX CXXFLAGS AR LD \
            LIBCLANG_PATH LLVM_CONFIG_PATH PKG_CONFIG_PATH PKG_CONFIG_LIBDIR \
            LD_LIBRARY_PATH MIMI_ROUTE_CENSUS MIMI_ROUTE_CENSUS_MIMI \
            MIMI_ROUTE_CENSUS_MIMI_SHA256 MIMI_ROUTE_CENSUS_AUTHORITY_SOURCE \
            MIMI_ACTOR_TWO_FIXTURE_EXPECTATIONS \
            MIMI_REACHABLE_DISPATCH PATH HOME; do
            if [[ ${!name+x} ]]; then
                printf 'env.%s=%q\n' "$name" "${!name}"
            else
                printf 'env.%s=<unset>\n' "$name"
            fi
        done
        rustc --version --verbose
        python3 --version
        if command -v valgrind >/dev/null 2>&1; then
            valgrind --version
        else
            printf 'valgrind=<unavailable>\n'
        fi
        if [[ -x "$LLVM_PREFIX/bin/llvm-config" ]]; then
            printf 'llvm_config_path=%s\n' "$(realpath -- "$LLVM_PREFIX/bin/llvm-config")"
            sha256sum -- "$LLVM_PREFIX/bin/llvm-config"
            "$LLVM_PREFIX/bin/llvm-config" --version
        else
            printf 'llvm_config_path=<unavailable>\n'
        fi
    } > "$ARTIFACT_DIR/replay-environment.txt" 2>&1
}

capture_environment || {
    write_failure_state "could not capture replay environment"
    exit 1
}

# Require a caller-reviewed expectation contract before any Cargo invocation
# (including the version probe) or Mimi build. Observations from the first census can validate this manifest;
# they cannot supply or auto-approve its expected classes.
if [[ -z "$EXPECTATIONS_REQUESTED" ]]; then
    fail_early "MIMI_ACTOR_TWO_FIXTURE_EXPECTATIONS must name an independent schema-v4 expectations manifest; refusing raw-observation authority generation"
fi
case "$EXPECTATIONS_REQUESTED" in
    /*) expectations_candidate="$EXPECTATIONS_REQUESTED" ;;
    *) expectations_candidate="$ROOT_DIR/$EXPECTATIONS_REQUESTED" ;;
esac
if [[ -L "$expectations_candidate" || ! -f "$expectations_candidate" ]]; then
    fail_early "independent expectations manifest must be a regular non-symlink file: $expectations_candidate"
fi
CENSUS_EXPECTATIONS_PATH="$(realpath -e -- "$expectations_candidate")" || \
    fail_early "could not resolve independent expectations manifest: $expectations_candidate"
CENSUS_EXPECTATIONS_SHA256="$(sha256sum -- "$CENSUS_EXPECTATIONS_PATH" | awk 'NR == 1 { print $1 }')"
[[ "$CENSUS_EXPECTATIONS_SHA256" =~ ^[0-9a-f]{64}$ ]] || \
    fail_early "could not hash independent expectations manifest"
if ! python3 - "$ROOT_DIR" "$CENSUS_EXPECTATIONS_PATH" <<'PY' > "$ARTIFACT_DIR/expectations-preflight.json"; then
import json
import sys
from pathlib import Path

root = Path(sys.argv[1]).resolve(strict=True)
manifest_path = Path(sys.argv[2]).resolve(strict=True)
sys.path.insert(0, str(root / "scripts"))
import full_route_census as census

try:
    declared, identity = census.load_expectations(manifest_path, root)
    tracked, _tracked_identity = census.tracked_mimi_paths(root)
    sources, _exclusions, scope = census.enumerate_source_scope(root, tracked)
    source_paths = {str(row["path"]) for row in sources}
    if set(declared) != source_paths:
        missing = sorted(source_paths - set(declared))
        extra = sorted(set(declared) - source_paths)
        raise ValueError(
            "schema-v4 expectations must cover the exact current denominator "
            f"(missing={len(missing)} first={missing[:3]}, extra={len(extra)} first={extra[:3]})"
        )
    for source in sources:
        path = str(source["path"])
        expectation = declared[path]
        if expectation.get("class") == "unmapped":
            raise ValueError(f"independent expectations cannot leave a candidate unmapped: {path}")
        if expectation.get("source_sha256") != source.get("sha256"):
            raise ValueError(f"independent expectation source hash is stale: {path}")
    bound, coverage = census.bind_expectations_to_inventory(declared, sources, root)
    invalid = sorted(
        path for path, expectation in bound.items()
        if not census.expectation_source_contract_valid_for_preflight(expectation)
    )
    if invalid:
        raise ValueError(
            "independent expectations contain invalid source-bound test contract "
            f"({len(invalid)} first={invalid[:5]})"
        )
    runtime_pending = census.expectation_paths_requiring_runtime_authority(bound)
    print(json.dumps({
        "status": "validated_independent_v4_manifest",
        "path": str(manifest_path),
        "sha256": identity["sha256"],
        "candidate_count": len(sources),
        "manifest_sha256": census._manifest_digest(sources),
        "source_scope_exactly_once": scope.get("exactly_once"),
        "expectation_coverage_exact_bijection": coverage.get("exact_bijection"),
        "source_contract_invalid_paths": [],
        "runtime_authority_pending_count": len(runtime_pending),
        "runtime_authority_pending_first": runtime_pending[:5],
    }, sort_keys=True))
except (OSError, ValueError, json.JSONDecodeError) as error:
    print(f"expectations preflight refused: {error}", file=sys.stderr)
    raise SystemExit(2)
PY
    fail_early "independent schema-v4 expectations preflight failed; census and authority generator were not run"
fi
cargo --version >> "$ARTIFACT_DIR/replay-environment.txt" 2>&1 || {
    write_failure_state "could not capture Cargo version after expectations preflight"
    exit 1
}
{
    printf 'path=%s\nsha256=%s\n' "$CENSUS_EXPECTATIONS_PATH" "$CENSUS_EXPECTATIONS_SHA256"
} > "$ARTIFACT_DIR/expectations-manifest.identity.txt"
printf 'PASS independent schema-v4 expectations validated before Cargo tests and census\n' \
    > "$ARTIFACT_DIR/expectations-manifest.validation.txt"

source_input_inventory() {
    local root source_path required_path optional_path
    local -a required_sources=(
        Cargo.toml
        Cargo.lock
        build.rs
        "$SCRIPT_RELATIVE"
        scripts/full_route_census.py
        scripts/actor_census_build_receipt.py
        scripts/generate_route_census_authority.py
        scripts/audit-mir-legacy-owners.sh
        src/core/mir/actor_tests.rs
        src/core/mir/route.rs
        src/core/resolved/tests.rs
        src/main/canonical_dispatch.rs
        src/tests/dual_backend.rs
        tests/real_world_cli.rs
        tests/scripts/test_full_route_census.py
        tests/scripts/test_actor_census_build_receipt.py
        tests/scripts/test_actor_two_fixture_runner_contract.py
        tests/real_world/actor_nested_func_capture.mimi
        tests/real_world/actor_nested_func_shadow.mimi
    )
    local -a roots=(src tests std)

    {
        for root in "${roots[@]}"; do
            if [[ ! -d "$root" || -L "$root" ]]; then
                printf 'required source root missing or symlinked: %s\n' "$root" >&2
                return 1
            fi
            source_path="$(find "$root" -type l -print -quit)" || return 1
            if [[ -n "$source_path" ]]; then
                printf 'source tree contains an unsupported symlink: %s\n' "$source_path" >&2
                return 1
            fi
            find "$root" -type f -print0 || return 1
        done

        if [[ -L .cargo ]]; then
            printf 'Cargo config directory must not be a symlink\n' >&2
            return 1
        elif [[ -d .cargo ]]; then
            source_path="$(find .cargo -type l -print -quit)" || return 1
            if [[ -n "$source_path" ]]; then
                printf 'Cargo config tree contains an unsupported symlink: %s\n' "$source_path" >&2
                return 1
            fi
            find .cargo -type f -print0 || return 1
        fi

        for required_path in "${required_sources[@]}"; do
            if [[ ! -f "$required_path" || -L "$required_path" ]]; then
                printf 'required source input missing or symlinked: %s\n' "$required_path" >&2
                return 1
            fi
            printf '%s\0' "$required_path"
        done

        for optional_path in rust-toolchain rust-toolchain.toml; do
            if [[ -L "$optional_path" ]]; then
                printf 'toolchain input must not be a symlink: %s\n' "$optional_path" >&2
                return 1
            elif [[ -f "$optional_path" ]]; then
                printf '%s\0' "$optional_path"
            fi
        done
    } | LC_ALL=C sort -zu
}

source_snapshot() {
    local inventory_path="$1" snapshot_path="$2"
    local verify_inventory_path="${inventory_path}.verify.tmp"
    local snapshot_tmp="${snapshot_path}.tmp"
    local source_path
    source_input_inventory > "$inventory_path" || return 1
    : > "$snapshot_tmp" || return 1
    while IFS= read -r -d '' source_path; do
        if [[ ! -f "$source_path" || -L "$source_path" ]] ||
            ! sha256sum -- "$source_path" >> "$snapshot_tmp"; then
            printf 'source input disappeared, changed type, or could not be hashed: %s\n' "$source_path" >&2
            rm -f -- "$snapshot_tmp" "$verify_inventory_path"
            return 1
        fi
    done < "$inventory_path" || return 1
    if ! source_input_inventory > "$verify_inventory_path" ||
        ! cmp -s "$inventory_path" "$verify_inventory_path"; then
        printf 'source input path inventory changed while hashing\n' >&2
        rm -f -- "$snapshot_tmp" "$verify_inventory_path"
        return 1
    fi
    rm -- "$verify_inventory_path" || return 1
    mv -- "$snapshot_tmp" "$snapshot_path" || return 1
}

validate_cargo_target_configuration() {
    local host_target="$1" report_path="$2" status
    if python3 - "$ROOT_DIR" "$host_target" > "$report_path" 2>&1 <<'PY'
import hashlib
import json
import os
import sys
from pathlib import Path

root = Path(sys.argv[1]).resolve()
host = sys.argv[2]
issues = []
print(f"native_host_target={host}")

environment_target = os.environ.get("CARGO_BUILD_TARGET")
if environment_target is None:
    print("env.CARGO_BUILD_TARGET=<unset>")
else:
    print(f"env.CARGO_BUILD_TARGET={json.dumps(environment_target)}")
    if environment_target != host:
        issues.append(f"CARGO_BUILD_TARGET selects {environment_target!r}, expected native host {host!r}")

config_paths = []
for directory in (root, *root.parents):
    cargo_dir = directory / ".cargo"
    for name in ("config.toml", "config"):
        candidate = cargo_dir / name
        if candidate.is_file():
            config_paths.append(candidate)

cargo_home = os.environ.get("CARGO_HOME")
if cargo_home == "":
    issues.append("CARGO_HOME is set to an empty path")
    cargo_home_path = root / ".cargo-home-unavailable"
elif cargo_home:
    cargo_home_path = Path(cargo_home)
    if not cargo_home_path.is_absolute():
        cargo_home_path = root / cargo_home_path
else:
    cargo_home_path = Path(os.path.expanduser("~")) / ".cargo"
for name in ("config.toml", "config"):
    candidate = cargo_home_path / name
    if candidate.is_file() and candidate not in config_paths:
        config_paths.append(candidate)

toml = None
if config_paths:
    try:
        import tomllib as toml
    except ImportError:
        try:
            import tomli as toml
        except ImportError:
            issues.append("Python tomllib/tomli is required to inspect Cargo target configuration")

if toml is not None:
    for path in config_paths:
        try:
            raw = path.read_bytes()
            config = toml.loads(raw.decode("utf-8"))
        except (OSError, UnicodeError, ValueError) as error:
            issues.append(f"could not parse Cargo config {path}: {error}")
            continue
        digest = hashlib.sha256(raw).hexdigest()
        build = config.get("build", {})
        if not isinstance(build, dict):
            issues.append(f"Cargo config {path} has a non-table [build] value")
            continue
        configured_target = build.get("target")
        print(f"cargo_config={json.dumps(str(path))} sha256={digest}")
        if configured_target is None:
            print("  build.target=<unset>")
            continue
        if isinstance(configured_target, str):
            targets = [configured_target]
        elif isinstance(configured_target, list) and all(isinstance(item, str) for item in configured_target):
            targets = configured_target
        else:
            issues.append(f"Cargo config {path} has an unsupported build.target value")
            continue
        if not targets:
            issues.append(f"Cargo config {path} has an empty build.target list")
        for target in targets:
            print(f"  build.target={json.dumps(target)}")
            if target != host:
                issues.append(f"Cargo config {path} selects {target!r}, expected native host {host!r}")

if not config_paths:
    print("cargo_config_build.target=<unset>")
if issues:
    for issue in issues:
        print(f"FAIL {issue}")
    raise SystemExit(1)
print("PASS Cargo target settings select the native host or are unset")
PY
    then
        return 0
    else
        status=$?
        printf 'FAIL could not validate native Cargo target configuration (status=%s)\n' "$status" \
            >> "$report_path"
        return 1
    fi
}

source_snapshot "$ARTIFACT_DIR/source.inputs.before.nul" "$ARTIFACT_DIR/source.before.sha256" || {
    write_failure_state "could not capture before source snapshot"
    exit 1
}
sha256sum "$ARTIFACT_DIR/source.inputs.before.nul" > "$ARTIFACT_DIR/source.before.inventory.sha256"
sha256sum "$ARTIFACT_DIR/source.before.sha256" > "$ARTIFACT_DIR/source.before.manifest.sha256"

if ! validate_cargo_target_configuration "$NATIVE_HOST_TARGET" \
    "$ARTIFACT_DIR/cargo-target-configuration.txt"; then
    GATE_FAILURES=$((GATE_FAILURES + 1))
    printf 'FAIL native evidence gate rejected cross-target or unreadable Cargo target configuration; see cargo-target-configuration.txt\n' \
        >| "$ARTIFACT_DIR/result.txt"
    exit 1
fi

record_gate() {
    local label="$1" expected_test="$2" expected_passed="$3"
    shift 3
    local gate_dir="$ARTIFACT_DIR/commands/$label"
    local cargo_path status result_line summary_count validation_status=0
    local expected_pattern
    local -a cargo_args=("$@")
    if [[ "${cargo_args[0]:-}" == "test" ]]; then
        cargo_args=(test --locked "${cargo_args[@]:1}")
    fi
    mkdir -- "$gate_dir" || {
        printf 'FAIL could not create command evidence directory: %s\n' "$gate_dir" >&2
        GATE_FAILURES=$((GATE_FAILURES + 1))
        return 1
    }
    cargo_path="$(rustup which cargo)" || {
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL rustup could not resolve Cargo for the selected toolchain\n' \
            > "$gate_dir/validation.txt"
        return 1
    }
    cargo_path="$(realpath -e -- "$cargo_path")" || {
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL selected rustup Cargo path is unavailable\n' > "$gate_dir/validation.txt"
        return 1
    }
    if [[ "${cargo_path##*/}" != cargo || ! -f "$cargo_path" || ! -x "$cargo_path" ]]; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL selected rustup Cargo is not a canonical executable named cargo\n' \
            > "$gate_dir/validation.txt"
        return 1
    fi
    {
        printf 'RUST_MIN_STACK=%q LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 CARGO_TERM_COLOR=never CARGO_TARGET_DIR=%q %q ' \
            "$TEST_THREAD_STACK_BYTES" "$LLVM_PREFIX" "$EFFECTIVE_TARGET_DIR" "$cargo_path"
        printf '%q ' "${cargo_args[@]}"
        printf '\n'
    } > "$gate_dir/command.txt"
    printf '%s\0' "$cargo_path" "${cargo_args[@]}" > "$gate_dir/argv.nul"
    {
        printf 'RUST_MIN_STACK=%s\nLLVM_SYS_181_PREFIX=%s\n' \
            "$TEST_THREAD_STACK_BYTES" "$LLVM_PREFIX"
        printf 'CARGO_BUILD_JOBS=1\nCARGO_TERM_COLOR=never\nCARGO_TARGET_DIR=%q\n' "$EFFECTIVE_TARGET_DIR"
        printf 'working_directory=%s\n' "$ROOT_DIR"
        printf 'expected_test_id=%s\nexpected_passed=%s\n' "$expected_test" "$expected_passed"
    } > "$gate_dir/environment.txt"
    if env RUST_MIN_STACK="$TEST_THREAD_STACK_BYTES" LLVM_SYS_181_PREFIX="$LLVM_PREFIX" \
        CARGO_BUILD_JOBS=1 CARGO_TERM_COLOR=never \
        CARGO_TARGET_DIR="$EFFECTIVE_TARGET_DIR" \
        "$cargo_path" "${cargo_args[@]}" > "$gate_dir/stdout.bin" 2> "$gate_dir/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    printf 'exit_code=%s\n' "$status" > "$gate_dir/exit.txt"
    {
        printf '===== stdout =====\n'
        cat "$gate_dir/stdout.bin"
        printf '\n===== stderr =====\n'
        cat "$gate_dir/stderr.bin"
    } > "$gate_dir/combined.log"

    summary_count="$(awk '/^test result:/ { count++ } END { print count + 0 }' "$gate_dir/combined.log")"
    result_line="$(awk '/^test result:/ { result = $0 } END { print result }' "$gate_dir/combined.log")"
    expected_pattern="^test result: ok[.] ${expected_passed} passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out; finished in [0-9]+([.][0-9]+)?s$"
    if [[ "$status" -ne 0 ]]; then
        validation_status=1
        printf 'FAIL cargo exit=%s\n' "$status" > "$gate_dir/validation.txt"
    elif [[ "$summary_count" -ne 1 || ! "$result_line" =~ $expected_pattern ]]; then
        validation_status=1
        printf 'FAIL expected one exact all-passing test summary\nobserved_count=%s\nobserved=%s\n' \
            "$summary_count" "$result_line" > "$gate_dir/validation.txt"
    elif ! awk -v expected_test="$expected_test" '
        $1 == "test" && $2 == expected_test {
            found++
            if ($NF != "ok") failed = 1
        }
        END { exit !(found == 1 && !failed) }
    ' "$gate_dir/combined.log"; then
        validation_status=1
        printf 'FAIL exact authority test was absent, duplicated, or not ok: %s\n' \
            "$expected_test" > "$gate_dir/validation.txt"
    else
        printf 'PASS exact test %s; %s\n' "$expected_test" "$result_line" > "$gate_dir/validation.txt"
    fi
    cat "$gate_dir/stdout.bin"
    cat "$gate_dir/stderr.bin" >&2
    if [[ "$validation_status" -ne 0 ]]; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
        return 1
    fi
    return 0
}

record_build() {
    local label="$1"
    shift
    local gate_dir="$ARTIFACT_DIR/commands/$label"
    local cargo_path python_path helper_path status receipt_status validation_status=0
    local build_target_dir host_target
    local -a cargo_args=("$@")
    local -a cargo_env cargo_argv receipt_command
    mkdir -- "$gate_dir" || {
        printf 'FAIL could not create command evidence directory: %s\n' "$gate_dir" >&2
        GATE_FAILURES=$((GATE_FAILURES + 1))
        return 1
    }
    cargo_path="$(rustup which cargo)" || {
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL rustup could not resolve Cargo for the selected toolchain\n' \
            > "$gate_dir/validation.txt"
        return 1
    }
    cargo_path="$(realpath -e -- "$cargo_path")" || {
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL selected rustup Cargo path is unavailable\n' \
            > "$gate_dir/validation.txt"
        return 1
    }
    if [[ "${cargo_path##*/}" != cargo || ! -f "$cargo_path" || ! -x "$cargo_path" ]]; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL selected rustup Cargo is not a canonical executable named cargo\n' \
            > "$gate_dir/validation.txt"
        return 1
    fi
    build_target_dir="$EFFECTIVE_TARGET_DIR/actor-two-fixture-current-cargo-${BASHPID:-$$}"
    if [[ "${cargo_args[0]:-}" != "build" ]]; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL current-source binary recorder requires a cargo build command\n' \
            >> "$gate_dir/validation.txt"
        return 1
    fi
    host_target="$NATIVE_HOST_TARGET"
    if ! validate_cargo_target_configuration "$host_target" "$gate_dir/cargo-target-configuration.txt"; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL Cargo target configuration changed or no longer selects the native host\n' \
            >> "$gate_dir/validation.txt"
        return 1
    fi
    if [[ -e "$build_target_dir" || -L "$build_target_dir" ]] ||
        ! mkdir -- "$build_target_dir"; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL could not reserve a fresh isolated Cargo target directory: %s\n' \
            "$build_target_dir" >> "$gate_dir/validation.txt"
        return 1
    fi
    build_target_dir="$(realpath -- "$build_target_dir")" || {
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL could not canonicalize fresh Cargo target directory\n' \
            >> "$gate_dir/validation.txt"
        return 1
    }
    printf 'target_path=%s\ndirectory_existed_before=%s\ncreated_exclusively=true\n' \
        "$build_target_dir" false \
        > "$gate_dir/fresh-target.txt"
    cargo_args+=(--target "$host_target" --message-format=json-render-diagnostics)
    cargo_argv=("$cargo_path" "${cargo_args[@]}")
    python_path="$(command -v python3)"
    helper_path="$ROOT_DIR/scripts/actor_census_build_receipt.py"
    cargo_env=(
        env
        -u CARGO_BUILD_TARGET
        -u CARGO_ENCODED_RUSTFLAGS
        -u _
        "LLVM_SYS_181_PREFIX=$LLVM_PREFIX"
        CARGO_BUILD_JOBS=1
        CARGO_TERM_COLOR=never
        "CARGO_TARGET_DIR=$build_target_dir"
    )
    {
        printf 'LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 CARGO_TERM_COLOR=never CARGO_TARGET_DIR=%q ' \
            "$LLVM_PREFIX" "$build_target_dir"
        printf 'env -u CARGO_BUILD_TARGET -u CARGO_ENCODED_RUSTFLAGS -u _ %q ' "$cargo_path"
        printf '%q ' "${cargo_args[@]}"
        printf '\n'
    } > "$gate_dir/command.txt"
    printf '%s\0' "${cargo_argv[@]}" > "$gate_dir/argv.nul"

    record_build_helper() {
        local evidence_name="$1"
        shift
        local helper_status
        printf '%s\0' "$@" > "$gate_dir/$evidence_name.argv.nul"
        {
            printf '%q ' "$@"
            printf '> %q 2> %q\n' \
                "$gate_dir/$evidence_name.stdout.bin" "$gate_dir/$evidence_name.stderr.bin"
        } > "$gate_dir/$evidence_name.command.txt"
        if "$@" > "$gate_dir/$evidence_name.stdout.bin" \
            2> "$gate_dir/$evidence_name.stderr.bin"; then
            helper_status=0
        else
            helper_status=$?
        fi
        printf 'exit_code=%s\n' "$helper_status" > "$gate_dir/$evidence_name.exit.txt"
        return "$helper_status"
    }

    if ! record_build_helper environment-capture \
        "${cargo_env[@]}" "$python_path" "$helper_path" capture-environment \
        --output "$gate_dir/cargo-environment.json"; then
        validation_status=1
        printf 'FAIL could not capture the effective Cargo environment\n' \
            >> "$gate_dir/validation.txt"
    fi
    if [[ "$validation_status" -eq 0 ]] && ! record_build_helper source-before-capture \
        "${cargo_env[@]}" "$python_path" "$helper_path" capture-source-snapshot \
        --root "$ROOT_DIR" --output "$gate_dir/source.before.json"; then
        validation_status=1
        printf 'FAIL could not capture the full-route source snapshot before Cargo\n' \
            >> "$gate_dir/validation.txt"
    fi
    if [[ "$validation_status" -eq 0 ]] && ! record_build_helper cargo-identity-before-capture \
        "${cargo_env[@]}" "$python_path" "$helper_path" capture-cargo-identity \
        --root "$ROOT_DIR" --cargo "$cargo_path" --output "$gate_dir/cargo-identity.before.json"; then
        validation_status=1
        printf 'FAIL could not capture selected rustup Cargo identity before build\n' \
            >> "$gate_dir/validation.txt"
    fi
    if [[ "$validation_status" -eq 0 ]] && ! record_build_helper cargo-config-before-capture \
        "${cargo_env[@]}" "$python_path" "$helper_path" capture-cargo-config \
        --root "$ROOT_DIR" --output "$gate_dir/cargo-config.before.json"; then
        validation_status=1
        printf 'FAIL could not capture effective Cargo config files before build\n' \
            >> "$gate_dir/validation.txt"
    fi
    if [[ "$validation_status" -eq 0 ]]; then
        if "${cargo_env[@]}" "$cargo_path" "${cargo_args[@]}" \
            > "$gate_dir/stdout.bin" 2> "$gate_dir/stderr.bin"; then
            status=0
        else
            status=$?
        fi
    else
        status=125
        : > "$gate_dir/stdout.bin"
        : > "$gate_dir/stderr.bin"
    fi
    printf 'exit_code=%s\n' "$status" > "$gate_dir/exit.txt"
    if [[ "$status" -ne 125 ]]; then
        if ! record_build_helper source-after-capture \
            "${cargo_env[@]}" "$python_path" "$helper_path" capture-source-snapshot \
            --root "$ROOT_DIR" --output "$gate_dir/source.after.json"; then
            validation_status=1
            printf 'FAIL could not capture the full-route source snapshot after Cargo\n' \
                > "$gate_dir/source-after-capture.validation.txt"
        fi
        if ! record_build_helper cargo-identity-after-capture \
            "${cargo_env[@]}" "$python_path" "$helper_path" capture-cargo-identity \
            --root "$ROOT_DIR" --cargo "$cargo_path" --output "$gate_dir/cargo-identity.after.json"; then
            validation_status=1
            printf 'FAIL could not capture selected rustup Cargo identity after build\n' \
                > "$gate_dir/cargo-identity-after-capture.validation.txt"
        fi
        if ! record_build_helper cargo-config-after-capture \
            "${cargo_env[@]}" "$python_path" "$helper_path" capture-cargo-config \
            --root "$ROOT_DIR" --output "$gate_dir/cargo-config.after.json"; then
            validation_status=1
            printf 'FAIL could not capture effective Cargo config files after build\n' \
                > "$gate_dir/cargo-config-after-capture.validation.txt"
        fi
    fi
    {
        printf '===== stdout =====\n'
        cat "$gate_dir/stdout.bin"
        printf '\n===== stderr =====\n'
        cat "$gate_dir/stderr.bin"
    } > "$gate_dir/combined.log"
    if [[ "$status" -ne 0 ]]; then
        validation_status=1
        printf 'FAIL current-source mimi build exit=%s\n' "$status" >> "$gate_dir/validation.txt"
    fi
    if [[ "$status" -ne 125 && -f "$gate_dir/source.before.json" &&
        -f "$gate_dir/source.after.json" && -f "$gate_dir/cargo-environment.json" &&
        -f "$gate_dir/cargo-identity.before.json" && -f "$gate_dir/cargo-identity.after.json" &&
        -f "$gate_dir/cargo-config.before.json" && -f "$gate_dir/cargo-config.after.json" ]]; then
        receipt_command=(
            "$python_path" "$helper_path" create
            --artifact-root "$ARTIFACT_DIR"
            --gate-dir "$gate_dir"
            --target-dir "$build_target_dir"
            --host-target "$host_target"
            --root "$ROOT_DIR"
            --receipt "$gate_dir/build-receipt.json"
        )
        printf '%s\0' "${receipt_command[@]}" > "$gate_dir/receipt-create.argv.nul"
        {
            printf '%q ' "${receipt_command[@]}"
            printf '> %q 2> %q\n' "$gate_dir/receipt-create.stdout.bin" \
                "$gate_dir/receipt-create.stderr.bin"
        } > "$gate_dir/receipt-create.command.txt"
        if "${receipt_command[@]}" > "$gate_dir/receipt-create.stdout.bin" \
            2> "$gate_dir/receipt-create.stderr.bin"; then
            receipt_status=0
        else
            receipt_status=$?
            validation_status=1
        fi
        printf 'exit_code=%s\n' "$receipt_status" > "$gate_dir/receipt-create.exit.txt"
        if [[ "$receipt_status" -eq 0 && -f "$gate_dir/mimi.path" && -f "$gate_dir/mimi.sha256" ]]; then
            ISOLATED_MIMI_DIR="$build_target_dir"
            ISOLATED_MIMI_PATH="$(<"$gate_dir/mimi.path")"
            if [[ -x "$ISOLATED_MIMI_PATH" ]]; then
                printf 'PASS Cargo receipt binds raw invocation, environment, source snapshots, and Mimi artifact\n' \
                    >> "$gate_dir/validation.txt"
                MIMI_BUILD_PASSED=1
            else
                validation_status=1
                printf 'FAIL receipt Cargo-reported Mimi executable is not executable\n' \
                    >> "$gate_dir/validation.txt"
            fi
        else
            validation_status=1
            printf 'FAIL source-bound Mimi build receipt did not validate\n' \
                >> "$gate_dir/validation.txt"
        fi
    else
        validation_status=1
        printf 'FAIL build did not produce all raw inputs needed for a reproducible receipt\n' \
            >> "$gate_dir/validation.txt"
    fi
    cat "$gate_dir/stdout.bin"
    cat "$gate_dir/stderr.bin" >&2
    if [[ "$validation_status" -ne 0 ]]; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
        return 1
    fi
    return 0
}

record_python_step() {
    local label="$1" run_allowed="$2" skip_reason="$3"
    shift 3
    local step_dir="$ARTIFACT_DIR/commands/$label"
    local python_path status
    local -a invocation

    mkdir -- "$step_dir" || {
        printf 'FAIL could not create route-census evidence directory: %s\n' "$step_dir" >&2
        return 1
    }
    python_path="$(command -v python3)"
    invocation=(
        env
        -u RUSTFLAGS
        -u CARGO_ENCODED_RUSTFLAGS
        -u MIMI_ROUTE_CENSUS
        -u MIMI_ROUTE_CENSUS_MIMI
        -u MIMI_ROUTE_CENSUS_MIMI_SHA256
        -u MIMI_ROUTE_CENSUS_AUTHORITY_SOURCE
        "CARGO_TARGET_DIR=$EFFECTIVE_TARGET_DIR"
        "$python_path"
        "$@"
    )
    {
        printf 'working_directory=%q\n' "$ROOT_DIR"
        printf '%q ' "${invocation[@]}"
        printf '> %q 2> %q\n' "$step_dir/stdout.bin" "$step_dir/stderr.bin"
    } > "$step_dir/command.txt"
    printf '%s\0' "${invocation[@]}" > "$step_dir/argv.nul"
    {
        printf 'working_directory=%s\n' "$ROOT_DIR"
        printf 'CARGO_TARGET_DIR=%q\n' "$EFFECTIVE_TARGET_DIR"
        printf 'unset_environment=MIMI_ROUTE_CENSUS,MIMI_ROUTE_CENSUS_MIMI,MIMI_ROUTE_CENSUS_MIMI_SHA256,MIMI_ROUTE_CENSUS_AUTHORITY_SOURCE,RUSTFLAGS,CARGO_ENCODED_RUSTFLAGS\n'
        printf 'collector_injects_authority_environment=true\n'
    } > "$step_dir/environment.txt"

    if [[ "$run_allowed" != 1 ]]; then
        : > "$step_dir/stdout.bin"
        printf 'SKIPPED: %s\n' "$skip_reason" > "$step_dir/stderr.bin"
        printf 'SKIPPED: %s\n' "$skip_reason" > "$step_dir/combined.log"
        printf 'exit_code=not_run\n' > "$step_dir/exit.txt"
        printf 'SKIPPED: %s\n' "$skip_reason" > "$step_dir/validation.txt"
        return 1
    fi

    if "${invocation[@]}" > "$step_dir/stdout.bin" 2> "$step_dir/stderr.bin"; then
        status=0
    else
        status=$?
    fi
    printf 'exit_code=%s\n' "$status" > "$step_dir/exit.txt"
    {
        printf '===== stdout =====\n'
        cat "$step_dir/stdout.bin"
        printf '\n===== stderr =====\n'
        cat "$step_dir/stderr.bin"
    } > "$step_dir/combined.log"
    if [[ "$status" -eq 0 ]]; then
        printf 'PASS command exit=0\n' > "$step_dir/validation.txt"
    else
        printf 'FAIL command exit=%s\n' "$status" > "$step_dir/validation.txt"
    fi
    cat "$step_dir/stdout.bin"
    cat "$step_dir/stderr.bin" >&2
    [[ "$status" -eq 0 ]]
}

require_l3_memcheck_reports() {
    local gate_dir="$ARTIFACT_DIR/commands/actor-strict-l3"
    local source="$gate_dir/stderr.bin"
    local report_dir="$ARTIFACT_DIR/l3-memcheck"
    local spec label marker report normalized errors=0
    local -a report_specs=(
        'capture-default|Memcheck actor_nested_func_capture.mimi (default route) report:'
        'capture-explicit|Memcheck actor_nested_func_capture.mimi (explicit --mir) report:'
        'shadow-default|Memcheck actor_nested_func_shadow.mimi (default route) report:'
        'shadow-explicit|Memcheck actor_nested_func_shadow.mimi (explicit --mir) report:'
    )
    mkdir -- "$report_dir" || {
        printf 'FAIL could not create Memcheck report directory: %s\n' "$report_dir" >&2
        printf 'FAIL could not create Memcheck report directory: %s\n' "$report_dir" \
            > "$ARTIFACT_DIR/l3-memcheck.validation.txt" || true
        return 1
    }
    for spec in "${report_specs[@]}"; do
        label="${spec%%|*}"
        marker="${spec#*|}"
        report="$report_dir/$label.txt"
        if ! awk -v marker="$marker" '
            index($0, marker) { active = 1; found++; next }
            active && /^Memcheck actor_nested_func_/ { active = 0 }
            active && /^Actor L3 route-binding / { active = 0 }
            active && /^test result:/ { active = 0 }
            active { print }
            END { if (found != 1) exit 3 }
        ' "$source" > "$report"; then
            if ! printf 'FAIL missing or duplicate Memcheck report: %s\n' "$label" \
                > "$report_dir/$label.validation.txt"; then
                printf 'FAIL could not write validation record for Memcheck report: %s\n' "$label" >&2
            fi
            errors=$((errors + 1))
            continue
        fi
        normalized="$report_dir/$label.normalized.txt"
        awk '
            {
                sub(/^==[0-9]+== ?/, "")
                sub(/^[[:space:]]+/, "")
                sub(/[[:space:]]+$/, "")
                print
            }
        ' "$report" > "$normalized"
        if grep -Fqx 'Memcheck, a memory error detector' "$normalized" &&
            grep -Fqx 'HEAP SUMMARY:' "$normalized" &&
            grep -Fqx 'LEAK SUMMARY:' "$normalized" &&
            grep -Fqx 'definitely lost: 0 bytes in 0 blocks' "$normalized" &&
            grep -Fqx 'indirectly lost: 0 bytes in 0 blocks' "$normalized" &&
            grep -Fqx 'possibly lost: 0 bytes in 0 blocks' "$normalized" &&
            grep -Fqx 'still reachable: 192 bytes in 1 blocks' "$normalized" &&
            grep -Fqx 'suppressed: 0 bytes in 0 blocks' "$normalized" &&
            grep -Fqx 'in use at exit: 192 bytes in 1 blocks' "$normalized" &&
            grep -Fqx '192 bytes in 1 blocks are still reachable in loss record 1 of 1' "$normalized" &&
            grep -F 'BTreeMap<u64, alloc::sync::Arc<mimi_runtime::runtime::actor_domain::NativeDomainRoot>>' "$normalized" >/dev/null &&
            grep -F 'mimi_mir_actor_domain_open' "$normalized" >/dev/null &&
            grep -Fqx 'ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)' "$normalized"; then
            if ! printf 'PASS %s: zero loss/errors; one 192-byte reachable Actor domain registry allocation\n' \
                "$label" > "$report_dir/$label.validation.txt"; then
                printf 'FAIL could not write validation record for Memcheck report: %s\n' "$label" >&2
                errors=$((errors + 1))
            fi
        else
            if ! printf 'FAIL Memcheck report did not match the strict Actor resource-domain summary: %s\n' \
                "$label" > "$report_dir/$label.validation.txt"; then
                printf 'FAIL could not write validation record for Memcheck report: %s\n' "$label" >&2
            fi
            errors=$((errors + 1))
        fi
    done
    if [[ "$errors" -ne 0 ]]; then
        printf 'FAIL Memcheck report validation failed: errors=%s\n' "$errors" \
            > "$ARTIFACT_DIR/l3-memcheck.validation.txt" || true
        return 1
    fi
    printf 'PASS all four strict Actor Memcheck reports matched the expected resource-domain summary\n' \
        > "$ARTIFACT_DIR/l3-memcheck.validation.txt" || return 1
    return 0
}

validate_full_route_census() {
    local report="$ARTIFACT_DIR/full-route-census.strict.json"
    local initial_report="$ARTIFACT_DIR/full-route-census.initial.json"
    local validation="$ARTIFACT_DIR/census-status.txt"
    local binary_hash="$ARTIFACT_DIR/commands/current-source-mimi-build/mimi.sha256"
    local census_mimi="${ISOLATED_MIMI_PATH:-}"
    python3 - "$report" "$initial_report" "$validation" "$MIMI_BUILD_PASSED" \
        "$binary_hash" "$census_mimi" \
        "$ARTIFACT_DIR/commands/census-initial/exit.txt" \
        "$ARTIFACT_DIR/commands/census-authority-generator/exit.txt" \
        "$ARTIFACT_DIR/commands/census-strict/exit.txt" \
        "$ARTIFACT_DIR/route-census-authority-bundle" \
        "$ARTIFACT_DIR/commands/current-source-mimi-build/build-receipt.json" \
        "$ROOT_DIR" \
        "$ARTIFACT_DIR/source.inputs.before.nul" \
        "$ARTIFACT_DIR/source.before.sha256" \
        "$ARTIFACT_DIR/source.inputs.after.nul" \
        "$ARTIFACT_DIR/source.after.sha256" \
        "$ARTIFACT_DIR/source-scope-reconciliation.json" \
        "$CENSUS_EXPECTATIONS_PATH" "$CENSUS_EXPECTATIONS_SHA256" <<'PY'
import base64
import hashlib
import json
import re
import sys
from pathlib import Path

report_path = Path(sys.argv[1])
initial_report_path = Path(sys.argv[2])
status_path = Path(sys.argv[3])
binary_build_passed = sys.argv[4] == "1"
binary_hash_path = Path(sys.argv[5])
expected_binary_path = Path(sys.argv[6])
initial_exit_path = Path(sys.argv[7])
generator_exit_path = Path(sys.argv[8])
strict_exit_path = Path(sys.argv[9])
expected_bundle_path = Path(sys.argv[10])
build_receipt_path = Path(sys.argv[11])
repository_root = Path(sys.argv[12]).resolve()
outer_before_inventory = Path(sys.argv[13])
outer_before_hashes = Path(sys.argv[14])
outer_after_inventory = Path(sys.argv[15])
outer_after_hashes = Path(sys.argv[16])
reconciliation_path = Path(sys.argv[17])
expectations_path = str(Path(sys.argv[18]).resolve(strict=True))
expectations_sha256 = sys.argv[19]
sys.path.insert(0, str(repository_root / "scripts"))
import actor_census_build_receipt as build_receipt
import full_route_census as route_census
expected = {
    "tests/real_world/actor_nested_func_capture.mimi": (
        "canonical_actor_capture_original_uses_default_and_explicit_cli_without_fallback",
        b"15\n",
    ),
    "tests/real_world/actor_nested_func_shadow.mimi": (
        "canonical_actor_shadow_original_uses_default_and_explicit_cli_without_fallback",
        b"42\n101\n",
    ),
}
issues = []

def mapping(value):
    return value if isinstance(value, dict) else {}

def rows(value):
    return value if isinstance(value, list) else []

def load_json(path, label):
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        issues.append(f"{label} is unavailable or invalid: {error}")
        return {}
    if not isinstance(value, dict):
        issues.append(f"{label} is not a JSON object")
        return {}
    return value

def read_exit(path, label):
    try:
        value = path.read_text(encoding="utf-8").strip()
    except (OSError, UnicodeError) as error:
        issues.append(f"{label} exit record is unavailable: {error}")
        return None
    match = re.fullmatch(r"exit_code=(\d+)", value)
    if match is None:
        issues.append(f"{label} did not execute successfully: {value or '<empty>'}")
        return None
    return int(match.group(1))

report = load_json(report_path, "strict full census report")
initial_report = load_json(initial_report_path, "initial full census report")
receipt = load_json(build_receipt_path, "source-bound Mimi Cargo build receipt")

def validate_complete_report(value, label, *, strict):
    inventory_value = mapping(value.get("input_inventory"))
    manifest_rows_value = rows(inventory_value.get("inputs"))
    records_value = rows(value.get("records"))
    manifest_paths_value = [row.get("path") for row in manifest_rows_value if isinstance(row, dict)]
    record_paths_value = [row.get("path") for row in records_value if isinstance(row, dict)]
    manifest_value = mapping(value.get("manifest"))
    denominator_value = manifest_value.get("denominator")
    if value.get("schema_version") != "0.1.11-full-route-census-v7":
        issues.append(f"{label} has an unexpected census schema version")
    if value.get("collection_mode") != "full_execution":
        issues.append(f"{label} did not run full_execution mode")
    if not isinstance(denominator_value, int) or denominator_value != len(manifest_rows_value) or len(records_value) != denominator_value:
        issues.append(f"{label} records do not preserve the complete denominator")
    if inventory_value.get("count") != denominator_value:
        issues.append(f"{label} input inventory count differs from its denominator")
    if manifest_value.get("sha256") != inventory_value.get("manifest_sha256"):
        issues.append(f"{label} source manifest identity is inconsistent")
    if len(manifest_paths_value) != len(set(manifest_paths_value)) or len(record_paths_value) != len(set(record_paths_value)):
        issues.append(f"{label} has duplicate paths in the inventory or records")
    if set(record_paths_value) != set(manifest_paths_value):
        issues.append(f"{label} records do not exactly cover the inventory inputs")
    if value.get("source_snapshot_stable") is not True:
        issues.append(f"{label} source snapshot is not stable")
    snapshot_start = mapping(value.get("source_snapshot"))
    snapshot_end = mapping(value.get("source_snapshot_end"))
    if not snapshot_start.get("snapshot_id"):
        issues.append(f"{label} source identity is absent")
    if strict and snapshot_end.get("snapshot_id") != snapshot_start.get("snapshot_id"):
        issues.append(f"{label} source snapshot start/end identities differ")
    gate_value = mapping(value.get("gate"))
    if strict:
        if gate_value.get("requested") is not True or gate_value.get("mode") != "strict_check":
            issues.append("strict census report was not produced by --check")
        if gate_value.get("passed") is not True:
            issues.append("strict full census gate did not pass")
    elif gate_value.get("requested") is not False or gate_value.get("mode") != "evidence_collection_only":
        issues.append("initial census report was not an unchecked evidence collection")
    return inventory_value, manifest_rows_value, records_value, manifest_value, denominator_value, manifest_paths_value, record_paths_value

initial_inventory, initial_manifest_rows, initial_records, initial_manifest, initial_denominator, initial_manifest_paths, initial_record_paths = validate_complete_report(
    initial_report, "initial census", strict=False
)
inventory, manifest_rows, records, manifest, denominator, manifest_paths, record_paths = validate_complete_report(
    report, "strict census", strict=True
)
if initial_manifest.get("sha256") != manifest.get("sha256"):
    issues.append("initial and strict census source manifest hashes differ")
if initial_denominator != denominator or set(initial_manifest_paths) != set(manifest_paths):
    issues.append("initial and strict census denominator identities differ")
for label, value in (("initial", initial_report), ("strict", report)):
    if not route_census._provided_expectations_identity_matches(
        value,
        expected_path=expectations_path,
        expected_sha256=expectations_sha256,
    ):
        issues.append(f"{label} census did not use the runner's exact independent expectations manifest")

required_build_runner_paths = (
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "scripts/full_route_census.py",
    "scripts/actor_census_build_receipt.py",
    "scripts/verify-actor-two-fixture-current.sh",
    "scripts/generate_route_census_authority.py",
    "scripts/audit-mir-legacy-owners.sh",
    "tests/scripts/test_full_route_census.py",
    "tests/scripts/test_actor_census_build_receipt.py",
    "tests/scripts/test_actor_two_fixture_runner_contract.py",
    "src/main/canonical_dispatch.rs",
    "tests/real_world_cli.rs",
    "tests/real_world/actor_nested_func_capture.mimi",
    "tests/real_world/actor_nested_func_shadow.mimi",
)
outer_before = {}
outer_after = {}
try:
    outer_before = build_receipt.manifest_from_snapshot_files(
        outer_before_inventory, outer_before_hashes
    )["files"]
    outer_after = build_receipt.manifest_from_snapshot_files(
        outer_after_inventory, outer_after_hashes
    )["files"]
except (OSError, ValueError) as error:
    issues.append(f"runner curated source snapshots could not be recomputed: {error}")
build_source = mapping(receipt.get("build_source"))
build_source_manifest = mapping(build_source.get("manifest"))
build_source_files = mapping(build_source_manifest.get("files"))
reconciliation, reconciliation_issues = build_receipt.reconcile_source_manifests(
    outer_before,
    outer_after,
    build_source_files,
    required_build_runner_paths,
)
issues.extend(f"source scope reconciliation: {item}" for item in reconciliation_issues)
try:
    with reconciliation_path.open("x", encoding="utf-8") as stream:
        json.dump(reconciliation, stream, ensure_ascii=False, sort_keys=True, indent=2)
        stream.write("\n")
except OSError as error:
    issues.append(f"could not save source-scope reconciliation evidence: {error}")

report_snapshots = {
    "initial.source_snapshot": initial_report.get("source_snapshot"),
    "initial.source_snapshot_end": initial_report.get("source_snapshot_end"),
    "strict.source_snapshot": report.get("source_snapshot"),
    "strict.source_snapshot_end": report.get("source_snapshot_end"),
}
receipt_issues = build_receipt.validate_receipt(
    receipt,
    receipt_path=build_receipt_path,
    root=repository_root,
    mimi_path=expected_binary_path,
    report_snapshots=report_snapshots,
    outer_source_manifest=outer_before,
)
issues.extend(f"source-bound Mimi build receipt: {item}" for item in receipt_issues)
try:
    receipt_sha256 = hashlib.sha256(build_receipt_path.read_bytes()).hexdigest()
    receipt_resolved_path = str(build_receipt_path.resolve(strict=True))
except OSError:
    receipt_sha256 = None
    receipt_resolved_path = None
for label, value in (("initial", initial_report), ("strict", report)):
    provenance = mapping(value.get("build_provenance"))
    if provenance.get("status") != "verified" or provenance.get("current_source_attested") is not True:
        issues.append(f"{label} full census report does not carry verified source-bound build provenance")
    if provenance.get("receipt_path") != receipt_resolved_path or provenance.get("receipt_sha256") != receipt_sha256:
        issues.append(f"{label} full census report is not bound to the runner build receipt")
if mapping(mapping(report.get("gate")).get("checks")).get("source_bound_mimi_build_receipt") is not True:
    issues.append("strict full census gate did not enforce the source-bound Mimi build receipt")

manifest_by_path = {
    str(row.get("path")): row for row in manifest_rows if isinstance(row, dict)
}
record_by_path = {
    str(row.get("path")): row for row in records if isinstance(row, dict)
}
case_rows = rows(mapping(report.get("actor_fallback_cases")).get("cases"))
case_by_path = {
    str(row.get("path")): row for row in case_rows if isinstance(row, dict)
}
fallback_paths = set(rows(mapping(report.get("gate")).get("fallback_inputs")))
if not fallback_paths:
    fallback_paths = set(rows(report.get("fallback_inputs")))
gate = mapping(report.get("gate"))
if not binary_build_passed:
    issues.append("the current-source mimi binary build did not pass")
for exit_path, label in (
    (initial_exit_path, "initial census"),
    (generator_exit_path, "authority generator"),
    (strict_exit_path, "strict census"),
):
    if read_exit(exit_path, label) != 0:
        issues.append(f"{label} command did not exit successfully")
try:
    # sha256sum includes the absolute file path, which can contain non-ASCII
    # characters in local workspaces. Decode the path as UTF-8, then validate
    # the digest token itself as ASCII hex.
    built_binary_hash = binary_hash_path.read_text(encoding="utf-8").split()[0]
except (OSError, UnicodeError, IndexError):
    built_binary_hash = None
census_binary = mapping(mapping(report.get("toolchain")).get("mimi"))
initial_binary = mapping(mapping(initial_report.get("toolchain")).get("mimi"))
if not isinstance(built_binary_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", built_binary_hash):
    issues.append("the successfully built mimi binary SHA-256 is missing")
try:
    resolved_binary_path = str(expected_binary_path.resolve(strict=True))
except OSError:
    resolved_binary_path = None
if census_binary.get("path") != resolved_binary_path:
    issues.append("strict census used a different mimi executable path")
if census_binary.get("sha256") != built_binary_hash:
    issues.append("strict census mimi executable hash differs from the current-source Cargo build")
if initial_binary.get("path") != resolved_binary_path or initial_binary.get("sha256") != built_binary_hash:
    issues.append("initial census did not use the exact current-source Mimi path and hash")
try:
    current_binary_hash = hashlib.sha256(expected_binary_path.read_bytes()).hexdigest()
except OSError:
    current_binary_hash = None
if current_binary_hash != built_binary_hash:
    issues.append("current-source Mimi executable changed after its recorded build hash")

authority_identity = mapping(report.get("authority_mimi_identity"))
if authority_identity.get("path") != resolved_binary_path or authority_identity.get("sha256") != built_binary_hash:
    issues.append("strict authority identity differs from the current-source Mimi executable")
for observation_name in ("before_authority_tests", "after_collection"):
    observation = mapping(authority_identity.get(observation_name))
    if observation.get("resolved_path") != resolved_binary_path or observation.get("sha256") != built_binary_hash:
        issues.append(f"strict authority Mimi identity changed at {observation_name}")

bundle = mapping(report.get("generated_authority_bundle"))
bundle_execution = mapping(report.get("generated_authority_bundle_execution"))
expectations = mapping(report.get("expectations"))
try:
    resolved_bundle_path = str(expected_bundle_path.resolve(strict=True))
except OSError:
    resolved_bundle_path = None
if bundle.get("path") != resolved_bundle_path or bundle.get("status") != "validated":
    issues.append("strict census did not validate the newly generated authority bundle")
if bundle.get("generation_mode") != "strict_authority":
    issues.append("Actor gate rejects bootstrap_observation authority bundles")
if bundle.get("mimi_path") != resolved_binary_path or bundle.get("mimi_sha256") != built_binary_hash:
    issues.append("authority bundle is not bound to the current-source Mimi path and hash")
if bundle_execution.get("status") != "stable":
    issues.append("generated authority bundle was not stable throughout strict replay")
if bundle_execution.get("stable_after_authority_tests") is not True or bundle_execution.get("stable_after_collection") is not True:
    issues.append("authority bundle stability observations did not both pass")
if expectations.get("authority_bundle_stable") is not True:
    issues.append("strict expectations do not prove authority bundle stability")
if rows(expectations.get("invalid_authority_paths")):
    issues.append("strict census reports invalid source-bound authorities")
for path, (test_id, expected_stdout) in expected.items():
    source = manifest_by_path.get(path)
    record = record_by_path.get(path)
    case = case_by_path.get(path)
    source_hash = source.get("sha256") if isinstance(source, dict) else None
    if not isinstance(source, dict) or source.get("present") is not True or source.get("presence_kind") != "regular_file":
        issues.append(f"{path}: source is absent or not a regular inventory input")
    if not isinstance(source_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", source_hash):
        issues.append(f"{path}: source SHA-256 is missing")
    if not isinstance(record, dict):
        issues.append(f"{path}: full route record is missing")
    else:
        route = mapping(record.get("route"))
        per_function = mapping(record.get("per_function"))
        if record.get("sha256") != source_hash or record.get("source_hash_before_execution") != source_hash or record.get("source_hash_after_execution") != source_hash:
            issues.append(f"{path}: route record is not bound to the inventory source hash")
        if record.get("source_stable_for_input") is not True:
            issues.append(f"{path}: source changed during the route observation")
        if record.get("run_status") != "completed" or record.get("exit_code") != 0:
            issues.append(f"{path}: default route build did not complete successfully")
        if route.get("disposition") != "canonical" or route.get("classification_status") != "observed":
            issues.append(f"{path}: census did not observe a canonical route disposition")
        if record.get("candidate_classification") != "canonical":
            issues.append(f"{path}: census candidate classification is not canonical")
        if per_function.get("named_roster_complete") is not True or per_function.get("has_fallback") is not False:
            issues.append(f"{path}: per-function eligibility/emission roster is incomplete or uses fallback")
        if path in fallback_paths:
            issues.append(f"{path}: census lists the fixture in fallback_inputs")
    if not isinstance(case, dict):
        issues.append(f"{path}: actor fallback evidence row is missing")
        continue
    if case.get("in_denominator") is not True or case.get("present") is not True:
        issues.append(f"{path}: Actor row is not a present denominator input")
    if case.get("whole_program_disposition") != "canonical" or case.get("run_status") != "completed" or case.get("exit_code") != 0:
        issues.append(f"{path}: Actor row is not a successful canonical build")
    per_function = mapping(case.get("per_function"))
    if per_function.get("named_roster_complete") is not True or per_function.get("has_fallback") is not False:
        issues.append(f"{path}: Actor row lacks a complete no-fallback per-function roster")
    authority = mapping(case.get("native_test_authority"))
    assertion = mapping(authority.get("assertion"))
    execution = mapping(case.get("rust_test_execution"))
    if case.get("native_test_authority_valid") is not True or case.get("native_test_helper_valid") is not True:
        issues.append(f"{path}: source-bound Actor CLI authority/helper is invalid")
    if authority.get("test_id") != test_id or execution.get("test_id") != test_id:
        issues.append(f"{path}: exact Actor authority test ID does not match {test_id}")
    if assertion.get("path") != "tests/real_world_cli.rs" or not re.fullmatch(r"[0-9a-f]{64}", str(assertion.get("sha256", ""))):
        issues.append(f"{path}: authority assertion source identity is missing")
    if case.get("rust_test_execution_valid") is not True or execution.get("execution_valid") is not True:
        issues.append(f"{path}: exact authority Cargo test execution is not valid")
    if execution.get("fixture_sha256_expected") != source_hash or execution.get("fixture_sha256_before") != source_hash or execution.get("fixture_sha256_after") != source_hash:
        issues.append(f"{path}: authority execution did not retain matching fixture hashes")
    if execution.get("test_source_sha256_before") != assertion.get("sha256") or execution.get("test_source_sha256_after") != assertion.get("sha256"):
        issues.append(f"{path}: authority test source hash changed during its run")
    if case.get("native_stdout_matches_exactly") is not True or case.get("native_exit_code_matches") is not True or case.get("native_executable_contract_matches") is not True:
        issues.append(f"{path}: native executable output/exit does not match the fixture contract")
    native_run = mapping(case.get("native_executable_run"))
    native_stdout = mapping(native_run.get("stdout_raw"))
    encoded = native_stdout.get("base64")
    try:
        observed_stdout = base64.b64decode(encoded, validate=True) if isinstance(encoded, str) else None
    except (ValueError, TypeError):
        observed_stdout = None
    if observed_stdout != expected_stdout:
        issues.append(f"{path}: exact native stdout bytes do not match the fixture contract")

actor_flags = (
    "actor_fallback_evidence_complete",
    "actor_fallback_cases_are_canonical_without_fallback",
)
for flag in actor_flags:
    if gate.get(flag) is not True:
        issues.append(f"full census gate does not prove {flag}")
for field in (
    "actor_fallback_invalid_test_authority",
    "actor_fallback_native_output_mismatches",
    "actor_rust_test_execution_failures",
):
    if rows(gate.get(field)):
        issues.append(f"full census reports {field}: {gate[field]}")

route_counts = mapping(report.get("route_counts"))
class_counts = mapping(report.get("candidate_classification_counts"))
gate_passed = gate.get("passed")
census_gate_status = "passed" if gate_passed is True else "failed" if gate_passed is False else "<missing>"
failed_global_checks = sorted(
    name for name, passed in mapping(gate.get("checks")).items() if passed is not True
)
target_status = "CLOSED" if not issues else "NOT_CLOSED"
lines = [
    f"actor_fixture_rows={target_status}",
    f"census_global_gate_passed={gate_passed}",
    f"census_global_gate_mode={gate.get('mode', '<missing>')}",
    f"census_global_gate_status={census_gate_status}",
    f"denominator={denominator}",
    f"manifest_sha256={mapping(report.get('manifest')).get('sha256', '<missing>')}",
    f"source_snapshot_id={mapping(report.get('source_snapshot')).get('snapshot_id', '<missing>')}",
    f"source_snapshot_stable={report.get('source_snapshot_stable')}",
    f"authority_bundle_status={bundle_execution.get('status', '<missing>')}",
    f"authority_bundle_stable={expectations.get('authority_bundle_stable', '<missing>')}",
    f"mimi_binary_path={census_binary.get('path', '<missing>')}",
    f"mimi_binary_sha256={census_binary.get('sha256', '<missing>')}",
    f"route_counts={json.dumps(route_counts, sort_keys=True)}",
    f"candidate_classification_counts={json.dumps(class_counts, sort_keys=True)}",
    f"failure_class_counts={json.dumps(mapping(report.get('failure_class_counts')), sort_keys=True)}",
    f"global_failed_gate_checks={json.dumps(failed_global_checks, ensure_ascii=False)}",
    f"global_not_successful_inputs={json.dumps(rows(gate.get('not_successful_inputs')), ensure_ascii=False)}",
    f"global_noncanonical_route_inputs={json.dumps(rows(gate.get('noncanonical_route_inputs')), ensure_ascii=False)}",
    f"global_unauthorized_noncanonical_route_inputs={json.dumps(rows(gate.get('unauthorized_noncanonical_route_inputs')), ensure_ascii=False)}",
    f"global_fallback_inputs={json.dumps(rows(gate.get('fallback_inputs')), ensure_ascii=False)}",
    f"global_unknown_or_unmapped_candidates={json.dumps(rows(gate.get('unknown_or_unmapped_candidates')), ensure_ascii=False)}",
    f"global_per_function_incomplete_inputs={json.dumps(rows(gate.get('per_function_incomplete_inputs')), ensure_ascii=False)}",
    f"global_unclassified_failure_inputs={json.dumps(rows(gate.get('unclassified_failure_inputs')), ensure_ascii=False)}",
    f"global_unexpected_failure_inputs={json.dumps(rows(gate.get('unexpected_failure_inputs')), ensure_ascii=False)}",
    f"global_unexpected_or_mismatched_inputs={json.dumps(rows(gate.get('unexpected_or_mismatched_inputs')), ensure_ascii=False)}",
    f"global_missing_tracked_inputs={json.dumps(rows(gate.get('missing_tracked_inputs')), ensure_ascii=False)}",
    f"global_unfollowed_mimi_symlinks={json.dumps(rows(gate.get('unfollowed_mimi_symlinks')), ensure_ascii=False)}",
    "actor_rows=" + json.dumps([
        {
            "path": path,
            "source_sha256": manifest_by_path.get(path, {}).get("sha256"),
            "authority_test_id": test_id,
            "authority_source_path": mapping(mapping(case_by_path.get(path, {}).get("native_test_authority")).get("assertion")).get("path"),
            "authority_source_sha256": mapping(mapping(case_by_path.get(path, {}).get("native_test_authority")).get("assertion")).get("sha256"),
            "authority_source_line": mapping(mapping(case_by_path.get(path, {}).get("native_test_authority")).get("assertion")).get("line"),
            "authority_helper_sha256": case_by_path.get(path, {}).get("native_test_helper_sha256"),
            "route_disposition": mapping(record_by_path.get(path, {}).get("route")).get("disposition"),
            "route_reason": mapping(record_by_path.get(path, {}).get("route")).get("reason"),
            "candidate_classification": record_by_path.get(path, {}).get("candidate_classification"),
            "named_roster_complete": mapping(record_by_path.get(path, {}).get("per_function")).get("named_roster_complete"),
            "has_fallback": mapping(record_by_path.get(path, {}).get("per_function")).get("has_fallback"),
            "native_exit_code": mapping(case_by_path.get(path, {}).get("native_executable_run")).get("exit_code"),
            "native_stdout_sha256": mapping(mapping(case_by_path.get(path, {}).get("native_executable_run")).get("stdout_raw")).get("sha256"),
            "native_stdout_expected": expected_stdout.decode("ascii"),
            "per_function_eligibility": mapping(record_by_path.get(path, {}).get("per_function")),
        }
        for path, (test_id, expected_stdout) in expected.items()
    ], ensure_ascii=False, sort_keys=True),
    "issues=" + json.dumps(issues, ensure_ascii=False),
]
status_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
print("\n".join(lines))
raise SystemExit(0 if target_status == "CLOSED" else 1)
PY
}

# L1: exact checker ownership/effects, Actor environment receipt, and the two
# historical VM/native equivalence fixtures. Each command selects one test.
record_gate checker-owner-effects-l1 \
    core::resolved::tests::nested_actor_function_callsite_uses_exact_checker_owner_and_effects 1 \
    test --lib core::resolved::tests::nested_actor_function_callsite_uses_exact_checker_owner_and_effects \
    -- --exact --nocapture --test-threads=1 || true
record_gate actor-environment-receipt-l1 \
    core::mir::actor_tests::canonical_actor_nested_helpers_use_actor_environment_receipts 1 \
    test --lib core::mir::actor_tests::canonical_actor_nested_helpers_use_actor_environment_receipts \
    -- --exact --nocapture --test-threads=1 || true
record_gate fixture-owner-effects-l1 \
    core::mir::route::tests::actor_nested_helper_fixtures_have_direct_typed_targets_and_exact_effect_summaries 1 \
    test --lib core::mir::route::tests::actor_nested_helper_fixtures_have_direct_typed_targets_and_exact_effect_summaries \
    -- --exact --nocapture --test-threads=1 || true
record_gate capture-dual-backend-l1 tests::dual_backend::dual_actor_nested_func_captures_local 1 \
    test --lib tests::dual_backend::dual_actor_nested_func_captures_local \
    -- --exact --test-threads=1 || true
record_gate shadow-dual-backend-l1 tests::dual_backend::dual_actor_nested_func_shadows_global 1 \
    test --lib tests::dual_backend::dual_actor_nested_func_shadows_global \
    -- --exact --test-threads=1 || true

# L2: the exact two-source graph must agree across default/direct route
# selection; an unsupported Actor nested-helper shape must fail closed before
# reaching the legacy Actor emitter.
record_gate original-fixture-route-identity-l2 \
    canonical_dispatch::tests::canonical_actor_default_and_explicit_share_original_fixture_graphs 1 \
    test --bin mimi canonical_dispatch::tests::canonical_actor_default_and_explicit_share_original_fixture_graphs \
    -- --exact --nocapture --test-threads=1 || true
record_gate unsupported-shape-fail-closed-l2 \
    canonical_dispatch::tests::unsupported_nested_actor_candidate_rejects_before_legacy_route 1 \
    test --bin mimi canonical_dispatch::tests::unsupported_nested_actor_candidate_rejects_before_legacy_route \
    -- --exact --nocapture --test-threads=1 || true

# Each original fixture's exact CLI test checks default and explicit --mir
# run/build/verify, native output, disassembly opcodes, route profile/digest,
# receipt identity, and absence of legacy disposition.
record_gate capture-cli-default-explicit-l2 \
    canonical_actor_capture_original_uses_default_and_explicit_cli_without_fallback 1 \
    test --test real_world_cli canonical_actor_capture_original_uses_default_and_explicit_cli_without_fallback \
    -- --exact --nocapture --test-threads=1 || true
record_gate shadow-cli-default-explicit-l2 \
    canonical_actor_shadow_original_uses_default_and_explicit_cli_without_fallback 1 \
    test --test real_world_cli canonical_actor_shadow_original_uses_default_and_explicit_cli_without_fallback \
    -- --exact --nocapture --test-threads=1 || true

# The integrated no-Z3 verifier case checks both original fixtures under
# default and explicit --mir, then exercises unsupported proof/FFI owners and
# verifies owner-scoped NITS plus artifact-free --stats rows. Keep it in this
# replay so route success cannot stand in for correct no-Z3 result ownership.
record_gate no-z3-owner-attribution-l2 \
    canonical_actor_cli_no_z3_keeps_infrastructure_and_owner_nits_results 1 \
    test --test real_world_cli canonical_actor_cli_no_z3_keeps_infrastructure_and_owner_nits_results \
    -- --exact --nocapture --test-threads=1 || true

# The ignored strict integration test rebuilds both fixtures under default and
# explicit --mir routes and runs all four native binaries under Memcheck.
record_gate actor-strict-l3 \
    canonical_actor_original_default_and_explicit_native_bins_pass_strict_memcheck_l3 1 \
    test --test real_world_cli canonical_actor_original_default_and_explicit_native_bins_pass_strict_memcheck_l3 \
    -- --exact --ignored --nocapture --test-threads=1 || true
if ! require_l3_memcheck_reports; then
    GATE_FAILURES=$((GATE_FAILURES + 1))
fi

# Build exactly one Mimi executable for both full-census passes. A failed or
# missing isolated build is recorded as a skipped census chain; never fall
# back to a possibly stale target/debug/mimi.
record_build current-source-mimi-build build --bin mimi --locked || true
census_scratch_root="$EFFECTIVE_TARGET_DIR/route-census-scratch"
census_mimi=""
census_mimi_sha256=""
census_mimi_ready=0
census_mimi_build_hash=""
if [[ "$MIMI_BUILD_PASSED" -eq 1 && -n "$ISOLATED_MIMI_PATH" && -x "$ISOLATED_MIMI_PATH" ]]; then
    if census_mimi="$(realpath -- "$ISOLATED_MIMI_PATH")"; then
        census_mimi_build_hash="$(awk 'NR == 1 { print $1 }' \
            "$ARTIFACT_DIR/commands/current-source-mimi-build/mimi.sha256")"
        census_mimi_sha256="$(sha256sum -- "$census_mimi" | awk 'NR == 1 { print $1 }')"
        if [[ -n "$census_mimi" && "$census_mimi" == /* && -x "$census_mimi" &&
            "$census_mimi_sha256" =~ ^[0-9a-f]{64}$ &&
            "$census_mimi_sha256" == "$census_mimi_build_hash" ]]; then
            ISOLATED_MIMI_PATH="$census_mimi"
            census_mimi_ready=1
        fi
    fi
fi
{
    printf 'isolated_build_passed=%s\n' "$MIMI_BUILD_PASSED"
    printf 'mimi_path=%s\n' "${census_mimi:-<missing>}"
    printf 'build_sha256=%s\n' "${census_mimi_build_hash:-<missing>}"
    printf 'current_sha256=%s\n' "${census_mimi_sha256:-<missing>}"
    if [[ "$census_mimi_ready" -eq 1 ]]; then
        printf 'PASS isolated Mimi path and current SHA-256 are fixed\n'
    else
        printf 'FAIL no verified isolated Mimi executable; census stages must not run\n'
    fi
} > "$ARTIFACT_DIR/census-mimi-identity.txt"
if [[ "$census_mimi_ready" -ne 1 ]]; then
    GATE_FAILURES=$((GATE_FAILURES + 1))
fi

initial_census_args=(
    scripts/full_route_census.py
    --root "$ROOT_DIR"
    --mimi "$census_mimi"
    --build-receipt "$ARTIFACT_DIR/commands/current-source-mimi-build/build-receipt.json"
    --expectations "$CENSUS_EXPECTATIONS_PATH"
    --output "$ARTIFACT_DIR/full-route-census.initial.json"
    --artifacts "$ARTIFACT_DIR/full-route-census.initial.artifacts"
    --scratch-root "$census_scratch_root"
    --jobs 8
)
generator_args=(
    scripts/generate_route_census_authority.py
    --root "$ROOT_DIR"
    --report "$ARTIFACT_DIR/full-route-census.initial.json"
    --output-dir "$ARTIFACT_DIR/route-census-authority-bundle"
)
strict_census_args=(
    scripts/full_route_census.py
    --root "$ROOT_DIR"
    --mimi "$census_mimi"
    --build-receipt "$ARTIFACT_DIR/commands/current-source-mimi-build/build-receipt.json"
    --expectations "$CENSUS_EXPECTATIONS_PATH"
    --authority-bundle "$ARTIFACT_DIR/route-census-authority-bundle"
    --output "$ARTIFACT_DIR/full-route-census.strict.json"
    --artifacts "$ARTIFACT_DIR/full-route-census.strict.artifacts"
    --scratch-root "$census_scratch_root"
    --jobs 8
    --check
)

initial_census_ok=0
if record_python_step census-initial "$census_mimi_ready" \
    "current-source isolated Mimi build/path/hash was not verified" "${initial_census_args[@]}"; then
    initial_census_ok=1
else
    GATE_FAILURES=$((GATE_FAILURES + 1))
fi
expectations_after_initial="$(sha256sum -- "$CENSUS_EXPECTATIONS_PATH" | awk 'NR == 1 { print $1 }')"
if [[ "$expectations_after_initial" != "$CENSUS_EXPECTATIONS_SHA256" ]]; then
    printf 'FAIL independent expectations manifest changed during initial census\n' \
        > "$ARTIFACT_DIR/expectations-manifest.validation.txt"
    initial_census_ok=0
    GATE_FAILURES=$((GATE_FAILURES + 1))
fi

generator_ok=0
if record_python_step census-authority-generator "$initial_census_ok" \
    "initial full census did not complete successfully" "${generator_args[@]}"; then
    generator_ok=1
elif [[ "$initial_census_ok" -eq 1 ]]; then
    GATE_FAILURES=$((GATE_FAILURES + 1))
fi

strict_allowed=0
if [[ "$generator_ok" -eq 1 && "$census_mimi_ready" -eq 1 ]]; then
    census_mimi_pre_strict_sha256="$(sha256sum -- "$census_mimi" | awk 'NR == 1 { print $1 }')"
    if [[ "$census_mimi_pre_strict_sha256" == "$census_mimi_sha256" ]]; then
        strict_allowed=1
    else
        printf 'FAIL Mimi executable changed between initial census and strict replay\n' \
            > "$ARTIFACT_DIR/census-mimi-stability.txt"
        GATE_FAILURES=$((GATE_FAILURES + 1))
    fi
fi
if record_python_step census-strict "$strict_allowed" \
    "authority generation failed or the fixed Mimi binary hash changed" "${strict_census_args[@]}"; then
    census_mimi_post_strict_sha256="$(sha256sum -- "$census_mimi" | awk 'NR == 1 { print $1 }')"
    if [[ "$census_mimi_post_strict_sha256" == "$census_mimi_sha256" ]]; then
        printf 'PASS same Mimi binary SHA-256 before and after strict replay: %s\n' \
            "$census_mimi_sha256" > "$ARTIFACT_DIR/census-mimi-stability.txt"
    else
        printf 'FAIL Mimi executable changed during strict replay: expected=%s observed=%s\n' \
            "$census_mimi_sha256" "$census_mimi_post_strict_sha256" \
            > "$ARTIFACT_DIR/census-mimi-stability.txt"
        GATE_FAILURES=$((GATE_FAILURES + 1))
    fi
else
    if [[ "$strict_allowed" -eq 1 ]]; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
    fi
fi
expectations_after_strict="$(sha256sum -- "$CENSUS_EXPECTATIONS_PATH" | awk 'NR == 1 { print $1 }')"
if [[ "$expectations_after_strict" != "$CENSUS_EXPECTATIONS_SHA256" ]]; then
    printf 'FAIL independent expectations manifest changed during strict census\n' \
        > "$ARTIFACT_DIR/expectations-manifest.validation.txt"
    GATE_FAILURES=$((GATE_FAILURES + 1))
fi

source_snapshot "$ARTIFACT_DIR/source.inputs.after.nul" "$ARTIFACT_DIR/source.after.sha256" || {
    SNAPSHOT_FAILURE=1
    printf 'FAIL could not capture after source snapshot\n' > "$ARTIFACT_DIR/source-snapshot.validation.txt"
}
if [[ "$SNAPSHOT_FAILURE" -eq 0 ]]; then
    sha256sum "$ARTIFACT_DIR/source.inputs.after.nul" > "$ARTIFACT_DIR/source.after.inventory.sha256"
    sha256sum "$ARTIFACT_DIR/source.after.sha256" > "$ARTIFACT_DIR/source.after.manifest.sha256"
    if cmp -s "$ARTIFACT_DIR/source.inputs.before.nul" "$ARTIFACT_DIR/source.inputs.after.nul" &&
        cmp -s "$ARTIFACT_DIR/source.before.sha256" "$ARTIFACT_DIR/source.after.sha256" &&
        (cd "$ARTIFACT_DIR" && sha256sum --check --status \
            source.before.inventory.sha256 source.before.manifest.sha256 \
            source.after.inventory.sha256 source.after.manifest.sha256); then
        source_file_count=0
        while IFS= read -r -d '' _source_file; do source_file_count=$((source_file_count + 1)); done \
            < "$ARTIFACT_DIR/source.inputs.before.nul"
        printf 'PASS %s source paths and SHA-256 manifests match before/after replay\n' \
            "$source_file_count" > "$ARTIFACT_DIR/source-snapshot.validation.txt"
    else
        SNAPSHOT_FAILURE=1
        printf 'FAIL source path inventory or hashes changed during replay\n' \
            > "$ARTIFACT_DIR/source-snapshot.validation.txt"
    fi
fi

if validate_full_route_census; then
    if grep -qx 'actor_fixture_rows=CLOSED' "$ARTIFACT_DIR/census-status.txt"; then
        ACTOR_CENSUS_CLOSED=1
    else
        GATE_FAILURES=$((GATE_FAILURES + 1))
    fi
else
    GATE_FAILURES=$((GATE_FAILURES + 1))
    ACTOR_CENSUS_CLOSED=0
fi

if [[ "$GATE_FAILURES" -eq 0 && "$SNAPSHOT_FAILURE" -eq 0 && "$ACTOR_CENSUS_CLOSED" -eq 1 ]]; then
    printf 'PASS narrow Actor capture/shadow current-source L1/L2/L3 evidence; actor fixture rows closed. The strict full-census gate passed; all route rows remain recorded in full-route-census.strict.json and census-status.txt.\n' \
        > "$ARTIFACT_DIR/result.txt"
    RUN_STATUS=0
else
    printf 'FAIL narrow Actor capture/shadow current-source evidence: gate_failures=%s source_snapshot_failure=%s actor_census_closed=%s; full evidence retained\n' \
        "$GATE_FAILURES" "$SNAPSHOT_FAILURE" "$ACTOR_CENSUS_CLOSED" \
        > "$ARTIFACT_DIR/result.txt"
    RUN_STATUS=1
fi

printf 'artifact_dir=%s\nrun_status=%s\nactor_census_closed=%s\n' \
    "$ARTIFACT_DIR" "$RUN_STATUS" "$ACTOR_CENSUS_CLOSED"
exit "$RUN_STATUS"
