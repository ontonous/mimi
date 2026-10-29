#!/usr/bin/env bash
# Narrow current-source gate for the two original Actor nested-helper fixtures.
# The full route census is retained as-is; only its two Actor rows determine
# this script's fixture-specific closure status. Its global gate and all other
# candidate classifications/failures are written to census-status.txt.
# This does not run or alter the broad FFI test gate; the full census retains
# all candidate outcomes and reports its global status without filtering rows.
set -uo pipefail
set -o noclobber

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_RELATIVE="scripts/verify-actor-two-fixture-current.sh"
LLVM_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
export LLVM_SYS_181_PREFIX="$LLVM_PREFIX"
ARTIFACT_REQUESTED="${MIMI_ACTOR_TWO_FIXTURE_ARTIFACT_DIR:-}"
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

for required_tool in cargo rustc python3 sha256sum sort find xargs cmp awk grep realpath date git uname env; do
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
    {
        printf 'started_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf 'repository=%s\n' "$ROOT_DIR"
        printf 'git_head=%s\n' "$(git rev-parse HEAD 2>/dev/null || printf '<unavailable>')"
        printf 'git_branch=%s\n' "$(git symbolic-ref --quiet --short HEAD 2>/dev/null || printf '<detached-or-unavailable>')"
        printf 'host=%s\n' "$(uname -a)"
        printf 'native_host_target=%s\n' "$NATIVE_HOST_TARGET"
        printf 'LLVM_SYS_181_PREFIX=%q\n' "$LLVM_PREFIX"
        if [[ -e "$LLVM_PREFIX" ]]; then
            printf 'LLVM_SYS_181_PREFIX_realpath=%s\n' "$(realpath -- "$LLVM_PREFIX")"
        else
            printf 'LLVM_SYS_181_PREFIX_realpath=<missing>\n'
        fi
        printf 'effective.CARGO_BUILD_JOBS=1\n'
        printf 'effective.CARGO_TERM_COLOR=never\n'
        printf 'effective.CARGO_TARGET_DIR=%q\n' "$EFFECTIVE_TARGET_DIR"
        for name in CARGO_HOME CARGO_TARGET_DIR CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_TARGET \
            CARGO_BUILD_JOBS CARGO_INCREMENTAL CARGO_NET_OFFLINE CARGO_TERM_COLOR \
            CARGO_PROFILE_DEV_OPT_LEVEL CARGO_PROFILE_TEST_OPT_LEVEL \
            CARGO_PROFILE_DEV_DEBUG CARGO_PROFILE_TEST_DEBUG \
            RUSTFLAGS RUSTDOCFLAGS RUSTUP_TOOLCHAIN RUSTC RUSTC_WRAPPER \
            RUSTC_WORKSPACE_WRAPPER RUSTDOC CC CFLAGS CXX CXXFLAGS AR LD \
            LIBCLANG_PATH LLVM_CONFIG_PATH PKG_CONFIG_PATH PKG_CONFIG_LIBDIR \
            LD_LIBRARY_PATH MIMI_ROUTE_CENSUS MIMI_REACHABLE_DISPATCH PATH HOME; do
            if [[ ${!name+x} ]]; then
                printf 'env.%s=%q\n' "$name" "${!name}"
            else
                printf 'env.%s=<unset>\n' "$name"
            fi
        done
        cargo --version
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

source_input_inventory() {
    local root source_path required_path optional_path
    local -a required_sources=(
        Cargo.toml
        Cargo.lock
        build.rs
        "$SCRIPT_RELATIVE"
        scripts/full_route_census.py
        AGENTS.md
        devdocs/v0.41/actor-closure-0.1.11.md
        'devdocs/V0.2/09-0.1.11交接门禁.md'
        src/core/mir/actor_tests.rs
        src/core/mir/route.rs
        src/core/resolved/tests.rs
        src/main/canonical_dispatch.rs
        src/tests/dual_backend.rs
        tests/real_world_cli.rs
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
    cargo_path="$(command -v cargo)"
    {
        printf 'LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 CARGO_TERM_COLOR=never CARGO_TARGET_DIR=%q %q ' \
            "$LLVM_PREFIX" "$EFFECTIVE_TARGET_DIR" "$cargo_path"
        printf '%q ' "${cargo_args[@]}"
        printf '\n'
    } > "$gate_dir/command.txt"
    printf '%s\0' "$cargo_path" "${cargo_args[@]}" > "$gate_dir/argv.nul"
    {
        printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_PREFIX"
        printf 'CARGO_BUILD_JOBS=1\nCARGO_TERM_COLOR=never\nCARGO_TARGET_DIR=%q\n' "$EFFECTIVE_TARGET_DIR"
        printf 'working_directory=%s\n' "$ROOT_DIR"
        printf 'expected_test_id=%s\nexpected_passed=%s\n' "$expected_test" "$expected_passed"
    } > "$gate_dir/environment.txt"
    if env LLVM_SYS_181_PREFIX="$LLVM_PREFIX" CARGO_BUILD_JOBS=1 CARGO_TERM_COLOR=never \
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
    local cargo_path status validation_status=0
    local build_target_dir host_target
    local -a cargo_args=("$@")
    mkdir -- "$gate_dir" || {
        printf 'FAIL could not create command evidence directory: %s\n' "$gate_dir" >&2
        GATE_FAILURES=$((GATE_FAILURES + 1))
        return 1
    }
    cargo_path="$(command -v cargo)"
    build_target_dir="$EFFECTIVE_TARGET_DIR/actor-two-fixture-current-cargo-${BASHPID:-$$}"
    if [[ "${cargo_args[0]:-}" != "build" ]]; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL current-source binary recorder requires a cargo build command\n' \
            > "$gate_dir/validation.txt"
        return 1
    fi
    host_target="$NATIVE_HOST_TARGET"
    if ! validate_cargo_target_configuration "$host_target" "$gate_dir/cargo-target-configuration.txt"; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL Cargo target configuration changed or no longer selects the native host\n' \
            > "$gate_dir/validation.txt"
        return 1
    fi
    if [[ -e "$build_target_dir" || -L "$build_target_dir" ]] ||
        ! mkdir -- "$build_target_dir"; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
        printf 'FAIL could not reserve a fresh isolated Cargo target directory: %s\n' \
            "$build_target_dir" > "$gate_dir/validation.txt"
        return 1
    fi
    cargo_args+=(--target "$host_target" --message-format=json-render-diagnostics)
    {
        printf 'LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 CARGO_TERM_COLOR=never CARGO_TARGET_DIR=%q ' \
            "$LLVM_PREFIX" "$build_target_dir"
        printf 'env -u CARGO_BUILD_TARGET -u CARGO_ENCODED_RUSTFLAGS %q ' "$cargo_path"
        printf '%q ' "${cargo_args[@]}"
        printf '\n'
    } > "$gate_dir/command.txt"
    printf '%s\0' "$cargo_path" "${cargo_args[@]}" > "$gate_dir/argv.nul"
    {
        printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_PREFIX"
        printf 'CARGO_BUILD_JOBS=1\nCARGO_TERM_COLOR=never\nCARGO_TARGET_DIR=%q\n' "$build_target_dir"
        printf 'forced_host_target=%s\n' "$host_target"
        printf 'CARGO_BUILD_TARGET=unset\nCARGO_ENCODED_RUSTFLAGS=unset\n'
        printf 'working_directory=%s\n' "$ROOT_DIR"
    } > "$gate_dir/environment.txt"
    if env -u CARGO_BUILD_TARGET -u CARGO_ENCODED_RUSTFLAGS \
        LLVM_SYS_181_PREFIX="$LLVM_PREFIX" CARGO_BUILD_JOBS=1 CARGO_TERM_COLOR=never \
        CARGO_TARGET_DIR="$build_target_dir" \
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
    if [[ "$status" -ne 0 ]]; then
        validation_status=1
        printf 'FAIL current-source mimi build exit=%s\n' "$status" > "$gate_dir/validation.txt"
    else
        # This per-run CARGO_TARGET_DIR avoids Cargo artifact races with the shared target tree.
        if ! python3 - "$gate_dir/stdout.bin" "$build_target_dir" "$host_target" "$gate_dir/mimi.path" <<'PY'
import json
import os
import sys
from pathlib import Path

output = Path(sys.argv[1])
target_dir = Path(sys.argv[2])
expected_host = sys.argv[3]
path_file = Path(sys.argv[4])
target_root = target_dir.resolve(strict=True)
executables = []
for line in output.read_text(encoding="utf-8").splitlines():
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        continue
    if not isinstance(event, dict):
        continue
    target = event.get("target")
    executable = event.get("executable")
    if (
        event.get("reason") == "compiler-artifact"
        and isinstance(target, dict)
        and target.get("name") == "mimi"
        and isinstance(target.get("kind"), list)
        and "bin" in target.get("kind", [])
        and isinstance(executable, str)
    ):
        path = Path(executable).resolve(strict=True)
        try:
            relative_path = path.relative_to(target_root)
        except ValueError:
            raise SystemExit("Cargo executable path escaped the isolated target directory")
        if not relative_path.parts or relative_path.parts[0] != expected_host:
            raise SystemExit("Cargo executable path is not under the forced native-host target directory")
        if not path.is_file() or not os.access(path, os.X_OK):
            raise SystemExit("Cargo-reported Mimi executable is not executable")
        executables.append(path)
if len(executables) != 1:
    raise SystemExit(f"expected exactly one Cargo-reported Mimi executable, observed {len(executables)}")
path_file.write_text(f"{executables[0]}\n", encoding="utf-8")
PY
        then
            validation_status=1
            printf 'FAIL Cargo did not identify one executable inside the isolated target directory\n' \
                > "$gate_dir/validation.txt"
        else
            ISOLATED_MIMI_DIR="$build_target_dir"
            ISOLATED_MIMI_PATH="$(<"$gate_dir/mimi.path")"
            if [[ ! -x "$ISOLATED_MIMI_PATH" ]] ||
                ! sha256sum -- "$ISOLATED_MIMI_PATH" | awk 'NR == 1 { print $1 }' > "$gate_dir/mimi.sha256"; then
                validation_status=1
                printf 'FAIL Cargo-reported Mimi executable disappeared or could not be hashed\n' \
                    > "$gate_dir/validation.txt"
            else
                printf 'PASS Cargo identified an executable inside the isolated host-target directory\n' \
                    > "$gate_dir/validation.txt"
                MIMI_BUILD_PASSED=1
            fi
        fi
    fi
    cat "$gate_dir/stdout.bin"
    cat "$gate_dir/stderr.bin" >&2
    if [[ "$validation_status" -ne 0 ]]; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
        return 1
    fi
    return 0
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
    local report="$ARTIFACT_DIR/full-route-census.json"
    local validation="$ARTIFACT_DIR/census-status.txt"
    local binary_hash="$ARTIFACT_DIR/commands/current-source-mimi-build/mimi.sha256"
    local census_mimi="$ISOLATED_MIMI_PATH"
    if [[ -z "$census_mimi" ]]; then
        census_mimi="$EFFECTIVE_TARGET_DIR/debug/mimi"
    fi
    python3 - "$report" "$validation" "$MIMI_BUILD_PASSED" "$binary_hash" "$census_mimi" <<'PY'
import base64
import json
import re
import sys
from pathlib import Path

report_path = Path(sys.argv[1])
status_path = Path(sys.argv[2])
binary_build_passed = sys.argv[3] == "1"
binary_hash_path = Path(sys.argv[4])
expected_binary_path = Path(sys.argv[5])
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
try:
    report = json.loads(report_path.read_text(encoding="utf-8"))
except (OSError, UnicodeError, json.JSONDecodeError) as error:
    status_path.write_text(
        f"actor_fixture_rows=NOT_CLOSED\nfull_census_report=unavailable\nerror={error}\n",
        encoding="utf-8",
    )
    raise SystemExit(1)

def mapping(value):
    return value if isinstance(value, dict) else {}

def rows(value):
    return value if isinstance(value, list) else []

inventory = mapping(report.get("input_inventory"))
manifest_rows = rows(inventory.get("inputs"))
records = rows(report.get("records"))
manifest_paths = [row.get("path") for row in manifest_rows if isinstance(row, dict)]
record_paths = [row.get("path") for row in records if isinstance(row, dict)]
denominator = report.get("manifest", {}).get("denominator") if isinstance(report.get("manifest"), dict) else None
if report.get("schema_version") != "0.1.11-full-route-census-v6":
    issues.append("unexpected census schema version")
if report.get("collection_mode") != "full_execution":
    issues.append("census did not run full_execution mode")
if not isinstance(denominator, int) or denominator != len(manifest_rows) or len(records) != denominator:
    issues.append("full census records do not preserve the complete denominator")
if len(manifest_paths) != len(set(manifest_paths)) or len(record_paths) != len(set(record_paths)):
    issues.append("duplicate paths in census inventory or records")
if set(record_paths) != set(manifest_paths):
    issues.append("census records do not exactly cover the inventory inputs")
if report.get("source_snapshot_stable") is not True:
    issues.append("full census source snapshot is not stable")
if not isinstance(report.get("source_snapshot"), dict) or not report["source_snapshot"].get("snapshot_id"):
    issues.append("full census source identity is absent")

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
try:
    # sha256sum includes the absolute file path, which can contain non-ASCII
    # characters in local workspaces. Decode the path as UTF-8, then validate
    # the digest token itself as ASCII hex.
    built_binary_hash = binary_hash_path.read_text(encoding="utf-8").split()[0]
except (OSError, UnicodeError, IndexError):
    built_binary_hash = None
census_binary = mapping(mapping(report.get("toolchain")).get("mimi"))
if not isinstance(built_binary_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", built_binary_hash):
    issues.append("the successfully built mimi binary SHA-256 is missing")
try:
    resolved_binary_path = str(expected_binary_path.resolve(strict=True))
except OSError:
    resolved_binary_path = None
if census_binary.get("path") != resolved_binary_path:
    issues.append("full census used a different mimi executable path")
if census_binary.get("sha256") != built_binary_hash:
    issues.append("full census mimi executable hash differs from the current-source Cargo build")
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

# Build the exact binary consumed by the full current-source census. The
# census runs without --check so it records the complete denominator and all
# current route outcomes; global failures remain visible in the report.
record_build current-source-mimi-build build --bin mimi --locked || true
census_status=0
census_mimi="$ISOLATED_MIMI_PATH"
if [[ -z "$census_mimi" ]]; then
    census_mimi="$EFFECTIVE_TARGET_DIR/debug/mimi"
fi
census_scratch_root="$EFFECTIVE_TARGET_DIR/route-census-scratch"
{
    printf 'env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS CARGO_TARGET_DIR=%q python3 scripts/full_route_census.py --root %q --mimi %q --output %q --artifacts %q --scratch-root %q --jobs 8\n' \
        "$EFFECTIVE_TARGET_DIR" "$ROOT_DIR" "$census_mimi" "$ARTIFACT_DIR/full-route-census.json" \
        "$ARTIFACT_DIR/full-route-census.artifacts" "$census_scratch_root"
} > "$ARTIFACT_DIR/full-route-census.command.txt"
printf '%s\0' env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS \
    "CARGO_TARGET_DIR=$EFFECTIVE_TARGET_DIR" \
    python3 scripts/full_route_census.py \
    --root "$ROOT_DIR" \
    --mimi "$census_mimi" \
    --output "$ARTIFACT_DIR/full-route-census.json" \
    --artifacts "$ARTIFACT_DIR/full-route-census.artifacts" \
    --scratch-root "$census_scratch_root" \
    --jobs 8 > "$ARTIFACT_DIR/full-route-census.argv.nul"
env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS \
"CARGO_TARGET_DIR=$EFFECTIVE_TARGET_DIR" \
python3 scripts/full_route_census.py \
    --root "$ROOT_DIR" \
    --mimi "$census_mimi" \
    --output "$ARTIFACT_DIR/full-route-census.json" \
    --artifacts "$ARTIFACT_DIR/full-route-census.artifacts" \
    --scratch-root "$census_scratch_root" \
    --jobs 8 > "$ARTIFACT_DIR/full-route-census.stdout.bin" \
    2> "$ARTIFACT_DIR/full-route-census.stderr.bin" || census_status=$?
printf 'exit_code=%s\n' "$census_status" > "$ARTIFACT_DIR/full-route-census.exit.txt"
if [[ "$census_status" -ne 0 ]]; then
    GATE_FAILURES=$((GATE_FAILURES + 1))
    printf 'FAIL full route census command exit=%s; see retained report/artifacts if produced\n' \
        "$census_status" > "$ARTIFACT_DIR/census-status.txt"
else
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

if [[ "$GATE_FAILURES" -eq 0 && "$SNAPSHOT_FAILURE" -eq 0 && "$ACTOR_CENSUS_CLOSED" -eq 1 ]]; then
    printf 'PASS narrow Actor capture/shadow current-source L1/L2/L3 evidence; actor fixture rows closed. The complete census global gate and every non-Actor route result remain recorded separately in census-status.txt and full-route-census.json.\n' \
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
