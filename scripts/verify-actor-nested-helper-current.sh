#!/usr/bin/env bash
set -uo pipefail
set -o noclobber

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_RELATIVE="scripts/verify-actor-nested-helper-current.sh"
LLVM_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
ARTIFACT_REQUESTED="${MIMI_ACTOR_CURRENT_ARTIFACT_DIR:-}"
RUN_STATUS=1
GATE_FAILURES=0
SNAPSHOT_FAILURE=0
ARTIFACT_DIR=""

fail_early() {
    printf '%s\n' "$*" >&2
    exit 2
}

for required_tool in cargo sha256sum sort find xargs cmp awk grep tail realpath date mv rm git uname; do
    command -v "$required_tool" >/dev/null 2>&1 || fail_early "Actor current-source replay requires $required_tool"
done

if [[ -z "$ARTIFACT_REQUESTED" ]]; then
    ARTIFACT_REQUESTED="evidence/0.1.11-dev/gates/actor-current-$(date -u +%Y%m%dT%H%M%SZ)-replay"
fi
case "$ARTIFACT_REQUESTED" in
    /*) artifact_candidate="$ARTIFACT_REQUESTED" ;;
    *) artifact_candidate="$ROOT_DIR/$ARTIFACT_REQUESTED" ;;
esac
if [[ -e "$artifact_candidate" || -L "$artifact_candidate" ]]; then
    fail_early "refusing to overwrite existing Actor replay artifact path: $artifact_candidate"
fi
ARTIFACT_DIR="$(realpath -m -- "$artifact_candidate")"
case "$ARTIFACT_DIR/" in
    "$ROOT_DIR/evidence/0.1.11-dev/gates/"*) ;;
    "$ROOT_DIR/"*)
        fail_early "in-repository Actor replay artifacts must be under evidence/0.1.11-dev/gates: $ARTIFACT_DIR" ;;
esac
mkdir -p -- "$(dirname "$ARTIFACT_DIR")" || fail_early "cannot create artifact parent directory"
mkdir -- "$ARTIFACT_DIR" || fail_early "cannot create new artifact directory: $ARTIFACT_DIR"
ARTIFACT_DIR="$(realpath -- "$ARTIFACT_DIR")"

mkdir -- "$ARTIFACT_DIR/commands" "$ARTIFACT_DIR/l3-memcheck" || exit 2
# Capture Git identity and status from this repository even when invoked elsewhere.
cd "$ROOT_DIR" || exit 2

write_failure_state() {
    local reason="$1"
    printf 'FAIL %s; partial evidence retained\n' "$reason" >| "$ARTIFACT_DIR/result.txt"
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
        printf 'FAIL artifact files.sha256 generation or verification failed; partial evidence retained\n' \
            >| "$ARTIFACT_DIR/result.txt"
        (
            cd "$ARTIFACT_DIR" || exit 1
            find . -type f ! -name files.sha256 ! -name files.sha256.sha256 \
                ! -name files.sha256.validation.txt -print0 \
                | sort -z | xargs -0 -r sha256sum >| files.sha256
            sha256sum --check files.sha256
            sha256sum files.sha256 >| files.sha256.sha256
            sha256sum --check files.sha256.sha256
        ) >| "$ARTIFACT_DIR/files.sha256.validation.txt" 2>&1 || true
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

record_tool_version() {
    local name="$1"
    shift
    local tool_path
    tool_path="$(command -v "$name" || true)"
    if [[ -z "$tool_path" ]]; then
        printf '%s_path=<unavailable>\n' "$name"
        return 1
    fi
    printf '%s_path=%s\n' "$name" "$tool_path"
    "$tool_path" "$@"
}

{
    printf 'started_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'repository=%s\n' "$ROOT_DIR"
    printf 'git_head=%s\n' "$(git rev-parse HEAD 2>/dev/null || printf '<unavailable>')"
    printf 'git_branch=%s\n' "$(git symbolic-ref --quiet --short HEAD 2>/dev/null || printf '<detached-or-unavailable>')"
    if git diff --quiet --ignore-submodules --; then
        printf 'tracked_worktree=clean\n'
    else
        printf 'tracked_worktree=dirty\n'
    fi
    printf 'host=%s\n' "$(uname -a)"
    printf 'LLVM_SYS_181_PREFIX=%q\n' "$LLVM_PREFIX"
    if [[ -e "$LLVM_PREFIX" ]]; then
        printf 'LLVM_SYS_181_PREFIX_realpath=%s\n' "$(realpath -- "$LLVM_PREFIX")"
    else
        printf 'LLVM_SYS_181_PREFIX_realpath=<missing>\n'
    fi
    for name in CARGO_HOME CARGO_TARGET_DIR CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_TARGET \
        CARGO_BUILD_JOBS CARGO_INCREMENTAL CARGO_NET_OFFLINE CARGO_TERM_COLOR \
        CARGO_PROFILE_DEV_OPT_LEVEL CARGO_PROFILE_TEST_OPT_LEVEL \
        CARGO_PROFILE_DEV_DEBUG CARGO_PROFILE_TEST_DEBUG \
        RUSTFLAGS RUSTDOCFLAGS RUSTUP_TOOLCHAIN RUSTC RUSTC_WRAPPER \
        RUSTC_WORKSPACE_WRAPPER RUSTDOC \
        CC CFLAGS CXX CXXFLAGS AR LD LIBCLANG_PATH LLVM_CONFIG_PATH \
        PKG_CONFIG_PATH PKG_CONFIG_LIBDIR LD_LIBRARY_PATH DYLD_LIBRARY_PATH \
        MIMI_ASAN MIMI_OPT MIMI_VALUE_GLUE MIMI_LOOP_UNROLL_CAP \
        MIMI_REACHABLE_DISPATCH MIMI_EAGER_SPAWN MIMI_VERBOSE \
        MIMI_ROUTE_CENSUS MIMI_RESOLVED_MODULE_BODIES PATH HOME; do
        if [[ ${!name+x} ]]; then
            printf 'env.%s=%q\n' "$name" "${!name}"
        else
            printf 'env.%s=<unset>\n' "$name"
        fi
    done
    printf 'effective.CARGO_BUILD_JOBS=1\n'
    printf 'effective.CARGO_TERM_COLOR=never\n'
    record_tool_version cargo --version || printf 'cargo_version_status=unavailable\n'
    record_tool_version rustc --version --verbose || printf 'rustc_version_status=unavailable\n'
    record_tool_version cc --version || printf 'cc_version_status=unavailable\n'
    record_tool_version clang --version || printf 'clang_version_status=unavailable\n'
    record_tool_version ld --version || printf 'ld_version_status=unavailable\n'
    record_tool_version ld.lld --version || printf 'ld_lld_version_status=unavailable\n'
    record_tool_version valgrind --version || printf 'valgrind_version_status=unavailable\n'
    llvm_config="$LLVM_PREFIX/bin/llvm-config"
    if [[ -x "$llvm_config" ]]; then
        printf 'llvm_config_path=%s\n' "$(realpath -- "$llvm_config")"
        sha256sum -- "$llvm_config"
        "$llvm_config" --version
    else
        printf 'llvm_config_path=<unavailable>\n'
    fi
    cargo_home="${CARGO_HOME:-${HOME:-}/.cargo}"
    for config in "$cargo_home/config" "$cargo_home/config.toml"; do
        if [[ -f "$config" && ! -L "$config" ]]; then
            sha256sum -- "$config"
        elif [[ -L "$config" ]]; then
            printf 'cargo_config_symlink=%s\n' "$config"
            resolved_config="$(realpath -- "$config" 2>/dev/null || true)"
            if [[ -n "$resolved_config" && -f "$resolved_config" ]]; then
                printf 'cargo_config_symlink_target=%s\n' "$resolved_config"
                sha256sum -- "$resolved_config"
            else
                printf 'cargo_config_symlink_target=<unavailable>\n'
            fi
        fi
    done
} > "$ARTIFACT_DIR/replay-environment.txt" 2>&1
if [[ $? -ne 0 ]]; then
    write_failure_state "could not capture replay environment"
    exit 1
fi

source_input_inventory() {
    local root source_path required_path optional_path
    local -a required_sources=(
        Cargo.toml
        Cargo.lock
        build.rs
        "$SCRIPT_RELATIVE"
        src/core/mir/actor_tests.rs
        src/core/mir/route.rs
        src/core/resolved/tests.rs
        src/main/canonical_dispatch.rs
        src/tests/dual_backend.rs
        src/codegen/mir/actor_status.rs
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
            printf 'source input disappeared, changed type, or could not be hashed: %s\n' \
                "$source_path" >&2
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

source_snapshot "$ARTIFACT_DIR/source.inputs.before.nul" \
    "$ARTIFACT_DIR/source.before.sha256"
if [[ $? -ne 0 ]]; then
    write_failure_state "could not capture before source snapshot"
    exit 1
fi
cat > "$ARTIFACT_DIR/source-snapshot-scope.txt" <<'EOF'
The NUL-delimited inventories are C-locale sorted and must remain byte-identical
while each SHA-256 manifest is captured. Manifests cover every regular file
under src/, tests/, and std/, plus .cargo/ when present, Cargo.toml, Cargo.lock,
build.rs, the replay runner, and root rust-toolchain files when present.
Required Actor test sources and both original fixture paths are checked
explicitly. Symlinks inside source/config trees are rejected so their targets
cannot be omitted from the snapshot. Build outputs and evidence artifacts are
outside the source-input inventory.
EOF
sha256sum "$ARTIFACT_DIR/source.inputs.before.nul" \
    > "$ARTIFACT_DIR/source.before.inventory.sha256"
sha256sum "$ARTIFACT_DIR/source.before.sha256" > "$ARTIFACT_DIR/source.before.manifest.sha256"

record_gate() {
    local label="$1"
    local expected_passed="$2"
    local expected_ignored="$3"
    shift 3
    local gate_dir="$ARTIFACT_DIR/commands/$label"
    local cargo_path result_line summary_count expected_pattern status validation_status=0
    local -a cargo_args=("$@")

    if ! mkdir -- "$gate_dir"; then
        printf 'FAIL could not create fresh command evidence directory: %s\n' "$gate_dir" >&2
        GATE_FAILURES=$((GATE_FAILURES + 1))
        return 1
    fi
    cargo_path="$(command -v cargo)"
    {
        printf 'LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 CARGO_TERM_COLOR=never %q ' \
            "$LLVM_PREFIX" "$cargo_path"
        printf '%q ' "${cargo_args[@]}"
        printf '\n'
    } > "$gate_dir/command.txt"
    printf '%s\0' "$cargo_path" "${cargo_args[@]}" > "$gate_dir/argv.nul"
    {
        printf 'LLVM_SYS_181_PREFIX=%s\n' "$LLVM_PREFIX"
        printf 'CARGO_BUILD_JOBS=1\n'
        printf 'CARGO_TERM_COLOR=never\n'
        printf 'working_directory=%s\n' "$ROOT_DIR"
    } > "$gate_dir/environment.txt"
    printf 'expected_passed=%s\nexpected_ignored=%s\n' \
        "$expected_passed" "$expected_ignored" > "$gate_dir/expected-selection.txt"

    if env LLVM_SYS_181_PREFIX="$LLVM_PREFIX" CARGO_BUILD_JOBS=1 CARGO_TERM_COLOR=never \
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

    summary_count="$(awk '/^test result:/ { count++ } END { print count + 0 }' \
        "$gate_dir/combined.log")"
    result_line="$(awk '/^test result:/ { result = $0 } END { print result }' \
        "$gate_dir/combined.log")"
    expected_pattern="^test result: ok[.] ${expected_passed} passed; 0 failed; ${expected_ignored} ignored; 0 measured; [0-9]+ filtered out; finished in [0-9]+([.][0-9]+)?s$"
    if [[ "$status" -ne 0 ]]; then
        validation_status=1
        printf 'FAIL cargo exit=%s\n' "$status" > "$gate_dir/validation.txt"
    elif [[ "$summary_count" -ne 1 ]]; then
        validation_status=1
        printf 'FAIL expected exactly one test result summary\nobserved_count=%s\nobserved_last=%s\n' \
            "$summary_count" "$result_line" > "$gate_dir/validation.txt"
    elif [[ ! "$result_line" =~ $expected_pattern ]]; then
        validation_status=1
        printf 'FAIL test result summary was not the exact all-passing selection\nobserved=%s\n' "$result_line" \
            > "$gate_dir/validation.txt"
    else
        printf 'PASS unique full-pass summary: %s\n' "$result_line" > "$gate_dir/validation.txt"
    fi

    cat "$gate_dir/stdout.bin"
    cat "$gate_dir/stderr.bin" >&2
    if [[ "$validation_status" -ne 0 ]]; then
        GATE_FAILURES=$((GATE_FAILURES + 1))
        return 1
    fi
    return 0
}

run_gate() {
    if ! record_gate "$@"; then
        return 1
    fi
    return 0
}

require_passed_tests() {
    local label="$1" test_name log_path gate_dir
    shift
    gate_dir="$ARTIFACT_DIR/commands/$label"
    log_path="$ARTIFACT_DIR/commands/$label/combined.log"
    for test_name in "$@"; do
        if [[ ! -f "$log_path" ]] || ! awk -v expected_test="$test_name" '
            function finish_test() {
                if (active && !passed) failed = 1
                active = 0
            }
            /^test / {
                finish_test()
                prefix = "test " expected_test " ..."
                if (index($0, prefix) == 1) {
                    found++
                    active = 1
                    if (substr($0, length(prefix) + 1) == " ok") passed = 1
                }
                next
            }
            /^test result:/ { finish_test(); next }
            active && $0 == "ok" { passed = 1 }
            END {
                finish_test()
                exit !(found == 1 && !failed)
            }
        ' "$log_path"; then
            if [[ -d "$gate_dir" ]]; then
                printf 'FAIL required test must be selected exactly once and report ok: %s\n' \
                    "$test_name" >> "$gate_dir/validation.txt"
            fi
            GATE_FAILURES=$((GATE_FAILURES + 1))
        fi
    done
}

run_gate canonical-actor-l1 40 0 test --lib canonical_actor_ -- --nocapture --test-threads=1 || true
require_passed_tests canonical-actor-l1 \
    core::mir::actor_tests::canonical_actor_reference_preserves_original_capture_stdout \
    core::mir::actor_tests::canonical_actor_reference_preserves_original_shadow_and_global_stdout \
    core::mir::actor_tests::canonical_actor_nested_helpers_use_actor_environment_receipts
run_gate actor-helper-l1 5 0 test --lib actor_nested_helper_ -- --nocapture --test-threads=1 || true
require_passed_tests actor-helper-l1 \
    core::mir::route::tests::actor_nested_helper_does_not_receipt_an_unrelated_extern_call \
    core::mir::route::tests::actor_nested_helper_does_not_receipt_unknown_top_level_helper \
    core::mir::route::tests::actor_nested_helper_fixtures_have_direct_typed_targets_and_exact_effect_summaries \
    core::mir::route::tests::pure_actor_nested_helper_callsite_uses_exact_function_summary \
    core::mir::route::tests::unknown_call_inside_actor_nested_helper_is_checked_under_helper_owner
run_gate actor-method-effects-l1 1 0 test --lib \
    core::mir::route::tests::actor_method_callsite_uses_exact_finite_effect_summary \
    -- --exact --nocapture --test-threads=1 || true
require_passed_tests actor-method-effects-l1 \
    core::mir::route::tests::actor_method_callsite_uses_exact_finite_effect_summary
run_gate dual-nested-l1 2 0 test --lib dual_actor_nested_func_ -- --nocapture --test-threads=1 || true
require_passed_tests dual-nested-l1 \
    tests::dual_backend::dual_actor_nested_func_captures_local \
    tests::dual_backend::dual_actor_nested_func_shadows_global
run_gate checker-owner-l1 1 0 test --lib \
    core::resolved::tests::nested_actor_function_callsite_uses_exact_checker_owner_and_effects \
    -- --exact --nocapture --test-threads=1 || true
require_passed_tests checker-owner-l1 \
    core::resolved::tests::nested_actor_function_callsite_uses_exact_checker_owner_and_effects
run_gate graph-route-l2 1 0 test --bin mimi \
    canonical_dispatch::tests::canonical_actor_default_and_explicit_share_original_fixture_graphs \
    -- --exact --nocapture --test-threads=1 || true
require_passed_tests graph-route-l2 \
    canonical_dispatch::tests::canonical_actor_default_and_explicit_share_original_fixture_graphs
run_gate unsupported-route-l2 1 0 test --bin mimi \
    canonical_dispatch::tests::unsupported_nested_actor_candidate_rejects_before_legacy_route \
    -- --exact --nocapture --test-threads=1 || true
require_passed_tests unsupported-route-l2 \
    canonical_dispatch::tests::unsupported_nested_actor_candidate_rejects_before_legacy_route
run_gate native-jit-l2 3 0 test --lib canonical_actor_native_jit_ -- --nocapture --test-threads=1 || true
require_passed_tests native-jit-l2 \
    codegen::mir::actor_status::tests::canonical_actor_native_jit_power_wrap_and_traps_join_before_reporting \
    codegen::mir::actor_status::tests::canonical_actor_native_jit_preserves_fixtures_state_and_domain_join \
    codegen::mir::actor_status::tests::canonical_actor_native_jit_tagged_traps_propagate_801_802_and_join
run_gate actor-cli-l2 6 1 test --test real_world_cli canonical_actor_ -- --nocapture --test-threads=1 || true

require_passed_tests actor-cli-l2 \
    canonical_actor_capture_original_uses_default_and_explicit_cli_without_fallback \
    canonical_actor_shadow_original_uses_default_and_explicit_cli_without_fallback
cat > "$ARTIFACT_DIR/fixture-contracts.txt" <<'EOF'
tests/real_world/actor_nested_func_capture.mimi stdout: 15\n
tests/real_world/actor_nested_func_shadow.mimi stdout: 42\n101\n
The existing CLI regression tests assert these byte-exact stdout values for
default and explicit MIR routes. The ignored L3 regression rebuilds and checks
the same values for all four native binaries and their Memcheck runs.
EOF

run_gate strict-l3-ignored 1 0 test --test real_world_cli \
    canonical_actor_original_default_and_explicit_native_bins_pass_strict_memcheck_l3 \
    -- --exact --ignored --nocapture --test-threads=1 || true
require_passed_tests strict-l3-ignored \
    canonical_actor_original_default_and_explicit_native_bins_pass_strict_memcheck_l3

validate_actor_l3_route_bindings() {
    local log_path="$1"
    awk '
        function field(token, name, prefix) {
            prefix = name "="
            if (index(token, prefix) != 1) return ""
            return substr(token, length(prefix) + 1)
        }
        function fail(message) {
            print "FAIL " message
            failed = 1
        }
        BEGIN {
            expected["actor_nested_func_capture.mimi" SUBSEP "default"] = 1
            expected["actor_nested_func_capture.mimi" SUBSEP "explicit-mir"] = 1
            expected["actor_nested_func_shadow.mimi" SUBSEP "default"] = 1
            expected["actor_nested_func_shadow.mimi" SUBSEP "explicit-mir"] = 1
        }
        /^Actor L3 route-binding / {
            line_count++
            if (NF != 8) {
                fail("route-binding line must contain exactly five structured fields: " $0)
                next
            }
            fixture = field($4, "fixture")
            mode = field($5, "mode")
            profile = field($6, "checker_profile")
            digest = field($7, "mir_digest")
            identity = field($8, "receipt_identity_digest")
            key = fixture SUBSEP mode
            if (!(key in expected)) {
                fail("unexpected fixture/mode pair: " fixture " / " mode)
                next
            }
            if (++seen[key] != 1) fail("duplicate fixture/mode pair: " fixture " / " mode)
            if (profile != "build-canonical-v1") {
                fail("unexpected build profile for " fixture " / " mode ": " profile)
            }
            if (length(digest) != 64 || digest ~ /[^0-9a-f]/) {
                fail("MIR digest is not canonical lowercase 64-hex for " fixture " / " mode)
            }
            if (length(identity) != 64 || identity ~ /[^0-9a-f]/) {
                fail("receipt identity is not canonical lowercase 64-hex for " fixture " / " mode)
            }
            profiles[key] = profile
            digests[key] = digest
            identities[key] = identity
        }
        END {
            if (line_count != 4) fail("expected exactly four route-binding lines; observed " (line_count + 0))
            for (key in expected) {
                if (seen[key] != 1) fail("missing expected fixture/mode pair: " key)
            }
            fixtures["actor_nested_func_capture.mimi"] = 1
            fixtures["actor_nested_func_shadow.mimi"] = 1
            for (fixture in fixtures) {
                default_key = fixture SUBSEP "default"
                explicit_key = fixture SUBSEP "explicit-mir"
                if (seen[default_key] == 1 && seen[explicit_key] == 1 &&
                    (profiles[default_key] != profiles[explicit_key] ||
                     digests[default_key] != digests[explicit_key] ||
                     identities[default_key] != identities[explicit_key])) {
                    fail("default and explicit build tuples differ for " fixture)
                }
            }
            if (!failed) print "PASS exactly four expected Actor L3 fixture/mode pairs; each uses build-canonical-v1 and each fixture has matching default/explicit MIR digest and receipt identity"
            exit failed
        }
    ' "$log_path"
}

route_binding_log="$ARTIFACT_DIR/commands/strict-l3-ignored/combined.log"
route_binding_validation="$ARTIFACT_DIR/commands/strict-l3-ignored/route-binding.validation.txt"
if [[ -f "$route_binding_log" ]] &&
    route_binding_result="$(validate_actor_l3_route_bindings "$route_binding_log" 2>&1)"; then
    printf '%s\n' "$route_binding_result" > "$route_binding_validation"
else
    printf 'FAIL strict Actor L3 route-binding output did not match the exact four-build receipt contract\n%s\n' \
        "${route_binding_result:-strict L3 combined log is unavailable}" > "$route_binding_validation"
    GATE_FAILURES=$((GATE_FAILURES + 1))
fi

extract_memcheck_report() {
    local label="$1" marker="$2" source="$3" target="$4"
    awk -v marker="$marker" '
        index($0, marker) { active = 1; found++; next }
        active && /^Memcheck actor_nested_func_/ { active = 0 }
        active && /^test result:/ { active = 0 }
        active { print }
        END { if (found != 1) exit 3 }
    ' "$source" > "$target"
}

validate_memcheck_report() {
    local report_file="$1"
    awk '
        function normalized(line) {
            sub(/^==[0-9]+== ?/, "", line)
            sub(/^[[:space:]]+/, "", line)
            sub(/[[:space:]]+$/, "", line)
            return line
        }
        {
            line = normalized($0)
            if (line == "Memcheck, a memory error detector") banner_count++
            if (line == "HEAP SUMMARY:") heap_header_count++
            if (line == "LEAK SUMMARY:") {
                leak_header_count++
                in_leak_summary = 1
                next
            }
            if (line ~ /^ERROR SUMMARY:/) {
                error_summary_count++
                if (line == "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)") {
                    error_summary_exact++
                }
                in_leak_summary = 0
                next
            }
            if (in_leak_summary && line ~ /bytes in .* blocks$/) leak_row_count++
            if (in_leak_summary && line ~ /^definitely lost:/) {
                category_count["definitely lost"]++
                category_value["definitely lost"] = line
            }
            if (in_leak_summary && line ~ /^indirectly lost:/) {
                category_count["indirectly lost"]++
                category_value["indirectly lost"] = line
            }
            if (in_leak_summary && line ~ /^possibly lost:/) {
                category_count["possibly lost"]++
                category_value["possibly lost"] = line
            }
            if (in_leak_summary && line ~ /^still reachable:/) {
                category_count["still reachable"]++
                category_value["still reachable"] = line
            }
            if (in_leak_summary && line ~ /^suppressed:/) {
                category_count["suppressed"]++
                category_value["suppressed"] = line
            }
            if (line ~ /^in use at exit:/) {
                in_use_count++
                in_use_value = line
            }
            if (line ~ / in loss record /) {
                loss_record_count++
                loss_record_value = line
            }
            if (line ~ /BTreeMap<u64/ && line ~ /NativeDomainRoot/) actor_registry_stack_count++
            if (line ~ /mimi_mir_actor_domain_open/) actor_open_stack_count++
        }
        END {
            if (banner_count != 1) print "expected one Memcheck banner; observed " banner_count
            if (heap_header_count != 1) print "expected one HEAP SUMMARY; observed " heap_header_count
            if (leak_header_count != 1) print "expected one LEAK SUMMARY; observed " leak_header_count
            if (leak_row_count != 5) print "expected exactly five leak-category rows; observed " leak_row_count

            expected["definitely lost"] = "definitely lost: 0 bytes in 0 blocks"
            expected["indirectly lost"] = "indirectly lost: 0 bytes in 0 blocks"
            expected["possibly lost"] = "possibly lost: 0 bytes in 0 blocks"
            expected["still reachable"] = "still reachable: 192 bytes in 1 blocks"
            expected["suppressed"] = "suppressed: 0 bytes in 0 blocks"
            for (category in expected) {
                if (category_count[category] != 1) {
                    print "expected one " category " row; observed " category_count[category]
                } else if (category_value[category] != expected[category]) {
                    print "unexpected " category " row: " category_value[category]
                }
            }
            if (in_use_count != 1 || in_use_value != "in use at exit: 192 bytes in 1 blocks") {
                print "expected one exact 192-byte heap-at-exit summary; observed " in_use_count ": " in_use_value
            }
            if (loss_record_count != 1 || loss_record_value != "192 bytes in 1 blocks are still reachable in loss record 1 of 1") {
                print "expected one exact 192-byte reachable loss record; observed " loss_record_count ": " loss_record_value
            }
            if (actor_registry_stack_count != 1) {
                print "expected the reachable allocation stack to identify one BTreeMap<u64, NativeDomainRoot>; observed " actor_registry_stack_count
            }
            if (actor_open_stack_count != 1) {
                print "expected the reachable allocation stack to identify mimi_mir_actor_domain_open exactly once; observed " actor_open_stack_count
            }
            if (error_summary_count != 1 || error_summary_exact != 1) {
                print "expected one exact zero-error/zero-suppression ERROR SUMMARY; observed " error_summary_count
            }
            if (banner_count != 1 || heap_header_count != 1 || leak_header_count != 1 ||
                leak_row_count != 5 || in_use_count != 1 || loss_record_count != 1 ||
                actor_registry_stack_count != 1 || actor_open_stack_count != 1 ||
                error_summary_count != 1 || error_summary_exact != 1) failed = 1
            for (category in expected) {
                if (category_count[category] != 1 || category_value[category] != expected[category]) failed = 1
            }
            exit failed
        }
    ' "$report_file"
}

l3_stderr="$ARTIFACT_DIR/commands/strict-l3-ignored/stderr.bin"
memcheck_status=0
for report_spec in \
    'capture-default|Memcheck actor_nested_func_capture.mimi (default route) report:' \
    'capture-explicit|Memcheck actor_nested_func_capture.mimi (explicit --mir) report:' \
    'shadow-default|Memcheck actor_nested_func_shadow.mimi (default route) report:' \
    'shadow-explicit|Memcheck actor_nested_func_shadow.mimi (explicit --mir) report:'; do
    report_label="${report_spec%%|*}"
    report_marker="${report_spec#*|}"
    report_file="$ARTIFACT_DIR/l3-memcheck/$report_label.txt"
    if ! extract_memcheck_report "$report_label" "$report_marker" "$l3_stderr" "$report_file"; then
        printf 'FAIL missing or duplicate Memcheck detail for %s\n' "$report_label" \
            > "$ARTIFACT_DIR/l3-memcheck/$report_label.validation.txt"
        memcheck_status=1
        continue
    fi
    if report_errors="$(validate_memcheck_report "$report_file" 2>&1)"; then
        printf 'PASS unique Memcheck and heap/leak/error summaries; zero lost/suppressed/errors; exactly 192 bytes reachable in one Actor registry allocation\n' \
            > "$ARTIFACT_DIR/l3-memcheck/$report_label.validation.txt"
    else
        printf 'FAIL Memcheck report did not match the exact clean-summary contract\n%s\n' \
            "$report_errors" \
            > "$ARTIFACT_DIR/l3-memcheck/$report_label.validation.txt"
        memcheck_status=1
    fi
done
if [[ "$memcheck_status" -ne 0 ]]; then
    GATE_FAILURES=$((GATE_FAILURES + 1))
fi

source_snapshot "$ARTIFACT_DIR/source.inputs.after.nul" \
    "$ARTIFACT_DIR/source.after.sha256"
if [[ $? -ne 0 ]]; then
    SNAPSHOT_FAILURE=1
    printf 'FAIL could not capture after source snapshot\n' > "$ARTIFACT_DIR/source-snapshot.validation.txt"
else
    sha256sum "$ARTIFACT_DIR/source.inputs.after.nul" \
        > "$ARTIFACT_DIR/source.after.inventory.sha256"
    sha256sum "$ARTIFACT_DIR/source.after.sha256" > "$ARTIFACT_DIR/source.after.manifest.sha256"
    source_file_count=0
    while IFS= read -r -d '' _source_file; do
        source_file_count=$((source_file_count + 1))
    done < "$ARTIFACT_DIR/source.inputs.before.nul"
    if cmp -s "$ARTIFACT_DIR/source.inputs.before.nul" \
            "$ARTIFACT_DIR/source.inputs.after.nul" &&
        cmp -s "$ARTIFACT_DIR/source.before.sha256" "$ARTIFACT_DIR/source.after.sha256" &&
        (cd "$ARTIFACT_DIR" && sha256sum --check --status \
            source.before.inventory.sha256 source.before.manifest.sha256 \
            source.after.inventory.sha256 source.after.manifest.sha256); then
        printf 'PASS %s source paths and their SHA-256 manifests are byte-identical before/after replay\n' \
            "$source_file_count" > "$ARTIFACT_DIR/source-snapshot.validation.txt"
    else
        SNAPSHOT_FAILURE=1
        printf 'FAIL source path inventory or file hashes changed during replay\n' \
            > "$ARTIFACT_DIR/source-snapshot.validation.txt"
    fi
fi

if [[ "$GATE_FAILURES" -eq 0 && "$SNAPSHOT_FAILURE" -eq 0 ]]; then
    printf 'PASS Actor current-source L1/L2/L3 replay including actor-method-effects-l1; fixtures capture=15\\n shadow=42\\n101\\n; four native Memcheck reports are clean\n' \
        > "$ARTIFACT_DIR/result.txt"
    RUN_STATUS=0
else
    printf 'FAIL Actor current-source replay: gate_failures=%s source_snapshot_failure=%s; partial evidence retained\n' \
        "$GATE_FAILURES" "$SNAPSHOT_FAILURE" > "$ARTIFACT_DIR/result.txt"
    RUN_STATUS=1
fi

printf 'artifact_dir=%s\nrun_status=%s\n' "$ARTIFACT_DIR" "$RUN_STATUS"
exit "$RUN_STATUS"
