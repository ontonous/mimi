#!/usr/bin/env bash
# The default entry point performs the static inventory and every required
# dynamic probe. --static-only is reserved for the static helper and never
# represents the complete owner audit.
set -euo pipefail

ROOT_DIR="$(cd -P "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="$ROOT_DIR/src"

static_only=0
if [ "${1:-}" = "--static-only" ]; then
    static_only=1
    shift
    if [ "$#" -ne 0 ]; then
        printf 'static_audit_error=unexpected_arguments\n' >&2
        exit 2
    fi
elif [ "$#" -ne 0 ]; then
    printf 'owner_audit_error=unexpected_arguments value=%s\n' "$*" >&2
    exit 2
fi

if [ "${MIMI_LEGACY_OWNER_AUDIT_SKIP_DYNAMIC_PROBES:-0}" != "0" ]; then
    printf 'owner_audit_error=dynamic_probe_skip_is_forbidden value=%s\n' \
        "$MIMI_LEGACY_OWNER_AUDIT_SKIP_DYNAMIC_PROBES" >&2
    exit 2
fi

if [ "$static_only" -eq 1 ]; then
    canonical_target_dir=''
    AUDIT_EVIDENCE_DIR=''
else
requested_target_dir="${MIMI_LEGACY_OWNER_AUDIT_TARGET_DIR:-${CARGO_TARGET_DIR:-/tmp/mimi-legacy-owner-audit-target}}"
if [[ "$requested_target_dir" != /* ]]; then
    printf 'owner_audit_error=target_dir_must_be_absolute path=%s\n' "$requested_target_dir" >&2
    exit 2
fi
canonical_target_dir="$(realpath -m -- "$requested_target_dir")" || {
    printf 'owner_audit_error=cannot_resolve_target_dir path=%s\n' "$requested_target_dir" >&2
    exit 2
}
if [[ "$canonical_target_dir" == "$ROOT_DIR" || "$canonical_target_dir" == "$ROOT_DIR"/* ]]; then
    printf 'owner_audit_error=target_dir_inside_repository path=%s\n' "$canonical_target_dir" >&2
    exit 2
fi
mkdir -p -- "$canonical_target_dir"
canonical_target_dir="$(realpath -e -- "$canonical_target_dir")" || {
    printf 'owner_audit_error=cannot_create_target_dir path=%s\n' "$requested_target_dir" >&2
    exit 2
}
if [[ "$canonical_target_dir" == "$ROOT_DIR" || "$canonical_target_dir" == "$ROOT_DIR"/* ]]; then
    printf 'owner_audit_error=target_dir_resolves_inside_repository path=%s\n' "$canonical_target_dir" >&2
    exit 2
fi

# Probe artifacts are always written outside the repository.  The environment
# variable selects an external base directory; each invocation gets a unique
# child so reruns never overwrite earlier evidence.  The default is /tmp (or
# /var/tmp when /tmp resolves inside this checkout), never ROOT_DIR/evidence.
readonly AUDIT_EVIDENCE_ENV=MIMI_LEGACY_OWNER_AUDIT_EVIDENCE_DIR
requested_evidence_base="${!AUDIT_EVIDENCE_ENV:-}"
if [ -n "$requested_evidence_base" ]; then
    if [[ "$requested_evidence_base" != /* ]]; then
        printf '%s must be an absolute directory path; got: %s\n' \
            "$AUDIT_EVIDENCE_ENV" "$requested_evidence_base" >&2
        exit 2
    fi
else
    requested_evidence_base=/tmp
fi

is_inside_repository() {
    local candidate="$1"
    [[ "$candidate" == "$ROOT_DIR" || "$candidate" == "$ROOT_DIR"/* ]]
}

canonical_evidence_base="$(realpath -m -- "$requested_evidence_base")" || {
    printf 'cannot resolve evidence directory: %s\n' "$requested_evidence_base" >&2
    exit 2
}
if is_inside_repository "$canonical_evidence_base"; then
    if [ -n "${!AUDIT_EVIDENCE_ENV:-}" ]; then
        printf '%s resolves inside the repository; choose an external directory: %s\n' \
            "$AUDIT_EVIDENCE_ENV" "$canonical_evidence_base" >&2
        exit 2
    fi
    canonical_evidence_base="$(realpath -m -- /var/tmp)" || exit 2
    if is_inside_repository "$canonical_evidence_base"; then
        printf 'no external default evidence directory is available\n' >&2
        exit 2
    fi
fi
mkdir -p -- "$canonical_evidence_base"
canonical_evidence_base="$(realpath -e -- "$canonical_evidence_base")" || {
    printf 'cannot create evidence directory: %s\n' "$requested_evidence_base" >&2
    exit 2
}
if is_inside_repository "$canonical_evidence_base"; then
    printf 'evidence directory resolves inside the repository: %s\n' \
        "$canonical_evidence_base" >&2
    exit 2
fi
run_stamp="$(date -u +%Y%m%dT%H%M%SZ)-$$-${RANDOM}"
AUDIT_EVIDENCE_DIR="$canonical_evidence_base/legacy-owner-audit-$run_stamp"
mkdir -- "$AUDIT_EVIDENCE_DIR"
AUDIT_EVIDENCE_DIR="$(realpath -e -- "$AUDIT_EVIDENCE_DIR")"
if is_inside_repository "$AUDIT_EVIDENCE_DIR"; then
    printf 'run evidence directory resolves inside the repository: %s\n' \
        "$AUDIT_EVIDENCE_DIR" >&2
    exit 2
fi

printf 'evidence_dir=%s\n' "$AUDIT_EVIDENCE_DIR"
fi

write_probe_source_hashes() {
    local destination="$1"
    local source_root source_file
    (
        cd "$ROOT_DIR"
        {
            for source_root in src tests std; do
                if [ -d "$source_root" ]; then
                    find "$source_root" -type f -print0
                fi
            done
            for source_file in \
                Cargo.toml Cargo.lock build.rs \
                .cargo/config.toml rust-toolchain.toml rust-toolchain \
                scripts/audit-mir-legacy-owners.sh \
                scripts/audit-mir-legacy-owners-static.sh; do
                if [ -f "$source_file" ]; then
                    printf '%s\0' "$source_file"
                fi
            done
        } | LC_ALL=C sort -zu | while IFS= read -r -d '' source_file; do
            sha256sum -- "$source_file"
        done
    ) > "$destination"
    sha256sum -- "$destination" | awk '{print $1}' > "$destination.sha256"
}

# The CheckedProgram surface-body snapshot and accessor have been physically
# retired. Keep the absence check independent of production reachability: no
# test-only accessor or trace API may quietly reappear.
readonly EXPECTED_PRODUCTION_RAW_AST_CALL_SITES=0
readonly EXPECTED_PRODUCTION_COMPILE_FUNC_LEGACY_CALL_SITES=0
readonly EXPECTED_PRODUCTION_COMPILE_GENERIC_FUNC_CALL_SITES=0
readonly EXPECTED_SCALAR_FFI_DIRECT_EXPRESSION_LEGACY_REFS=0
# The deletion conditions are part of the compatibility contract. Keep a
# digest beside each prose condition so scope changes are detected by the
# gate and reviewed together with the migration that changes the condition.
audit_failed=0
closed_scalar_marker_sequence=()

emit_closed_scalar_marker() {
    local marker="$1"
    closed_scalar_marker_sequence+=("$marker")
    printf '%s\n' "$marker"
}

printf 'schema=canonical-mir-legacy-owner-boundary-audit-v2\n'
printf 'root=%s\n' "$ROOT_DIR"

# Keep executable behavior evidence for each active compatibility owner.
# Removing these tests without equivalent canonical-route or explicit
# rejection coverage must fail this audit.
owner_evidence() {
    local owner="$1"
    local source_file="$2"
    local test_name="$3"
    local scope="$4"
    local expected_marker="$5"
    local test_path="$ROOT_DIR/$source_file"
    if ! rg -q "^[[:space:]]*fn ${test_name}\\(" "$test_path"; then
        printf 'owner_audit_error=%s missing_evidence_test=%s::%s\n' \
            "$owner" "$source_file" "$test_name" >&2
        audit_failed=1
        return
    fi
    if ! sed -n "/^[[:space:]]*fn ${test_name}(/,/^[[:space:]]*#\\[test\\]/p" "$test_path" \
        | rg -F "${expected_marker}" >/dev/null; then
        printf 'owner_audit_error=%s evidence_test_missing_marker=%s marker=%s\n' \
            "$owner" "$test_name" "$expected_marker" >&2
        audit_failed=1
        return
    fi
    printf 'owner=%s evidence_scope=%s evidence_test=%s::%s evidence_marker=%s\n' \
        "$owner" "$scope" "$source_file" "$test_name" "$expected_marker"
}

owner_evidence \
    CodegenLegacyRemainder \
    src/codegen/tests.rs \
    compile_checked_routes_generic_ignored_bool_binding_projection_without_legacy_access \
    'generic ignored-bool binding uses canonical MIR without any legacy owner' \
    'test_legacy_codegen_function_calls().is_empty()'
owner_evidence \
    FfiVerifierCompatibility \
    src/verifier/tests.rs \
    typed_ffi_symbolic_string_contexts_defer_without_legacy_access \
    'typed String FFI context reports checker-owned coverage without any legacy owner' \
    'VerifStatus::NotInTrustedSubset'
owner_evidence \
    FfiVerifierCompatibility \
    src/verifier/tests.rs \
    typed_ffi_string_record_ensures_use_checker_receipt_and_fresh_result \
    'checked verifier-only owner slice proves Ready repr(C) String-field Ensures' \
    'Some("verifier-ffi-owner-slice-v1")'
owner_evidence \
    FfiVerifierCompatibility \
    src/verifier/tests.rs \
    typed_ffi_ready_unsupported_record_ensures_preserves_requires_proof \
    'unsupported Ready Ensures stays call-local without discarding Requires proof' \
    'result.artifact.is_none()'
owner_evidence \
    FfiVerifierCompatibility \
    src/verifier/tests.rs \
    typed_ffi_aggregate_nested_string_fields_preserve_nul_unicode_and_length \
    'nested repr(C) String fields preserve exact caller identity through MIR' \
    'Some("verifier-ffi-owner-slice-v1")'
owner_evidence \
    FfiVerifierCompatibility \
    tests/real_world_cli.rs \
    canonical_ffi_string_record_default_dual_cli_proven_disproven_nits \
    'default verify reports proven, disproven, and unsupported String-record Ensures' \
    '"? unknown"'
flow_dual_open_migration_evidence() {
    local verifier_dir="$SRC_DIR/verifier"
    local parity_tests="$verifier_dir/flow_retirement_tests.rs"
    local scalar transition kinds body test_name

    if [ -e "$verifier_dir/flow.rs" ]; then
        printf 'owner_audit_error=unexpected_flow_ast_owner_file_present path=src/verifier/flow.rs\n' >&2
        audit_failed=1
        return
    fi
    for marker in \
        'FlowVerifierCompatibility' \
        'DualVerifierCompatibility' \
        'flow_verify_file_with_hashes' \
        'pub(crate) fn verify_file(' \
        'verify_transition_vir(' \
        'lower_transition_to_vir(' \
        'synthesize_transition_func(' \
        'TypestateAxioms' \
        'typestate_context'; do
        if rg -n -F --glob '*.rs' "$marker" "$SRC_DIR/verifier" "$SRC_DIR/core/resolved" >/dev/null; then
            printf 'owner_audit_error=removed_flow_dual_adapter_symbol_reappeared marker=%s\n' "$marker" >&2
            audit_failed=1
            return
        fi
    done

    scalar='flow_scalar_migration_keeps_mir_and_dual_verdicts'
    transition='flow_transition_invariant_and_field_projection_owner_parity'
    kinds='canonical_mir_transition_preserves_contract_kinds'
    for test_name in "$scalar" "$transition" "$kinds"; do
        body="$(sed -n "/^[[:space:]]*fn ${test_name}(/,/^[[:space:]]*#\\[test\\]/p" "$parity_tests")"
        if [ -z "$body" ]; then
            printf 'owner_audit_error=missing_mir_native_flow_parity_test test=%s\n' "$test_name" >&2
            audit_failed=1
            return
        fi
    done
    body="$(sed -n "/^[[:space:]]*fn ${scalar}(/,/^[[:space:]]*#\\[test\\]/p" "$parity_tests")"
    for marker in 'VerifStatus::Proven' 'VerifStatus::Disproven' 'assert_public_routes_match_exact_mir'; do
        if ! printf '%s\n' "$body" | rg -F "$marker" >/dev/null; then
            printf 'owner_audit_error=flow_scalar_mir_dual_evidence_missing test=%s marker=%s\n' \
                "$scalar" "$marker" >&2
            audit_failed=1
            return
        fi
    done
    body="$(sed -n "/^[[:space:]]*fn ${transition}(/,/^[[:space:]]*#\\[test\\]/p" "$parity_tests")"
    for marker in 'VerifStatus::Proven' 'VerifStatus::Disproven' 'assert_resolved_primary_matches_exact_mir' 'assert_public_routes_match_exact_mir'; do
        if ! printf '%s\n' "$body" | rg -F "$marker" >/dev/null; then
            printf 'owner_audit_error=flow_transition_mir_dual_evidence_missing test=%s marker=%s\n' \
                "$transition" "$marker" >&2
            audit_failed=1
            return
        fi
    done
    body="$(sed -n "/^[[:space:]]*fn ${kinds}(/,/^[[:space:]]*#\\[test\\]/p" "$parity_tests")"
    for marker in 'MirContractKind::Requires' 'MirContractKind::Ensures' 'MirContractKind::Invariant'; do
        if ! printf '%s\n' "$body" | rg -F "$marker" >/dev/null; then
            printf 'owner_audit_error=flow_transition_mir_contract_kind_evidence_missing marker=%s\n' "$marker" >&2
            audit_failed=1
            return
        fi
    done
    printf 'flow_dual_legacy_ast_observation=source-adapter-absent;capability_migration=open;coverage_status=partial;evidence=%s+%s+%s;retirement_status=unproven\n' \
        "$scalar" "$transition" "$kinds"
}

flow_dual_open_migration_evidence

# Generic trait specialization tests must exercise the checker-owned impl
# catalog and canonical MIR consumers.  The old compatibility fixture for
# `List<T>::read` did not represent a production route and directly invoked
# the test-only surface body compiler, obscuring that MIR already carries the
# exact static method owner.  Keep its replacement tied to real positive
# native/bytecode/reference execution and a cross-specialization rejection.
codegen_generic_trait_mir_evidence() {
    local tests="$ROOT_DIR/src/codegen/tests.rs"
    local context
    local name
    local default_route_stdout_marker='"33\n22\n11\n"'

    name='checked_generic_trait_specializations_install_exact_vtables_from_checked_catalog'
    context="$(sed -n "/^[[:space:]]*fn ${name}(/,/^[[:space:]]*#\\[test\\]/p" "$tests")"
    for marker in \
        'install_checked_declaration_names(&program)' \
        'compile_vtables()' \
        'test_legacy_codegen_function_calls().is_empty()'; do
        if ! printf '%s\n' "$context" | rg -F "$marker" >/dev/null; then
            printf 'owner_audit_error=generic_trait_checked_vtable_evidence_missing test=%s marker=%s\n' \
                "$name" "$marker" >&2
            audit_failed=1
            return
        fi
    done
    if printf '%s\n' "$context" | rg -F 'compile_file_with_resolved(' >/dev/null; then
        printf 'owner_audit_error=generic_trait_vtable_test_reopened_surface_compiler test=%s\n' \
            "$name" >&2
        audit_failed=1
        return
    fi

    name='canonical_mir_dispatches_generic_trait_impls_by_exact_method_owner'
    context="$(sed -n "/^[[:space:]]*fn ${name}(/,/^[[:space:]]*#\\[test\\]/p" "$tests")"
    for marker in \
        'MirProgram::from_checked_program(&program)' \
        'execute_with_output(&main_owner, &[])' \
        'route_receipt(' \
        'compile_mir_program_with_route_receipt(&mir, &receipt)' \
        '.compile_mir_native_with_route_receipt(&mir, &receipt)' \
        'has_static_generic_trait_specialization_candidate(&program)' \
        '.compile_checked(&program)' \
        '22\n11\n' \
        'cross-specialization receiver ABI drift must fail closed'; do
        if ! printf '%s\n' "$context" | rg -F "$marker" >/dev/null; then
            printf 'owner_audit_error=generic_trait_mir_execution_evidence_missing test=%s marker=%s\n' \
                "$name" "$marker" >&2
            audit_failed=1
            return
        fi
    done
    if printf '%s\n' "$context" | rg -F 'compile_file_with_resolved(' >/dev/null; then
        printf 'owner_audit_error=generic_trait_dispatch_test_reopened_surface_compiler test=%s\n' \
            "$name" >&2
        audit_failed=1
        return
    fi
    name='generic_trait_specializations_use_default_canonical_route'
    context="$(sed -n "/^[[:space:]]*fn ${name}(/,/^[[:space:]]*#\\[test\\]/p" \
        "$ROOT_DIR/src/main/canonical_dispatch.rs")"
    for marker in \
        'has_static_generic_trait_specialization_candidate(&checked)' \
        'select_default_route(&checked, &file)' \
        'STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE' \
        "$default_route_stdout_marker"; do
        if ! printf '%s\n' "$context" | rg -F "$marker" >/dev/null; then
            printf 'owner_audit_error=generic_trait_default_route_evidence_missing test=%s marker=%s\n' \
                "$name" "$marker" >&2
            audit_failed=1
            return
        fi
    done
    printf 'generic_trait_default_route_stdout_marker=%s\n' "$default_route_stdout_marker"
    name='generic_trait_specialization_does_not_admit_a_user_flow'
    context="$(sed -n "/^[[:space:]]*fn ${name}(/,/^[[:space:]]*#\\[test\\]/p" \
        "$ROOT_DIR/src/main/canonical_dispatch.rs")"
    for marker in \
        'checked.flow("Counter").is_some()' \
        '!mimi::codegen::has_static_generic_trait_specialization_candidate(&checked)'; do
        if ! printf '%s\n' "$context" | rg -F "$marker" >/dev/null; then
            printf 'owner_audit_error=generic_trait_real_flow_negative_missing test=%s marker=%s\n' \
                "$name" "$marker" >&2
            audit_failed=1
            return
        fi
    done
    local cli_test="$ROOT_DIR/tests/canonical_mir_static_generic_trait_cli.rs"
    for marker in \
        'static_generic_trait_specialization_runs_through_default_and_explicit_mir_cli_routes' \
        'static-generic-trait-specialization-v1' \
        'explicit canonical (run-canonical-v1)' \
        '33\n22\n11\n'; do
        if ! rg -F "$marker" "$cli_test" >/dev/null; then
            printf 'owner_audit_error=generic_trait_cli_route_evidence_missing marker=%s\n' \
                "$marker" >&2
            audit_failed=1
            return
        fi
    done
    printf 'generic_trait_mir_evidence=checked-vtables+reference+receipted-bytecode+receipted-direct-native+compile_checked+default-route+explicit-mir-cli+real-flow-reject+cross-specialization-reject\n'
}

codegen_generic_trait_mir_evidence

# The old body snapshot/accessor is an explicitly retired surface. Keep a
# fail-closed inventory of every former storage/access/trace symbol, while
# retaining source provenance and route facts in CheckedProgram.
checked_program_surface_body_snapshot_retired() {
    local forbidden_pattern offenders resolved_tests
    forbidden_pattern='(^|[^[:alnum:]_])(legacy_file[[:space:]]*:|legacy_body_file[[:space:]]*\(|LegacyBodyConsumer|TEST_LEGACY_BODY_ACCESS|test_legacy_body_access|reset_test_legacy_body_access)([^[:alnum:]_]|$)'
    offenders="$(rg -n --glob '*.rs' --glob '!**/audit_fix_scripts.rs' \
        "$forbidden_pattern" "$SRC_DIR" || true)"
    if [ -n "$offenders" ]; then
        printf 'owner_audit_error=checked_program_surface_body_snapshot_present\n%s\n' \
            "$offenders" >&2
        audit_failed=1
        return
    fi
    resolved_tests="$ROOT_DIR/src/core/resolved/tests.rs"
    for marker in \
        'fn resolved_lowering_reads_source_provenance_from_checker_registry(' \
        'program.source_registry().key(source_id)' \
        'fn checked_program_keeps_import_route_fact_without_surface_snapshot(' \
        'program.has_imports()'; do
        if ! rg -F "$marker" "$resolved_tests" >/dev/null; then
            printf 'owner_audit_error=checked_program_surface_fact_test_missing marker=%s\n' \
                "$marker" >&2
            audit_failed=1
            return
        fi
    done
    printf 'checked_program_surface_body_snapshot=physically-retired;source_registry=retained;has_imports=retained\n'
}

test_only_codegen_adapter_module_is_cfg_test() {
    if ! rg -U -q '#\[cfg\(test\)\]\nmod tests;' "$ROOT_DIR/src/codegen/mod.rs"; then
        printf 'owner_audit_error=test_only_codegen_adapter_module_not_cfg_test\n' >&2
        audit_failed=1
        return
    fi
    printf 'test_only_codegen_adapter_module=src/codegen/mod.rs::tests+cfg-test\n'
}

# The closed scalar guard may live in the public entry function while the
# retained accessor is delegated to a compatibility helper. Verify the guard
# in its owning entry context separately so a count-only audit cannot hide a
# route that re-enters the legacy owner before canonical admission.
owner_closed_route_guard() {
    local owner="$1"
    local source_file="$2"
    local start_function="$3"
    local end_function="$4"
    local guard_name="$5"
    local guard_pattern="$6"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$guard_pattern" >/dev/null; then
        printf 'owner_audit_error=%s closed_route_guard_missing=%s::%s\n' \
            "$owner" "$source_file" "$guard_name" >&2
        audit_failed=1
        return
    fi
    printf 'owner=%s closed_route_guard=%s context=%s::%s\n' \
        "$owner" "$guard_name" "$source_file" "$start_function"
}

# Bind the scalar route's receipt label to the canonical profile declaration.
# This prevents a renamed or copied receipt from silently drifting away from
# the profile that the closed-route guard is meant to protect.
route_receipt_profile_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local profile_name="$4"
    local receipt_label="$5"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "route_receipt(\"${receipt_label}\")" >/dev/null; then
        printf 'owner_audit_error=scalar_route_receipt_missing=%s::%s label=%s\n' \
            "$source_file" "$start_function" "$receipt_label" >&2
        audit_failed=1
        return
    fi
    if ! rg -F "Self::${profile_name} => \"${receipt_label}\"" \
        "$ROOT_DIR/src/core/mir/route.rs" >/dev/null; then
        printf 'owner_audit_error=scalar_route_profile_receipt_mapping_missing=CanonicalMirRouteProfile::%s label=%s\n' \
            "$profile_name" "$receipt_label" >&2
        audit_failed=1
        return
    fi
    printf 'scalar_route_receipt_binding=CanonicalMirRouteProfile::%s->%s consumer=%s::%s\n' \
        "$profile_name" "$receipt_label" "$source_file" "$start_function"
}

checked_program_surface_body_snapshot_retired
test_only_codegen_adapter_module_is_cfg_test

# Generic surface-AST verifier helpers remain test-only for the still-active
# FFI audit. The Flow state-machine engine, file batch adapter, and transition
# VIR adapter have been physically removed; keep those deletions explicit.
test_only_ast_verifier_modules() {
    local module
    for module in expr func helpers vir; do
        if ! rg -U -q "#\[cfg\(test\)\]\n(?:pub[[:space:]]+)?mod ${module};" \
            "$ROOT_DIR/src/verifier/mod.rs"; then
            printf 'owner_audit_error=verifier_ast_module_not_cfg_test module=%s\n' "$module" >&2
            audit_failed=1
            return
        fi
    done
    if rg -U -q '#\[cfg\(test\)\]\n(?:pub[[:space:]]+)?mod flow;' \
        "$ROOT_DIR/src/verifier/mod.rs" \
        || [ -e "$ROOT_DIR/src/verifier/flow.rs" ]; then
        printf 'owner_audit_error=retired_flow_ast_module_reintroduced\n' >&2
        audit_failed=1
        return
    fi
    if rg -Fq 'pub(crate) fn verify_file(' "$ROOT_DIR/src/verifier/ctx.rs" \
        || rg -Fq 'pub(crate) fn verify_items(' "$ROOT_DIR/src/verifier/func.rs" \
        || rg -Fq 'pub(crate) fn verify_transition(' "$ROOT_DIR/src/verifier/func.rs" \
        || rg -Fq 'pub(crate) fn verify_transition_vir(' "$ROOT_DIR/src/verifier/func.rs" \
        || rg -Fq 'pub fn lower_transition_to_vir(' "$ROOT_DIR/src/verifier/vir.rs"; then
        printf 'owner_audit_error=retired_flow_or_batch_verifier_adapter_reintroduced\n' >&2
        audit_failed=1
        return
    fi
    if rg -Fq 'pub(crate) fn mock_verify_file(' "$ROOT_DIR/src/verifier/helpers.rs"; then
        printf 'owner_audit_error=retired_ast_mock_file_adapter_reintroduced\n' >&2
        audit_failed=1
        return
    fi
    printf 'test_only_ast_verifier_modules=expr,func,helpers,vir;flow-owner+batch+transition-adapters=deleted\n'
}

test_only_ast_verifier_modules

# Native Codegen has no retained surface-file adapter. The Bytecode source
# compiler is a separate, explicitly named compatibility consumer for
# `run`/`test`/`disasm`: each production call must sit behind the dispatcher’s
# `DefaultMirRoute::Legacy` arm, while the native build has no such ingress.
# Keep this distinction visible so removing LLVM legacy edges is not confused
# with silently deleting the interpreter's graph-outside compatibility route.
codegen_legacy_consumer_boundary() {
    if rg -n \
        '^[[:space:]]*(pub\([^)]*\)[[:space:]]+)?(pub[[:space:]]+)?fn (compile_file|compile_file_inner|compile_file_with_resolved)\(' \
        "$ROOT_DIR/src/codegen/compile.rs"; then
        printf 'owner_audit_error=native_codegen_surface_file_adapter_reintroduced\n' >&2
        audit_failed=1
        return
    fi
    if rg -n 'BytecodeCompiler|\.compile_file\(' "$ROOT_DIR/src/main/build.rs" >/dev/null; then
        printf 'owner_audit_error=native_build_reaches_bytecode_legacy_compiler\n' >&2
        audit_failed=1
        return
    fi
    local path
    for path in src/main/run.rs src/main/test.rs src/main/disasm_cmd.rs; do
        if ! rg -q 'DefaultMirRoute::Legacy' "$ROOT_DIR/$path" \
            || ! rg -q '\.compile_file\(' "$ROOT_DIR/$path" \
            || ! rg -q 'report_legacy_route' "$ROOT_DIR/$path"; then
            printf 'owner_audit_error=bytecode_legacy_route_not_explicit path=%s\n' "$path" >&2
            audit_failed=1
            return
        fi
    done
    printf 'codegen_legacy_consumer_boundary=llvm_surface_file_adapter_absent;native_build_no_bytecode_legacy;run-test-disasm_explicit_legacy_route\n'
}

codegen_legacy_consumer_boundary

owner_closed_route_guard \
    CodegenLegacyRemainder \
    src/codegen/compile.rs \
    'pub fn compile_checked(' \
    'fn try_compile_exact_migrated_mir_island(' \
    try_compile_exact_migrated_mir_island \
    'if let Some(canonical) = self.try_compile_exact_migrated_mir_island(program)?'
owner_closed_route_guard \
    FfiVerifierCompatibility \
    src/verifier/mod.rs \
    'pub fn verify_ffi_checked_with_source_hash(' \
    'fn ffi_infrastructure_error(' \
    scalar_ffi_admission \
    'if profile.is_admitted(admission)'

# Direct CodeGenerator callers must enforce the same called-extern boundary
# as CLI dispatch before the migrated-island probe can return a compatibility
# disposition to the resolved/legacy emitters.
direct_native_ffi_boundary_guard() {
    local context boundary_line route_line
    context="$(sed -n '/pub fn compile_checked(/,/^    fn try_compile_exact_migrated_mir_island(/p' \
        "$ROOT_DIR/src/codegen/compile.rs")"
    for pattern in \
        'scalar_ffi_boundary_reason(program)' \
        'MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE' \
        'canonical scalar FFI declaration boundary:'; do
        if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=direct_native_ffi_boundary_guard_missing=src/codegen/compile.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    boundary_line="$(printf '%s\n' "$context" | awk '!found && /scalar_ffi_boundary_reason\(program\)/ { found = NR } END { if (found) print found }')"
    route_line="$(printf '%s\n' "$context" | awk '!found && /try_compile_exact_migrated_mir_island\(program\)/ { found = NR } END { if (found) print found }')"
    if [ -z "$boundary_line" ] || [ -z "$route_line" ] || [ "$boundary_line" -ge "$route_line" ]; then
        printf 'owner_audit_error=direct_native_ffi_boundary_guard_not_before_route_probe\n' >&2
        audit_failed=1
        return
    fi
    printf 'direct_native_ffi_boundary_guard=before-route-probe-and-legacy-disposition consumer=src/codegen/compile.rs::pub fn compile_checked(\n'
}

direct_native_ffi_boundary_guard

# R6-1111: the raw-AST bytecode compiler and VM must not retain the old host
# loader ingress. The legacy instruction is kept only as a rejected bytecode
# shape, with VM-wide validation before execution or host lookup.
bytecode_vm_canonical_ffi_preflight_order() {
    local vm="$1"
    local structural_context
    local execution_context
    local ffi_validation_line
    local frame_validation_line
    local any_cfg_validation_line
    local structure_line
    local begin_line
    local worker_symbol_line
    local normal_symbol_line
    local worker_execute_line
    local normal_execute_line
    structural_context="$(sed -n '/fn preflight_canonical_program_structure(/,/^    }/p' "$vm")"
    ffi_validation_line="$(printf '%s\n' "$structural_context" | awk '!found && index($0, "self.validate_canonical_ffi_program()?") { found = NR } END { if (found) print found }')"
    frame_validation_line="$(printf '%s\n' "$structural_context" | awk '!found && index($0, "self.validate_bytecode_call_frames()?") { found = NR } END { if (found) print found }')"
    any_cfg_validation_line="$(printf '%s\n' "$structural_context" | awk '!found && index($0, "self.validate_canonical_managed_any_cfg_program()") { found = NR } END { if (found) print found }')"
    if [ -z "$structural_context" ] || [ -z "$ffi_validation_line" ] \
        || [ -z "$frame_validation_line" ] || [ -z "$any_cfg_validation_line" ] \
        || [ "$ffi_validation_line" -ge "$frame_validation_line" ] \
        || [ "$frame_validation_line" -ge "$any_cfg_validation_line" ]; then
        printf 'owner_audit_error=bytecode_vm_ffi_preflight_marker_or_order_missing function=preflight_canonical_program_structure expected=canonical-ffi-before-call-frame-before-managed-any-cfg-seal\n' >&2
        audit_failed=1
        return
    fi
    execution_context="$(sed -n '/fn with_canonical_actor_execution/,/Counters are observational/p' "$vm")"
    structure_line="$(printf '%s\n' "$execution_context" | awk '!found && index($0, "self.preflight_canonical_program_structure()?") { found = NR } END { if (found) print found }')"
    begin_line="$(printf '%s\n' "$execution_context" | awk '!found && index($0, ".begin_execution(&self.program, self.max_children)?") { found = NR } END { if (found) print found }')"
    worker_symbol_line="$(printf '%s\n' "$execution_context" | awk '!found && index($0, "self.preflight_canonical_ffi_symbols()?") { found = NR } END { if (found) print found }')"
    normal_symbol_line="$(printf '%s\n' "$execution_context" | awk '!found && index($0, "match self.preflight_canonical_ffi_symbols()") { found = NR } END { if (found) print found }')"
    worker_execute_line="$(printf '%s\n' "$execution_context" | awk -v symbol_line="$worker_symbol_line" '!found && NR > symbol_line && index($0, "return execute(self);") { found = NR } END { if (found) print found }')"
    normal_execute_line="$(printf '%s\n' "$execution_context" | awk -v symbol_line="$normal_symbol_line" '!found && NR > symbol_line && index($0, "Ok(()) => execute(self),") { found = NR } END { if (found) print found }')"
    if [ -z "$structure_line" ] || [ -z "$begin_line" ] \
        || [ -z "$worker_symbol_line" ] || [ -z "$normal_symbol_line" ] \
        || [ -z "$worker_execute_line" ] || [ -z "$normal_execute_line" ]; then
        printf 'owner_audit_error=bytecode_vm_ffi_preflight_order_marker_missing function=with_canonical_actor_execution\n' >&2
        audit_failed=1
        return
    fi
    if [ "$structure_line" -ge "$begin_line" ] \
        || [ "$structure_line" -ge "$worker_symbol_line" ] \
        || [ "$structure_line" -ge "$normal_symbol_line" ] \
        || [ "$begin_line" -ge "$normal_symbol_line" ] \
        || [ "$worker_symbol_line" -ge "$worker_execute_line" ] \
        || [ "$normal_symbol_line" -ge "$normal_execute_line" ]; then
        printf 'owner_audit_error=bytecode_vm_ffi_preflight_order_drift expected=canonical-receipts-before-actor-root-and-host-symbols-and-symbols-before-execution\n' >&2
        audit_failed=1
        return
    fi
    printf 'bytecode_vm_ffi_preflight_order=canonical-receipts+ffi-descriptors-before-actor-root;worker-and-normal-host-symbol-preflight-before-first-instruction\n'
}

retired_receiptless_bytecode_ffi_path() {
    local compiler="$ROOT_DIR/src/interp/bytecode/compiler.rs"
    local vm="$ROOT_DIR/src/interp/bytecode/vm.rs"
    local tests="$ROOT_DIR/src/interp/bytecode/mod.rs"
    if rg -q 'fc\.emit\(Op::CallExtern' "$compiler"; then
        printf 'owner_audit_error=raw_ast_bytecode_still_emits_receiptless_call_extern\n' >&2
        audit_failed=1
        return
    fi
    if ! rg -F 'raw-AST bytecode cannot execute extern call' "$compiler" >/dev/null; then
        printf 'owner_audit_error=raw_ast_bytecode_extern_boundary_missing\n' >&2
        audit_failed=1
        return
    fi
    if rg -n '\bFfiRuntime\b|call_extern_idx|vm_as_runner|call_extern_with_runner_ptr' "$vm" >/dev/null; then
        printf 'owner_audit_error=bytecode_vm_retains_ast_backed_ffi_loader\n' >&2
        audit_failed=1
        return
    fi
    for marker in \
        'receiptless legacy CallExtern' \
        'receiptless legacy CallExtern execution is disabled'; do
        if ! rg -F "$marker" "$vm" >/dev/null; then
            printf 'owner_audit_error=bytecode_vm_ffi_preflight_marker_missing marker=%s\n' \
                "$marker" >&2
            audit_failed=1
            return
        fi
    done
    bytecode_vm_canonical_ffi_preflight_order "$vm"
    if [ "$audit_failed" -ne 0 ]; then
        return
    fi
    for test_name in \
        raw_ast_compiler_rejects_receiptless_extern_calls \
        raw_ast_compiler_rejects_deterministic_receiptless_extern_matrix \
        vm_rejects_receiptless_extern_index_matrix_before_stdout_or_host_lookup \
        vm_checks_receiptless_extern_register_window_before_rejecting; do
        if ! rg -q "^[[:space:]]*fn ${test_name}\\(" "$tests"; then
            printf 'owner_audit_error=bytecode_ffi_retirement_test_missing=%s\n' \
                "$test_name" >&2
            audit_failed=1
            return
        fi
    done
    printf 'receiptless_bytecode_ffi=raw_compiler_reject+vm_whole_program_preflight+old_loader_removed\n'
}

retired_receiptless_bytecode_ffi_path

# R6-1112: the former AST-backed Interpreter FFI runtime and callback stack
# have no production or test consumer after the raw-AST bytecode loader was
# retired. Keep the shared host-library policy because Canonical MIR still
# uses it, and fail if any compatibility runtime/module is reintroduced.
retired_ast_ffi_compatibility_runtime() {
    local interpreter="$SRC_DIR/interp/mod.rs"
    local mir_runtime="$SRC_DIR/interp/bytecode/mir_ffi.rs"
    local legacy_ffi_files
    legacy_ffi_files="$(find "$SRC_DIR/interp/ffi" -type f -print -quit 2>/dev/null || true)"
    if [ -e "$SRC_DIR/interp/ffi_runtime.rs" ] || [ -n "$legacy_ffi_files" ]; then
        printf 'owner_audit_error=retired_ast_ffi_runtime_or_callback_tree_reintroduced\n' >&2
        audit_failed=1
        return
    fi
    if rg -n 'pub\(crate\) mod ffi;|pub\(crate\) mod ffi_runtime;|ffi_runtime:|FfiClosureRunner|call_extern_with_runner_ptr' \
        "$interpreter" >/dev/null; then
        printf 'owner_audit_error=checked_interpreter_ffi_owner_reintroduced\n' >&2
        audit_failed=1
        return
    fi
    if rg -n '\bFfiRuntime\b|FfiClosureRunner|ffi_runtime::|super::ffi::' "$SRC_DIR/interp" >/dev/null; then
        printf 'owner_audit_error=ast_ffi_compatibility_symbol_reintroduced\n' >&2
        audit_failed=1
        return
    fi
    if ! rg -F 'crate::interp::ffi_system_libraries' "$mir_runtime" >/dev/null; then
        printf 'owner_audit_error=canonical_mir_ffi_lost_shared_host_binding_policy\n' >&2
        audit_failed=1
        return
    fi
    printf 'ast_ffi_compatibility=retired+canonical_mir_runtime_only\n'
}

retired_ast_ffi_compatibility_runtime

# R6-1113: this CheckedProgram directory viewer has no production callers;
# unit tests retain it as a test adapter, while runtime execution stays in the
# AST-free bytecode/MIR consumers. Keep the legacy facade out of library builds.
retired_checked_directory_viewer_from_production() {
    local interpreter="$SRC_DIR/interp/mod.rs"
    if ! rg -U -q '#\[cfg\(test\)\]\npub struct Interpreter' "$interpreter" ||
        ! rg -U -q '#\[cfg\(test\)\]\nimpl Interpreter' "$interpreter"; then
        printf 'owner_audit_error=checked_directory_viewer_not_test_only\n' >&2
        audit_failed=1
        return
    fi
    if rg -n 'Interpreter::from_checked|interp::Interpreter' "$SRC_DIR" \
        --glob '!**/tests.rs' \
        --glob '!**/test.rs' \
        --glob '!src/tests/**' >/dev/null; then
        printf 'owner_audit_error=checked_directory_viewer_has_production_callsite\n' >&2
        audit_failed=1
        return
    fi
    printf 'checked_directory_viewer=test-only+no-production-consumers\n'
}

retired_checked_directory_viewer_from_production

route_receipt_profile_binding \
    src/main/canonical_dispatch.rs \
    'fn select_scalar_ffi_route(' \
    'fn reject_migrated_candidates(' \
    ScalarFfi \
    scalar-ffi-v1

# Keep the consumer-specific receipt labels anchored to the entry points that
# consume the same canonical graph. The labels may differ by consumer, but a
# missing label means that a direct API could silently stop binding its proof
# or ABI snapshot to the route it executed.
consumer_receipt_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local receipt_label="$4"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    local receipt_pattern="route_receipt(\"${receipt_label}\")"
    if [[ "$receipt_label" == "native-direct-v1" ]]; then
        receipt_pattern="MIR_NATIVE_DIRECT_ROUTE_PROFILE"
    elif [[ "$receipt_label" == "verify-ffi-v1" ]]; then
        # The admitted ScalarFfi route delegates execution and receipt binding
        # to ffi_compat::verify_scalar_route; keep both the mod.rs hand-off and
        # the ffi_compat.rs receipt lookup + program validation pinned together.
        local compat_context
        compat_context="$(sed -n '/^pub(crate) fn verify_scalar_route(/,/^pub(crate) fn verify_literal_contracts(/p' "$ROOT_DIR/src/verifier/ffi_compat.rs")"
        if ! printf '%s\n' "$context" | rg -F 'ffi_compat::verify_scalar_route(program, &canonical, source_hash)' >/dev/null \
            || ! printf '%s\n' "$compat_context" | rg -F 'mir.route_receipt("verify-ffi-v1")' >/dev/null \
            || ! printf '%s\n' "$compat_context" | rg -F 'receipt.validate_against_program(mir)' >/dev/null; then
            printf 'owner_audit_error=consumer_receipt_missing=%s::%s label=%s\n' \
                "$source_file" "$start_function" "$receipt_label" >&2
            audit_failed=1
            return
        fi
        printf 'consumer_receipt_binding=%s consumer=%s::%s\n' \
            "$receipt_label" "$source_file" "$start_function"
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$receipt_pattern" >/dev/null; then
        printf 'owner_audit_error=consumer_receipt_missing=%s::%s label=%s\n' \
            "$source_file" "$start_function" "$receipt_label" >&2
        audit_failed=1
        return
    fi
    printf 'consumer_receipt_binding=%s consumer=%s::%s\n' \
        "$receipt_label" "$source_file" "$start_function"
}

consumer_receipt_binding \
    src/codegen/compile.rs \
    'pub fn compile_checked(' \
    'fn try_compile_exact_migrated_mir_island(' \
    native-direct-v1
consumer_receipt_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_checked_with_source_hash(' \
    'pub fn is_z3_available(' \
    verify-ffi-v1

# Bind each consumer receipt to the canonical graph and, where applicable, to
# the caller's source provenance.  A label-only check could survive an
# accidental call that manufactures a receipt but drops it before invoking
# the consumer; checking the exact adapter call keeps the immutable MIR
# identity and source hash on the same path.
consumer_receipt_provenance_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local receipt_label="$4"
    local invocation_pattern="$5"
    local provenance_kind="$6"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$invocation_pattern" >/dev/null; then
        printf 'owner_audit_error=consumer_receipt_invocation_missing=%s::%s label=%s\n' \
            "$source_file" "$start_function" "$receipt_label" >&2
        audit_failed=1
        return
    fi
    if [[ "$receipt_label" == "verify-ffi-v1" ]]; then
        local compat_context
        compat_context="$(sed -n '/^pub(crate) fn verify_scalar_route(/,/^pub(crate) fn verify_literal_contracts(/p' "$ROOT_DIR/src/verifier/ffi_compat.rs")"
        if ! printf '%s\n' "$compat_context" | rg -F 'super::verify_ffi_mir_results_with_route_receipt(mir, source_hash.clone(), &receipt)' >/dev/null \
            || ! printf '%s\n' "$compat_context" | rg -F 'super::validate_mir_result_provenance(' >/dev/null; then
            printf 'owner_audit_error=consumer_receipt_invocation_missing=%s::%s label=%s\n' \
                "$source_file" "$start_function" "$receipt_label" >&2
            audit_failed=1
            return
        fi
    fi
    printf 'consumer_receipt_provenance_binding=%s graph=canonical provenance=%s consumer=%s::%s\n' \
        "$receipt_label" "$provenance_kind" "$source_file" "$start_function"
}

consumer_receipt_provenance_binding \
    src/codegen/compile.rs \
    'pub fn compile_checked(' \
    'fn try_compile_exact_migrated_mir_island(' \
    native-direct-v1 \
    'self.compile_mir_native_with_route_receipt(&canonical, &receipt)' \
    canonical-mir-graph
consumer_receipt_provenance_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_checked_with_source_hash(' \
    'pub fn is_z3_available(' \
    verify-ffi-v1 \
    'ffi_compat::verify_scalar_route(program, &canonical, source_hash)' \
    canonical-mir-graph+source-hash

# The direct native entry performs bytecode and verifier preflight before LLVM
# emission. Keep all three consumers on the same native-direct receipt so a
# preflight cannot silently manufacture a different route identity and then
# hand native emission a separate witness.
native_direct_preflight_receipt_binding() {
    local context
    context="$(sed -n '/fn try_compile_exact_migrated_mir_island(/,/^    fn mir_gate_diagnostics(/p' "$ROOT_DIR/src/codegen/compile.rs")"
    for pattern in \
        'MIR_NATIVE_DIRECT_ROUTE_PROFILE'; do
        if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=native_direct_preflight_receipt_binding_missing=src/codegen/compile.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    # `canonical` is already `&MirProgram` in this preflight, so Rust accepts
    # either `canonical` or the redundant `&canonical` at these call sites.
    # Keep the same MIR variable and receipt required; tolerate only that
    # source-level spelling difference so needless-borrow cleanup does not
    # break the semantic route-receipt tripwire.
    for pattern in \
        'compile_mir_program_with_route_receipt\(&?canonical, &receipt\)' \
        'verify_mir_with_route_receipt\(&?canonical, &receipt, String::new\(\)\)'; do
        if ! printf '%s\n' "$context" | rg "$pattern" >/dev/null; then
            printf 'owner_audit_error=native_direct_preflight_receipt_binding_missing=src/codegen/compile.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'native_direct_preflight_receipt_binding=native-direct-v1->bytecode+verifier+native consumer=src/codegen/compile.rs::try_compile_exact_migrated_mir_island(\n'
}

native_direct_preflight_receipt_binding

# The receipt-bearing verifier adapter must invoke the MIR verifier exactly
# once and pass the caller's witness into the semantic verifier directly.
# Calling the default wrapper here would manufacture a temporary verifier
# profile before the supplied receipt reached proof-artifact construction.
verifier_route_receipt_handoff() {
    local context
    context="$(sed -n '/pub fn verify_mir_with_route_receipt(/,/^fn validate_mir_result_provenance(/p' "$ROOT_DIR/src/verifier/mod.rs")"
    for pattern in \
        'verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?' \
        'validate_mir_result_provenance('; do
        if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=verifier_route_receipt_handoff_missing=src/verifier/mod.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    if printf '%s\n' "$context" | rg -F 'verify_mir(program, source_hash.clone())?' >/dev/null; then
        printf 'owner_audit_error=verifier_route_receipt_handoff_reenters_default=src/verifier/mod.rs\n' >&2
        audit_failed=1
        return
    fi
    printf 'verifier_route_receipt_handoff=direct-receipt-semantic-run consumer=src/verifier/mod.rs::pub fn verify_mir_with_route_receipt(\n'
}

verifier_route_receipt_handoff

# The FFI receipt-bearing verifier follows the same single-execution rule as
# the general MIR adapter.  It must pass the caller's witness into the
# semantic verifier so no temporary default receipt is created and replaced.
verifier_ffi_route_receipt_handoff() {
    local public_context profile_context
    public_context="$(sed -n '/^pub fn verify_ffi_mir_with_route_receipt(/,/^pub fn verify_ffi_mir_requires_with_route_receipt(/p' "$ROOT_DIR/src/verifier/mod.rs")"
    profile_context="$(sed -n '/^fn verify_ffi_mir_with_route_receipt_profile(/,/^pub fn verify_ffi_mir_with_route_manifest(/p' "$ROOT_DIR/src/verifier/mod.rs")"
    if ! printf '%s\n' "$public_context" | rg -F 'verify_ffi_mir_with_route_receipt_profile(program, receipt, source_hash, true)' >/dev/null; then
        printf 'owner_audit_error=verifier_ffi_route_receipt_profile_forward_missing=src/verifier/mod.rs\n' >&2
        audit_failed=1
        return
    fi
    for pattern in \
        '.validate_against_program(program)' \
        'validate_ffi_verifier_mir_capabilities(program)' \
        'mir::verify_ffi_program_with_route_receipt(program, source_hash.clone(), receipt)?' \
        'validate_mir_result_provenance('; do
        if ! printf '%s\n' "$profile_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=verifier_ffi_route_receipt_profile_missing=src/verifier/mod.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'verifier_ffi_route_receipt_handoff=public-api->profile-helper receipt+capability+proof+provenance consumer=src/verifier/mod.rs\n'
}

verifier_ffi_route_receipt_handoff

# The general verifier's default source-hash path must reuse the one receipt
# materialized for the function-contract pass when it appends FFI obligations.
# This keeps a mixed function/FFI result batch on one semantic witness even
# when no caller supplied an explicit route profile.
verifier_default_receipt_reuse() {
    local context
    context="$(sed -n '/^fn verify_program_inner(/,/^fn verify_ffi_program_inner(/p' "$ROOT_DIR/src/verifier/mir.rs")"
    for pattern in \
        'results.extend(verify_ffi_program_inner(' \
        'route_receipt,'; do
        if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=verifier_default_receipt_reuse_missing=src/verifier/mir.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'verifier_default_receipt_reuse=single-batch-witness consumer=src/verifier/mir.rs::fn verify_program_inner(\n'
}

verifier_default_receipt_reuse

# Both semantic bodies require the immutable receipt supplied by their adapter.
# A test-only convenience wrapper may materialize the default profile, but the
# inner proof path must never accept an optional receipt or rebuild one after
# entering the semantic pass.
verifier_semantic_receipt_required() {
    local source="$ROOT_DIR/src/verifier/mir.rs"
    if rg -n 'supplied_route_receipt: Option<|unwrap_or_else\(\|\| program\.route_receipt\("verifier-mir-v1"\)\)' "$source" >/dev/null; then
        printf 'owner_audit_error=verifier_semantic_receipt_optional_or_rebuilt=src/verifier/mir.rs\n' >&2
        audit_failed=1
        return
    fi
    if [ "$(rg -F -c 'route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,' "$source")" -lt 2 ]; then
        printf 'owner_audit_error=verifier_semantic_receipt_reference_missing=src/verifier/mir.rs\n' >&2
        audit_failed=1
        return
    fi
    if ! rg -F 'verify_program_with_route_receipt(program, source_hash, &route_receipt)' "$source" >/dev/null; then
        printf 'owner_audit_error=verifier_test_wrapper_receipt_handoff_missing=src/verifier/mir.rs\n' >&2
        audit_failed=1
        return
    fi
    printf 'verifier_semantic_receipt_required=explicit-reference consumer=src/verifier/mir.rs::verify_program_inner+verify_ffi_program_inner(\n'
}

verifier_semantic_receipt_required

# Public source-hash verifier adapters must derive one receipt before entering
# their receipt-bearing APIs, then reuse that same witness for semantic
# verification and provenance validation. The general MIR wrapper selects
# each exact profile before broader owner-v2 MIR and the default profile.
# Map Empty Root Queries V1, ZeroMissI32, and Observer V1 precede the other
# narrow owner-v2 selectors; preserve that source order exactly. FFI-only MIR
# uses its dedicated scalar verifier profile.
verifier_default_entry_receipt_handoff() {
    local mir_context ffi_context
    mir_context="$(sed -n '/^pub fn verify_mir(/,/^fn verify_mir_results_with_route_receipt(/p' "$ROOT_DIR/src/verifier/mod.rs")"
    ffi_context="$(sed -n '/^pub fn verify_ffi_mir_with_source_hash(/,/^fn verify_ffi_mir_results_with_route_receipt(/p' "$ROOT_DIR/src/verifier/mod.rs")"
    # Keep the complete ordered selector in one source-shape assertion. Each
    # narrow route needs its own exact verifier profile before the broad
    # owner-v2 fallback; omitting, reordering, or pairing a validator with a
    # different profile must fail closed.
    local profile_pattern='let profile =\s*if crate::core::mir::validate_map_empty_root_root_queries_v1_mir_route\(program\)\s*\.is_ok\(\)\s*\{\s*crate::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE\s*}'\
'\s*else if crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_mir_route\(\s*program,\s*\)\s*\.is_ok\(\)\s*\{\s*crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE\s*}'\
'\s*else if crate::core::mir::validate_ordinary_map_owner_lifecycle_observer_v1_mir_route\(program\)\s*\.is_ok\(\)\s*\{\s*crate::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE\s*}'\
'\s*else if crate::core::mir::validate_ordinary_map_owned_any_v2_direct_print_mir_route\(program\)\s*\.is_ok\(\)\s*\{\s*crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE\s*}'\
'\s*else if crate::core::mir::validate_ordinary_map_owned_any_v2_computed_key_get_mir_route\(program\)\s*\.is_ok\(\)\s*\{\s*crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE\s*}'\
'\s*else if crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_mir_route\(\s*program,\s*\)\s*\.is_ok\(\)\s*\{\s*crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE\s*}'\
'\s*else if crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_mir_route\(\s*program,\s*\)\s*\.is_ok\(\)\s*\{\s*crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE\s*}'\
'\s*else if crate::core::mir::validate_ordinary_map_owned_any_v2_values_snapshot_replacement_mir_route\(program\)\s*\.is_ok\(\)\s*\{\s*crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE\s*}'\
'\s*else if crate::core::mir::validate_ordinary_map_owned_any_v2_values_field_move_mir_route\(program\)\s*\.is_ok\(\)\s*\{\s*crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE\s*}'\
'\s*else if mir_program_has_ordinary_map_owned_any_v2_actions\(program\)\s*\{\s*crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND\s*}'\
'\s*else\s*\{\s*"verifier-mir-v1"\s*\};'
    local selector_call_count
    local profile_assignment_count
    selector_call_count="$(printf '%s\n' "$mir_context" | { rg -o 'validate_map_empty_root_root_queries_v1_mir_route|validate_ordinary_map_owned_any_v2_zero_miss_i32_mir_route|validate_ordinary_map_owner_lifecycle_observer_v1_mir_route|validate_ordinary_map_owned_any_v2_(direct_print|computed_key_get|values_mixed_snapshot_remove|values_mixed_snapshot|values_snapshot_replacement|values_field_move)_mir_route|mir_program_has_ordinary_map_owned_any_v2_actions' || true; } | wc -l | tr -d '[:space:]')"
    profile_assignment_count="$(printf '%s\n' "$mir_context" | { rg -o 'let profile\s*=' || true; } | wc -l | tr -d '[:space:]')"
    if [ "$selector_call_count" -ne 10 ] || [ "$profile_assignment_count" -ne 1 ] \
        || ! printf '%s\n' "$mir_context" | rg -U "$profile_pattern" >/dev/null; then
        printf 'owner_audit_error=verifier_default_entry_profile_selector_missing=src/verifier/mod.rs::verify_mir expected=one-complete-ordered-selector-with-ten-branches\n' >&2
        audit_failed=1
        return
    fi
    for pattern in \
        'let receipt = program.route_receipt(profile);' \
        'verify_mir_with_route_receipt(program, &receipt, source_hash)'; do
        if ! printf '%s\n' "$mir_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=verifier_default_entry_receipt_handoff_missing=src/verifier/mod.rs::verify_mir pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'verifier_default_entry_receipt_profile_order=map-empty-root-root-queries-v1->zero-miss-i32->observer-v1->direct-print->computed-key-get->mixed-snapshot-remove->mixed-snapshot->snapshot-replacement->field-move->ordinary-map-owner-v2->default consumer=src/verifier/mod.rs::pub fn verify_mir(\n'
    for pattern in \
        'let receipt = program.route_receipt("verifier-mir-v1");' \
        'verify_ffi_mir_results_with_route_receipt(program, source_hash.clone(), &receipt)?' \
        'validate_mir_result_provenance('; do
        if ! printf '%s\n' "$ffi_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=verifier_default_entry_receipt_handoff_missing=src/verifier/mod.rs::verify_ffi_mir_with_source_hash pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'verifier_default_entry_receipt_handoff=single-wrapper-witness consumer=src/verifier/mod.rs::verify_mir+verify_ffi_mir_with_source_hash(\n'
}

verifier_default_entry_receipt_handoff

# The unbound semantic helper remains available only to MIR unit fixtures.
# Production verifier adapters must use the receipt-bearing helper above so a
# future caller cannot silently reintroduce an unanchored proof path.
verifier_unbound_helper_test_only() {
    local context
    context="$(sed -n '/^#\[cfg(test)\]/,/^fn verify_program_inner(/p' "$ROOT_DIR/src/verifier/mir.rs")"
    for pattern in \
        'pub(crate) fn verify_program(' \
        'let route_receipt = program.route_receipt("verifier-mir-v1");' \
        'verify_program_with_route_receipt(program, source_hash, &route_receipt)'; do
        if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=verifier_unbound_helper_test_only_missing=src/verifier/mir.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'verifier_unbound_helper=test-only consumer=src/verifier/mir.rs::pub(crate) fn verify_program(\n'
}

verifier_unbound_helper_test_only

# Keep the FFI verifier's receipt tied to the same canonical route profile that
# admitted the island.  A source-hash forwarding check alone would still allow
# a copied receipt to be paired with a different profile; requiring the profile
# expression in the consumer context makes that drift fail closed.
consumer_receipt_profile_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local receipt_label="$4"
    local profile_name="$5"
    local context compat_context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    compat_context="$(sed -n '/^pub(crate) fn verify_scalar_route(/,/^pub(crate) fn verify_literal_contracts(/p' "$ROOT_DIR/src/verifier/ffi_compat.rs")"
    if ! printf '%s\n' "$context" | rg -F "CanonicalMirRouteProfile::${profile_name}" >/dev/null; then
        printf 'owner_audit_error=consumer_receipt_profile_missing=%s::%s label=%s profile=%s\n' \
            "$source_file" "$start_function" "$receipt_label" "$profile_name" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F 'profile.is_admitted(admission)' >/dev/null \
        || ! printf '%s\n' "$context" | rg -F 'ffi_compat::verify_scalar_route(program, &canonical, source_hash)' >/dev/null \
        || ! printf '%s\n' "$compat_context" | rg -F "mir.route_receipt(\"${receipt_label}\")" >/dev/null; then
        printf 'owner_audit_error=consumer_receipt_profile_mapping_missing=%s::%s expected=verify-ffi-v1-on-admitted-scalar-ffi\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$compat_context" | rg -F 'receipt.validate_against_program(mir)' >/dev/null; then
        printf 'owner_audit_error=consumer_receipt_profile_mapping_not_applied=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'consumer_receipt_profile_binding=%s profile=CanonicalMirRouteProfile::%s consumer=%s::%s\n' \
        "$receipt_label" "$profile_name" "$source_file" "$start_function"
    printf 'consumer_receipt_profile_mapping=verify-ffi-v1 after ScalarFfi admission consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

consumer_receipt_profile_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_checked_with_source_hash(' \
    'pub fn is_z3_available(' \
    verify-ffi-v1 \
    ScalarFfi

# The verifier-only singleton-For exception is a separate owner receipt and
# cannot acquire ScalarFfi execution authority. Ready Ensures are eligible
# only after the independent checked-source census and phase-aware call-site
# identity join; the MIR consumer still keeps foreign Requires independent.
owner_slice_ffi_receipt_binding() {
    local context compat_context projection_context source_identity_context
    context="$(sed -n '/^fn verify_isolated_ffi_callers(/,/^fn order_ffi_contract_sites_by_identity(/p' "$ROOT_DIR/src/verifier/mod.rs")"
    compat_context="$ROOT_DIR/src/verifier/ffi_compat.rs"
    projection_context="$(sed -n '/^impl FfiVerifierOnlyProjection {/,/^fn checked_body_calls(/p' "$compat_context")"
    source_identity_context="$(sed -n '/^impl CheckerFfiSourceIdentity {/,/^impl FfiVerifierOnlyProjection {/p' "$compat_context")"
    for pattern in \
        'ffi_compat::FfiVerifierOnlyProjection::from_checked_caller(' \
        'projection.verify(program, source_hash.clone(), &selected_ensures)' \
        'ResolvedFfiContractRoot::Ready { node_id }' \
        'contract.ensures_root.as_ref() == Some(node_id)' \
        'selected_ensures.insert(instruction.clone())' \
        'order_ffi_contract_sites_by_identity(' \
        'modeled_ensures_sites.extend(selected_ensures_sites)'; do
        if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=owner_slice_ffi_receipt_binding_missing=src/verifier/mod.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    if ! rg -Fq 'const MIR_FFI_OWNER_SLICE_ROUTE_PROFILE: &str = "verifier-ffi-owner-slice-v1";' "$compat_context"; then
        printf 'owner_audit_error=owner_slice_ffi_receipt_binding_missing=src/verifier/ffi_compat.rs pattern=%s\n' \
            'MIR_FFI_OWNER_SLICE_ROUTE_PROFILE="verifier-ffi-owner-slice-v1"' >&2
        audit_failed=1
        return
    fi
    for pattern in \
        'mir.route_receipt(MIR_FFI_OWNER_SLICE_ROUTE_PROFILE)' \
        'receipt.validate_against_program(&mir)' \
        'checker_source.validate_mir(&mir)' \
        'super::verify_ffi_mir_with_selected_ensures_route_receipt('; do
        if ! printf '%s\n' "$projection_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=owner_slice_ffi_receipt_binding_missing=src/verifier/ffi_compat.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    if ! printf '%s\n' "$projection_context" | rg -U 'self\.receipt\s*\.validate_against_program\(&self\.mir\)' >/dev/null; then
        printf 'owner_audit_error=owner_slice_ffi_receipt_binding_missing=src/verifier/ffi_compat.rs pattern=%s\n' \
            'self.receipt.validate_against_program(&self.mir)' >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$source_identity_context" | rg -F 'mir.validate_ffi_source_census()' >/dev/null; then
        printf 'owner_audit_error=owner_slice_ffi_receipt_binding_missing=src/verifier/ffi_compat.rs pattern=%s\n' \
            'mir.validate_ffi_source_census()' >&2
        audit_failed=1
        return
    fi
    printf 'owner_slice_ffi_receipt_binding=verifier-ffi-owner-slice-v1+Requires+selected-Ready-Ensures+phase-identity+checked-source-census consumer=src/verifier/mod.rs::verify_isolated_ffi_callers(\n'
}

owner_slice_ffi_receipt_binding

dual_ffi_string_record_owner_slice_binding() {
    local dual_context shared_owner_context owner_context
    dual_context="$(sed -n '/^pub fn verify_checked_dual(/,/^fn verify_deferred_dual_mir(/p' \
        "$ROOT_DIR/src/verifier/mod.rs")"
    for pattern in \
        'verify_dual_isolated_ffi_string_record_owner_slice(program, &source_hash)' \
        'verify_dual_isolated_ffi_mir_route(program, &source_hash)'; do
        if ! printf '%s\n' "$dual_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=dual_ffi_string_record_route_missing=src/verifier/mod.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    shared_owner_context="$(sed -n '/^fn isolated_ffi_string_record_owner_slice_callsite(/,/^\/\/\/ Whether the checked program has the exact verifier-only/p' \
        "$ROOT_DIR/src/verifier/mod.rs")"
    for pattern in \
        'contract_declarations.len() != 1' \
        'caller_contract_roots.len() != 1' \
        'ResolvedFfiParameterMode::Value' \
        'is_canonical_ffi_verifier_string_record(&signature.result)'; do
        if ! printf '%s\n' "$shared_owner_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=dual_ffi_string_record_shared_guard_missing=src/verifier/mod.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    owner_context="$(sed -n '/^fn verify_dual_isolated_ffi_string_record_owner_slice(/,/^fn verify_dual_isolated_ffi_mir_route(/p' \
        "$ROOT_DIR/src/verifier/mod.rs")"
    for pattern in \
        'isolated_ffi_string_record_owner_slice_callsite(program)' \
        'verify_ffi_checked_with_source_hash(program, source_hash.to_owned())' \
        'verifier-ffi-owner-slice-v1'; do
        if ! printf '%s\n' "$owner_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=dual_ffi_string_record_owner_guard_missing=src/verifier/mod.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'dual_ffi_owner_slice_binding=single-top-level-repr-c-string-record+single-caller-requires+exact-checker-callsite+verifier-ffi-owner-slice-v1 consumer=src/verifier/mod.rs::verify_checked_dual\n'
}

dual_ffi_string_record_owner_slice_binding

# The AST-free bytecode adapter must retain the route receipt that admitted
# the MIR graph.  Without this hand-off the compile-time receipt check would
# end at emission and a reusable VM could no longer prove that its binding
# snapshot belongs to one canonical route identity.
bytecode_route_receipt_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local pattern="$4"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
        printf 'owner_audit_error=bytecode_route_receipt_binding_missing=%s::%s pattern=%s\n' \
            "$source_file" "$start_function" "$pattern" >&2
        audit_failed=1
        return
    fi
    printf 'bytecode_route_receipt_binding=%s consumer=%s::%s\n' \
        "$pattern" "$source_file" "$start_function"
}

bytecode_direct_print_route_adapter_binding() {
    local source="$ROOT_DIR/src/interp/bytecode/mir.rs"
    local adapter_context token_context inner_context
    local direct_print_profile_pattern direct_print_validator_pattern
    local direct_print_token_pattern receipt_validation_pattern inner_handoff_pattern
    local inner_call_count
    adapter_context="$(sed -n '/^pub fn compile_mir_program_with_route_receipt(/,/^pub fn compile_mir_program_with_ordinary_map_owned_any_v2_direct_print_result_route(/p' "$source")"
    token_context="$(sed -n '/^enum ValidatedOrdinaryMapOwnedAnyV2Route/,/^fn compile_mir_program_inner(/p' "$source")"
    inner_context="$(sed -n '/^fn compile_mir_program_inner(/,/^fn materialize_canonical_ffi_bindings(/p' "$source")"
    direct_print_profile_pattern='receipt\.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE'
    direct_print_validator_pattern='validate_ordinary_map_owned_any_v2_direct_print_mir_route\(program\)'
    direct_print_token_pattern='Some\(ValidatedOrdinaryMapOwnedAnyV2Route::DirectPrint\(receipt\)\)'
    receipt_validation_pattern='receipt\.validate_against_program\(program\)'
    # Profiles may be selected in one typed narrow-route dispatcher. Verify
    # the exact DirectPrint validator and token still flow, in source order,
    # through that dispatcher into the one inner compiler handoff. This keeps
    # the audit semantic enough to survive local variable renames or adding a
    # sibling narrow profile, without accepting a receipt-only call path.
    inner_handoff_pattern='compile_mir_program_inner\(program,\s*Some\(receipt\),\s*[A-Za-z_][A-Za-z_0-9]*\)'
    inner_call_count="$(printf '%s\n' "$adapter_context" | { rg -o -F 'compile_mir_program_inner(' || true; } | wc -l | tr -d '[:space:]')"
    local profile_line validator_line token_line receipt_line handoff_line
    profile_line="$(printf '%s\n' "$adapter_context" | rg -n -U "$direct_print_profile_pattern" | awk -F: 'NR == 1 { first = $1 } END { if (first != "") print first }' || true)"
    validator_line="$(printf '%s\n' "$adapter_context" | rg -n -U "$direct_print_validator_pattern" | awk -F: 'NR == 1 { first = $1 } END { if (first != "") print first }' || true)"
    token_line="$(printf '%s\n' "$adapter_context" | rg -n -U "$direct_print_token_pattern" | awk -F: 'NR == 1 { first = $1 } END { if (first != "") print first }' || true)"
    receipt_line="$(printf '%s\n' "$adapter_context" | rg -n -U "$receipt_validation_pattern" | awk -F: 'NR == 1 { first = $1 } END { if (first != "") print first }' || true)"
    handoff_line="$(printf '%s\n' "$adapter_context" | rg -n -U "$inner_handoff_pattern" | awk -F: 'NR == 1 { first = $1 } END { if (first != "") print first }' || true)"
    if [ "$inner_call_count" -ne 1 ] \
        || [ -z "$profile_line" ] || [ -z "$validator_line" ] \
        || [ -z "$token_line" ] || [ -z "$receipt_line" ] || [ -z "$handoff_line" ] \
        || [ "$profile_line" -ge "$validator_line" ] \
        || [ "$validator_line" -ge "$token_line" ] \
        || [ "$token_line" -ge "$receipt_line" ] \
        || [ "$receipt_line" -ge "$handoff_line" ] \
        || ! printf '%s\n' "$token_context" | rg -U 'DirectPrint\(&\x27a CanonicalMirRouteReceipt\)' >/dev/null \
        || ! printf '%s\n' "$token_context" | rg -U 'Self::DirectPrint\(_\) =>\s*\{\s*crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE' >/dev/null \
        || ! printf '%s\n' "$inner_context" | rg -F 'ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE,' >/dev/null \
        || ! printf '%s\n' "$inner_context" | rg -F 'validated.receipt().profile != validated.expected_profile()' >/dev/null \
        || ! printf '%s\n' "$inner_context" | rg -F 'ValidatedOrdinaryMapOwnedAnyV2Route::DirectPrint(_)' >/dev/null \
        || ! printf '%s\n' "$inner_context" | rg -F 'validate_ordinary_map_owned_any_v2_direct_print_mir_route(program)' >/dev/null; then
        printf 'owner_audit_error=bytecode_direct_print_typed_route_adapter_missing=src/interp/bytecode/mir.rs::compile_mir_program_with_route_receipt expected=ordered-profile-validator-token-receipt-and-single-inner-handoff\n' >&2
        audit_failed=1
        return
    fi
    printf 'bytecode_direct_print_route_adapter_binding=ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE->validator+typed-token+receipt+single-inner-call consumer=src/interp/bytecode/mir.rs::compile_mir_program_with_route_receipt(\n'
}

bytecode_direct_print_route_adapter_binding

bytecode_direct_route_receipt_binding() {
    local context
    context="$(sed -n '/pub fn compile_mir_program(/,/^\/\/\/ Compile canonical MIR after checking the route receipt supplied by the/p' "$ROOT_DIR/src/interp/bytecode/mir.rs")"
    for pattern in \
        'pub fn compile_mir_program(' \
        'program.ffi_calls().is_empty()' \
        'MIR_BYTECODE_DIRECT_ROUTE_PROFILE' \
        'compile_mir_program_inner(program, receipt.as_ref(), None)'; do
        if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=bytecode_direct_route_receipt_binding_missing=src/interp/bytecode/mir.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'bytecode_direct_route_receipt_binding=ffi-call-site->MIR_BYTECODE_DIRECT_ROUTE_PROFILE->compile_mir_program_inner(program, receipt.as_ref(), None) consumer=src/interp/bytecode/mir.rs::pub fn compile_mir_program(\n'
}

bytecode_direct_route_receipt_binding

# Every narrow ordinary Map/Any bytecode entry accepts a CheckedProgram/MIR
# pair, validates its exact route, and passes the matching typed token as the
# third argument to the shared adapter. The token's expected-profile mapping
# and the inner identity check keep a receipt for one narrow profile from
# authorizing another.
bytecode_narrow_map_any_route_adapter() {
    local adapter="$1"
    local next_marker="$2"
    local validator="$3"
    local profile="$4"
    local variant="$5"
    local receipt_source="${6:-program}"
    local adapter_context
    local handoff_pattern
    local inner_call_count
    local validator_pattern
    local profile_receipt_pattern
    local token_profile_pattern
    local token_declaration_pattern
    local token_context
    adapter_context="$(awk -v signature="pub fn ${adapter}(" '
        index($0, signature) == 1 { in_adapter = 1; start_line = NR; found = 1 }
        in_adapter && NR > start_line && $0 ~ /^(pub[[:space:]]+)?fn[[:space:]]/ { exit }
        in_adapter { print }
        END { if (!found) exit 1 }
    ' "$ROOT_DIR/src/interp/bytecode/mir.rs")" || {
        printf 'owner_audit_error=bytecode_narrow_map_any_adapter_missing=src/interp/bytecode/mir.rs::%s pattern=pub-fn-signature\n' \
            "$adapter" >&2
        audit_failed=1
        return
    }
    token_context="$(sed -n '/^enum ValidatedOrdinaryMapOwnedAnyV2Route/,/^fn compile_mir_program_inner(/p' "$ROOT_DIR/src/interp/bytecode/mir.rs")"
    for pattern in \
        "pub fn ${adapter}(" \
        'compile_mir_program_inner('; do
        if ! printf '%s\n' "$adapter_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=bytecode_narrow_map_any_adapter_missing=src/interp/bytecode/mir.rs::%s pattern=%s\n' \
                "$adapter" "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    validator_pattern="crate::core::mir::${validator}\\(\\s*checked,\\s*program,?\\s*\\)"
    if ! printf '%s\n' "$adapter_context" | rg -U "$validator_pattern" >/dev/null; then
        printf 'owner_audit_error=bytecode_narrow_map_any_validator_missing=src/interp/bytecode/mir.rs::%s expected=%s(checked, program)\n' \
            "$adapter" "$validator" >&2
        audit_failed=1
        return
    fi
    if [ "$receipt_source" = "checker-validator" ]; then
        profile_receipt_pattern="let\\s+receipt\\s*=\\s*crate::core::mir::${validator}\\(\\s*checked,\\s*program,?\\s*\\)"
    elif [ "$receipt_source" = "checker-argument" ]; then
        profile_receipt_pattern="let\\s+expected\\s*=\\s*crate::core::mir::${validator}\\(\\s*checked,\\s*program,?\\s*\\)"
    else
        profile_receipt_pattern="program\\s*\\.route_receipt\\(\\s*crate::core::mir::${profile},?\\s*\\)"
    fi
    if ! printf '%s\n' "$adapter_context" | rg -U "$profile_receipt_pattern" >/dev/null; then
        printf 'owner_audit_error=bytecode_narrow_map_any_profile_receipt_missing=src/interp/bytecode/mir.rs::%s expected=%s\n' \
            "$adapter" "$profile" >&2
        audit_failed=1
        return
    fi
    if [ "$receipt_source" = "checker-validator" ] \
        && { ! printf '%s\n' "$adapter_context" | rg -U "if\\s+receipt\\.profile\\s*!=\\s*crate::core::mir::${profile}" >/dev/null \
            || ! printf '%s\n' "$adapter_context" | rg -U 'receipt\s*\.validate_against_program\(program\)' >/dev/null; }; then
        printf 'owner_audit_error=bytecode_narrow_map_any_validator_receipt_unbound=src/interp/bytecode/mir.rs::%s expected=exact-profile+program-validation\n' \
            "$adapter" >&2
        audit_failed=1
        return
    fi
    if [ "$receipt_source" = "checker-argument" ] \
        && { ! printf '%s\n' "$adapter_context" | rg -U 'if\s+expected\s*!=\s*\*receipt' >/dev/null \
            || ! printf '%s\n' "$adapter_context" | rg -U 'receipt\s*\.validate_against_program\(program\)' >/dev/null \
            || ! printf '%s\n' "$adapter_context" | rg -U 'let\s+replayed\s*=\s*receipt\.manifest_round_trip\(\)' >/dev/null \
            || ! printf '%s\n' "$adapter_context" | rg -U 'if\s+replayed\s*!=\s*\*receipt' >/dev/null; }; then
        printf 'owner_audit_error=bytecode_narrow_map_any_checker_argument_receipt_unbound=src/interp/bytecode/mir.rs::%s expected=exact-checker-receipt+MIR-binding+manifest-replay\n' \
            "$adapter" >&2
        audit_failed=1
        return
    fi
    if [ "$receipt_source" = "checker-argument" ]; then
        handoff_pattern="compile_mir_program_inner\\(\\s*program,\\s*Some\\(receipt\\),\\s*Some\\(ValidatedOrdinaryMapOwnedAnyV2Route::${variant}\\(\\s*receipt\\s*,?\\s*\\)\\)\\s*,?\\s*\\)"
    else
        handoff_pattern="compile_mir_program_inner\\(\\s*program,\\s*Some\\(&receipt\\),\\s*Some\\(ValidatedOrdinaryMapOwnedAnyV2Route::${variant}\\(\\s*&receipt\\s*,?\\s*\\)\\)\\s*,?\\s*\\)"
    fi
    inner_call_count="$(printf '%s\n' "$adapter_context" | rg -o -F 'compile_mir_program_inner(' | wc -l | tr -d '[:space:]')"
    if [ "$inner_call_count" -ne 1 ] || ! printf '%s\n' "$adapter_context" | rg -U "$handoff_pattern" >/dev/null; then
        printf 'owner_audit_error=bytecode_narrow_map_any_adapter_token_missing=src/interp/bytecode/mir.rs::%s expected=sole-compile_mir_program_inner-call-third-argument=ValidatedOrdinaryMapOwnedAnyV2Route::%s\n' \
            "$adapter" "$variant" >&2
        audit_failed=1
        return
    fi
    token_profile_pattern="Self::${variant}\\(_\\)\\s*=>\\s*\\{?\\s*crate::core::mir::${profile}\\s*\\}?"
    token_declaration_pattern="${variant}\\(&'a CanonicalMirRouteReceipt\\)"
    if ! printf '%s\n' "$token_context" | rg -F 'fn expected_profile(self)' >/dev/null \
        || ! printf '%s\n' "$token_context" | rg -U "$token_profile_pattern" >/dev/null \
        || ! printf '%s\n' "$token_context" | rg "$token_declaration_pattern" >/dev/null; then
        printf 'owner_audit_error=bytecode_narrow_map_any_token_profile_mismatch=src/interp/bytecode/mir.rs variant=%s expected=%s\n' \
            "$variant" "$profile" >&2
        audit_failed=1
        return
    fi
}

bytecode_narrow_map_any_route_binding() {
    local inner_context
    local mir_source="$ROOT_DIR/src/interp/bytecode/mir.rs"
    inner_context="$(sed -n '/^fn compile_mir_program_inner(/,/^fn materialize_canonical_ffi_bindings(/p' "$mir_source")"
    for pattern in \
        "validated_values_route: Option<ValidatedOrdinaryMapOwnedAnyV2Route<'_>>" \
        'let narrow_profiles = [' \
        'if let Some(validated) = validated_values_route' \
        'route_receipt != Some(validated.receipt())' \
        'validated.receipt().profile != validated.expected_profile()' \
        'else if route_receipt' \
        '.is_some_and(|receipt| {' \
        'receipt.profile != ORDINARY_MAP_OWNER_OBSERVER_V1_PROFILE' \
        'narrow_profiles.contains(&receipt.profile.as_str())' \
        '}) {'; do
        if ! printf '%s\n' "$inner_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=bytecode_inner_narrow_route_guard_missing=src/interp/bytecode/mir.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_values_field_move_route \
        pub\ fn\ compile_mir_program_with_values_snapshot_replacement_route\( \
        validate_ordinary_map_owned_any_v2_values_field_move_route \
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE \
        ValuesFieldMove
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_values_snapshot_replacement_route \
        pub\ fn\ compile_mir_program_with_values_mixed_snapshot_route\( \
        validate_ordinary_map_owned_any_v2_values_snapshot_replacement_route \
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE \
        ValuesSnapshotReplacement
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_values_mixed_snapshot_route \
        pub\ fn\ compile_mir_program_with_values_local_index_i32_route\( \
        validate_ordinary_map_owned_any_v2_values_mixed_snapshot_route \
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE \
        ValuesMixedSnapshot
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_values_mixed_snapshot_remove_route \
        pub\ fn\ compile_mir_program_with_values_producer_caller_route\( \
        validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route \
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE \
        ValuesMixedSnapshotRemove
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_values_producer_caller_route \
        pub\ fn\ compile_mir_program_with_values_crosscall_string_caller_route\( \
        validate_ordinary_map_owned_any_v2_values_producer_caller_route \
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_PRODUCER_CALLER_PROFILE \
        ValuesProducerCaller \
        checker-validator
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_values_crosscall_string_caller_route \
        pub\ fn\ compile_mir_program_with_values_crosscall_string_caller_index1_route\( \
        validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_route \
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_PROFILE \
        ValuesCrosscallStringCaller \
        checker-validator
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_values_crosscall_string_caller_index1_route \
        pub\ fn\ compile_mir_program_with_map_root_producer_caller_route\( \
        validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route \
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE \
        ValuesCrosscallStringCallerIndex1 \
        checker-validator
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_computed_key_get_route \
        pub\ fn\ compile_mir_program_with_computed_string_key_owner_chain_route\( \
        validate_ordinary_map_owned_any_v2_computed_key_get_route \
        ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE \
        ComputedKeyGet
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_computed_string_key_owner_chain_route \
        pub\ fn\ compile_mir_program_with_dynamic_keys_get_route\( \
        validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route \
        ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE \
        ComputedStringKeyOwnerChain
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_dynamic_keys_get_route \
        pub\ fn\ compile_mir_program_with_ordinary_map_owned_any_v2_zero_miss_i32_route\( \
        validate_ordinary_map_owned_any_v2_dynamic_keys_get_route \
        ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE \
        DynamicKeysGet
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_ordinary_map_owned_any_v2_zero_miss_i32_route \
        pub\ fn\ compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_i32_route\( \
        validate_ordinary_map_owned_any_v2_zero_miss_i32_route \
        ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE \
        ZeroMissI32
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_i32_route \
        pub\ fn\ compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_string_route\( \
        validate_ordinary_map_owned_any_v2_get_or_default_i32_route \
        ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_I32_PROFILE \
        GetOrDefaultI32 \
        checker-argument
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route \
        pub\ fn\ compile_mir_program_with_ordinary_map_owned_any_v2_is_empty_route\( \
        validate_ordinary_map_owned_any_v2_removed_key_checked_miss_route \
        ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE \
        RemovedKeyCheckedMiss
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_ordinary_map_owned_any_v2_is_empty_route \
        pub\ fn\ compile_mir_program_with_ordinary_map_owned_any_v2_record_unit_route\( \
        validate_ordinary_map_owned_any_v2_is_empty_route \
        ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE \
        IsEmpty
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_ordinary_map_owned_any_v2_record_unit_route \
        pub\ fn\ compile_mir_program_with_ordinary_map_owned_any_v2_map_set_drop_control_route\( \
        validate_ordinary_map_owned_any_v2_record_unit_route \
        ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE \
        RecordUnit
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_ordinary_map_owned_any_v2_map_set_drop_control_route \
        pub\ fn\ compile_mir_program_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route\( \
        validate_ordinary_map_owned_any_v2_map_set_drop_control_route \
        ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE \
        MapSetDropControl
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route \
        pub\ fn\ compile_mir_program_with_ordinary_map_owned_any_v2_clone_isolation_route\( \
        validate_ordinary_map_owned_any_v2_branch_local_set_diamond_route \
        ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE \
        BranchLocalSetDiamond
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_ordinary_map_owned_any_v2_clone_isolation_route \
        pub\ fn\ compile_mir_program_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route\( \
        validate_ordinary_map_owned_any_v2_clone_isolation_route \
        ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE \
        CloneIsolation
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route \
        pub\ fn\ compile_mir_program_with_empty_root_root_queries_v1_route\( \
        validate_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route \
        ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE \
        PersistentRootAliasReplacement
    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_empty_root_root_queries_v1_route \
        enum\ ValidatedOrdinaryMapOwnedAnyV2Route \
        validate_map_empty_root_root_queries_v1_route \
        MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE \
        EmptyRootRootQueriesV1

    # The two Result-branch routes share one materializer but carry distinct
    # profiles, typed route-token variants, and VM anchors. Check both public
    # adapters and the shared receipt validation path explicitly.
    local result_profile_context result_route_token_context wrapper_context
    result_profile_context="$(sed -n '/^fn validate_result_route_receipt(/,/^pub fn compile_mir_program_with_map_root_producer_caller_route(/p' "$mir_source")"
    result_route_token_context="$(sed -n '/fn expected_profile(self)/,/^    }/p' "$mir_source")"
    for spec in \
        'compile_mir_program_with_values_result_crosscall_string_caller_route|ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_PROFILE|false|ValuesResultCrosscallStringCaller' \
        'compile_mir_program_with_values_result_crosscall_string_caller_index1_route|ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_INDEX1_PROFILE|true|ValuesResultCrosscallStringCallerIndex1'; do
        IFS='|' read -r adapter profile index_flag variant <<< "$spec"
        wrapper_context="$(sed -n "/^pub fn ${adapter}(/,/^}$/p" "$mir_source")"
        if ! printf '%s\n' "$wrapper_context" | rg -F "crate::core::mir::${profile}" >/dev/null \
            || ! printf '%s\n' "$wrapper_context" | rg -F "$index_flag" >/dev/null; then
            printf 'owner_audit_error=bytecode_result_crosscall_wrapper_profile_missing adapter=%s profile=%s index=%s\n' \
                "$adapter" "$profile" "$index_flag" >&2
            audit_failed=1
        fi
        if ! printf '%s\n' "$result_profile_context" | rg -F 'validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(' >/dev/null \
            || ! printf '%s\n' "$result_profile_context" | rg -F 'validate_result_route_receipt(' >/dev/null \
            || ! printf '%s\n' "$result_profile_context" | rg -F 'if receipt.profile != expected_profile' >/dev/null \
            || ! printf '%s\n' "$result_profile_context" | rg -F '.validate_against_program(program)' >/dev/null \
            || ! printf '%s\n' "$result_profile_context" | rg -F 'receipt.manifest_round_trip()' >/dev/null \
            || ! printf '%s\n' "$result_profile_context" | rg -F "ValidatedOrdinaryMapOwnedAnyV2Route::${variant}(&receipt)" >/dev/null \
            || ! printf '%s\n' "$result_profile_context" | rg -U 'compile_mir_program_inner\(\s*program,\s*Some\(&receipt\),\s*Some\(route\)\s*\)' >/dev/null; then
            printf 'owner_audit_error=bytecode_result_crosscall_receipt_binding_missing adapter=%s variant=%s\n' \
                "$adapter" "$variant" >&2
            audit_failed=1
        fi
        case "$variant" in
            ValuesResultCrosscallStringCaller)
                token_pattern='Self::ValuesResultCrosscallStringCaller\(_\) => \{[[:space:]]*crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_PROFILE[[:space:]]*\}'
                token_declaration='ValuesResultCrosscallStringCaller(&'
                ;;
            ValuesResultCrosscallStringCallerIndex1)
                token_pattern='Self::ValuesResultCrosscallStringCallerIndex1\(_\) => \{[[:space:]]*crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_INDEX1_PROFILE[[:space:]]*\}'
                token_declaration='ValuesResultCrosscallStringCallerIndex1(&'
                ;;
        esac
        if ! printf '%s\n' "$result_route_token_context" | rg -U "$token_pattern" >/dev/null \
            || ! rg -Fq "$token_declaration" "$mir_source"; then
            printf 'owner_audit_error=bytecode_result_crosscall_route_token_profile_mismatch variant=%s profile=%s\n' \
                "$variant" "$profile" >&2
            audit_failed=1
        fi
    done
    printf 'bytecode_result_crosscall_route_binding=V3-index0+V4-index1->separate-profile+validated-Checker-MIR-receipt+typed-token+VM-anchor consumer=src/interp/bytecode/mir.rs+src/interp/bytecode/vm.rs\n'

    printf 'bytecode_ordinary_map_owned_any_v2_route_binding=21-dedicated-adapters->ValidatedOrdinaryMapOwnedAnyV2Route-third-argument+exact-profile-guard consumer=src/interp/bytecode/mir.rs::compile_mir_program_inner(\n'
    printf 'bytecode_record_unit_route_binding=ordinary-map-owned-any-v2-record-unit-v1->Checker-validated-adapter+RecordUnit-token+validated-narrow-route consumer=src/interp/bytecode/mir.rs\n'
    printf 'bytecode_map_empty_root_root_queries_v1_route_binding=1-dedicated-adapter->ValidatedOrdinaryMapOwnedAnyV2Route-third-argument+exact-profile-guard consumer=src/interp/bytecode/mir.rs::compile_mir_program_with_empty_root_root_queries_v1_route(\n'
}

bytecode_narrow_map_any_route_binding

# DynamicKeysGet projects an owned argv[0] String before touching the Map.
# Every public CLI consumer must select that same Checker-derived route and
# route its receipt through a dedicated adapter; neither generic adapter may
# authorize the projection opcode.
dynamic_keys_get_cli_receipt_consumers() {
    local run_source="$ROOT_DIR/src/main/run.rs"
    local build_source="$ROOT_DIR/src/main/build.rs"
    local verify_source="$ROOT_DIR/src/main/verify.rs"
    local test_source="$ROOT_DIR/src/main/test.rs"
    local disasm_source="$ROOT_DIR/src/main/disasm_cmd.rs"
    local bytecode_source="$ROOT_DIR/src/interp/bytecode/mir.rs"
    local native_source="$ROOT_DIR/src/codegen/mir/eligibility.rs"
    local verifier_source="$ROOT_DIR/src/verifier/mod.rs"
    local emitter_source="$ROOT_DIR/src/codegen/mir/ordinary_map_owned_any_v2.rs"
    local fixture="$ROOT_DIR/tests/fixtures/mir_ordinary_map_any_owner_v2_dynamic_keys_get.mimi"
    local integration_test="$ROOT_DIR/tests/canonical_mir_ordinary_map_any_v2_dynamic_keys_get.rs"
    local l3_test="$ROOT_DIR/tests/canonical_mir_ordinary_map_any_v2_dynamic_keys_get_memcheck_l3.rs"
    local runtime_test="$ROOT_DIR/src/runtime/ordinary_map_domain.rs"

    local -a consumer_requirements=(
        "$run_source|ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE|compile_mir_program_with_dynamic_keys_get_route"
        "$build_source|ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE|compile_checked_mir_native_with_dynamic_keys_get_route"
        "$verify_source|ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE|verify_mir_with_ordinary_map_owned_any_v2_dynamic_keys_get_route"
        "$test_source|ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE|compile_mir_program_with_dynamic_keys_get_route"
        "$disasm_source|ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE|compile_mir_program_with_dynamic_keys_get_route"
    )
    local requirement source profile_token adapter
    for requirement in "${consumer_requirements[@]}"; do
        IFS='|' read -r source profile_token adapter <<< "$requirement"
        if ! rg -F "$profile_token" "$source" >/dev/null \
            || ! rg -F "$adapter" "$source" >/dev/null; then
            printf 'owner_audit_error=dynamic_keys_get_cli_receipt_dispatch_missing=%s required=%s+%s\n' \
                "${source#"$ROOT_DIR/"}" "$profile_token" "$adapter" >&2
            audit_failed=1
            return
        fi
    done

    for pattern in \
        'ValidatedOrdinaryMapOwnedAnyV2Route::DynamicKeysGet' \
        'ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE' \
        'narrow V2 route receipt lacks its validated Checker/MIR token'; do
        if ! rg -F "$pattern" "$bytecode_source" >/dev/null; then
            printf 'owner_audit_error=dynamic_keys_get_bytecode_exact_token_missing=src/interp/bytecode/mir.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    for pattern in \
        'exact_dynamic_keys_get_receipt' \
        'dynamic_keys_get_authorized' \
        'with_dynamic_keys_get_authorization'; do
        if ! rg -F "$pattern" "$native_source" >/dev/null; then
            printf 'owner_audit_error=dynamic_keys_get_native_authorization_missing=src/codegen/mir/eligibility.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    for pattern in \
        'program_has_dynamic_keys_get' \
        'requires the exact CheckedProgram-aware verifier route' \
        'OrdinaryMapOwnedAnyV2Admission::DynamicKeysGet'; do
        if ! rg -F "$pattern" "$verifier_source" >/dev/null; then
            printf 'owner_audit_error=dynamic_keys_get_verifier_isolation_missing=src/verifier/mod.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done

    if ! rg -F 'args()[0]' "$fixture" >/dev/null \
        || ! rg -F '"\0雪"' "$fixture" >/dev/null \
        || ! rg -F 'EXPECTED_PREFIX_OUTPUT: &[u8] = b"pr\xc3\xa9\0\xe9\x9b\xaa\ntrue\n"' "$integration_test" >/dev/null \
        || ! rg -F 'dynamic_keys_get_rejects_generic_receipts_and_missing_arg_before_output' "$integration_test" >/dev/null \
        || ! rg -F 'dynamic_keys_get_default_and_explicit_cli_consumers_share_exact_route_receipts' "$integration_test" >/dev/null \
        || ! rg -F 'dynamic_keys_get_disasm_uses_the_exact_bytecode_adapter_and_test_cli_stays_nonexecuting' "$integration_test" >/dev/null \
        || ! rg -F 'MIMI_REQUIRE_VALGRIND' "$l3_test" >/dev/null \
        || ! rg -F 'source-manifest.before.sha256' "$l3_test" >/dev/null \
        || ! rg -F 'ordinary_map_owned_any_v2_dynamic_keys_get_default_and_explicit_are_memcheck_clean' "$l3_test" >/dev/null; then
        printf 'owner_audit_error=dynamic_keys_get_fixture_or_adversarial_test_missing\n' >&2
        audit_failed=1
        return
    fi

    local emitter_context
    emitter_context="$(sed -n '/pub(super) fn emit_ordinary_map_owned_any_v2_dynamic_arg0(/,/pub(super) fn emit_ordinary_map_owned_any_v2_values_len(/p' "$emitter_source")"
    if ! printf '%s\n' "$emitter_context" | rg -U 'emit_ordinary_map_domain_own_string\(arg, length, subject\)\?;[\s\S]*?build_string_struct\(arg, length\)' >/dev/null \
        || ! rg -F 'super::super::env::mimi_args_get(0)' "$runtime_test" >/dev/null \
        || ! rg -F 'ordinary_map_domain_own_string(dynamic, 11)' "$runtime_test" >/dev/null \
        || ! rg -F '"get-any-failure"' "$runtime_test" >/dev/null \
        || ! rg -F 'with_ordinary_map_owned_any_v2_get_any_publish_failure_for_test' "$runtime_test" >/dev/null; then
        printf 'owner_audit_error=dynamic_keys_get_native_argv_owner_cleanup_missing\n' >&2
        audit_failed=1
        return
    fi

    printf 'dynamic_keys_get_cli_receipt_dispatch=default+explicit-run/build/verify+test/disasm use dedicated Checker/MIR adapters; generic receipts remain denied\n'
    printf 'dynamic_keys_get_native_argv_ownership=argv-copy registered before String publication and MapNew; trap drain regression uses args_get allocation\n'
}

dynamic_keys_get_cli_receipt_consumers

# Pin the V1 observer profile from the main default selector through its
# checker/MIR receipt proof, bytecode anchor, and native build adapter. The
# standalone verify_mir selector remains a separate raw-MIR entry; the main
# route must pass its already-selected exact receipt to the verifier.
bytecode_observer_v1_route_receipt_binding() {
    local source="$ROOT_DIR/src/interp/bytecode/mir.rs"
    local route_source="$ROOT_DIR/src/core/mir/route.rs"
    local receipt_source="$ROOT_DIR/src/core/mir/receipt.rs"
    local dispatcher="$ROOT_DIR/src/main/canonical_dispatch.rs"
    local native_source="$ROOT_DIR/src/codegen/mir/eligibility.rs"
    local build_source="$ROOT_DIR/src/main/build.rs"
    local inner_context observer_nodes_context observer_anchor_context profile_context
    local main_profile_context main_profile_context_without_result main_route_context materialization_context preflight_context
    local native_context native_receipt_adapter checked_native_receipt_adapter build_route_context
    local profile_pattern observer_pattern anchor_pattern main_profile_pattern main_route_pattern
    inner_context="$(sed -n '/^fn compile_mir_program_inner(/,/^fn materialize_canonical_ffi_bindings(/p' "$source")"
    observer_nodes_context="$(printf '%s\n' "$inner_context" | sed -n '/let has_observer_mir_nodes =/,/let ordinary_map_owner_observer_v1_receipts =/p')"
    observer_anchor_context="$(printf '%s\n' "$inner_context" | sed -n '/if let (Some(route_receipt), Some(checker_receipts)) = (/,/observer_bindings,/p')"
    profile_context="$(sed -n '/^impl CanonicalMirRouteProfile {/,/^    \/\/\/ Return whether checker-owned/p' "$route_source")"
    main_profile_context="$(sed -n '/^pub(crate) fn route_receipt_profile_for_checked(/,/^}/p' "$dispatcher")"
    # Strip only the Result-branch prefix guards (and explanatory
    # leading comments inside that prefix) after validating each guard
    # independently below, leaving the established ordered selector contract intact.
    main_profile_context_without_result="$(printf '%s\n' "$main_profile_context" | awk '
        /^[[:space:]]*\/\// { next }
        /^[[:space:]]*if admission$/ {
            if (getline line2 <= 0) { print; exit }
            if (getline line3 <= 0) { print; print line2; exit }
            if (line2 ~ /\.ordinary_map_owned_any_v2$/ && line3 ~ /\.ordinary_map_owned_any_v2_values_local_index_i32_result_branch\(\)$/) {
                while (getline body > 0 && body !~ /^[[:space:]]*}[[:space:]]*$/) {}
                next
            }
            print
            print line2
            print line3
            next
        }
        /^[[:space:]]*if admission\.ordinary_map_owned_any_v2_values_result_(crosscall_string_caller_v4|crosscall_string_caller_v3|producer_caller)\(\) \{/ {
            while (getline body > 0 && body !~ /^[[:space:]]*}[[:space:]]*$/) {}
            next
        }
        /^[[:space:]]*if !admission\.ordinary_map_owned_any_v2\.is_direct_print_result\(\)$/ {
            buf = $0 "\n"
            matched = 0
            while (getline line > 0) {
                buf = buf line "\n"
                if (line ~ /\.any\(\|analysis\| analysis\.has_ordinary_map_owned_get_result_branch_v3_receipt\(\)\)$/) {
                    matched = 1
                }
                if (line ~ /^[[:space:]]*\{[[:space:]]*$/) {
                    break
                }
            }
            if (matched) {
                while (getline body > 0 && body !~ /^[[:space:]]*}[[:space:]]*$/) {}
                next
            }
            printf "%s", buf
            next
        }
        /if admission\.ordinary_map_owned_any_v2$/ {
            if (getline condition <= 0) exit
            if (condition ~ /OrdinaryMapOwnedAnyV2Admission::MapSetDropControlResult/) {
                while (getline body > 0 && body !~ /^[[:space:]]*}[[:space:]]*$/) {}
                next
            }
            print
            print condition
            next
        }
        { print }
    ')"
    main_route_context="$(sed -n '/^fn select_default_route_inner(/,/^fn select_scalar_actor_route(/p' "$dispatcher")"
    materialization_context="$(sed -n '/^fn ordinary_map_owner_lifecycle_observer_v1_materialization_is_exact(/,/^\/\/\/ Revalidate the Checker-derived Observer V1/p' "$dispatcher")"
    preflight_context="$(sed -n '/^fn preflight_ordinary_map_owner_lifecycle_observer_v1(/,/^fn select_ordinary_map_owner_lifecycle_observer_v1_route(/p' "$dispatcher")"
    native_context="$(sed -n '/^    fn compile_mir_native_inner(/,/^        let exact_checked_get_receipt/p' "$native_source")"
    native_receipt_adapter="$(sed -n '/^    pub fn compile_mir_native_with_route_receipt(/,/^    \/\/\/ Values field-move and snapshot-replacement/p' "$native_source")"
    checked_native_receipt_adapter="$(sed -n '/^    pub fn compile_checked_mir_native_with_route_receipt(/,/^    \/\/\/ Compile the exact Checker\/MIR computed-key/p' "$native_source")"
    # Count only the three compile-route branches. Later census formatting
    # independently asks the checked selector for a label while reading the
    # already-selected receipt; that telemetry must not be mistaken for a
    # fourth native route or included in this receipt hand-off contract.
    build_route_context="$(sed -n '/let (compile_result, canonical_default, route_receipt, route_consumer) = if mir {/,/^    let native_data_export_table_receipt =/p' "$build_source")"
    profile_pattern='const ORDINARY_MAP_OWNER_OBSERVER_V1_PROFILE:\s*&str\s*=\s*crate::core::mir::CanonicalMirRouteProfile::\s*OrdinaryMapOwnerLifecycleObserverV1\s*\.as_str\(\);'
    observer_pattern='let ordinary_map_owner_observer_v1_receipts = match route_receipt \{\s*Some\(receipt\) if receipt\.profile == ORDINARY_MAP_OWNER_OBSERVER_V1_PROFILE => \{[\s\S]*?receipt\.validate_against_program\(program\)[\s\S]*?validate_ordinary_map_owner_lifecycle_observer_v1_mir_route\(\s*program,?\s*\)[\s\S]*?Some\(receipts\)\s*\}\s*_ if has_observer_mir_nodes => \{[\s\S]*?instructions require the exact route receipt[\s\S]*?\}\s*_\s*=> None,\s*\};'
    anchor_pattern='bind_ordinary_map_owner_observer_v1_route_anchor\(\s*entry,\s*&functions,\s*&builtin_names,\s*route_receipt,\s*checker_receipts\.clone\(\),\s*observer_bindings,'
    main_profile_pattern='let admission\s*=\s*mimi::core::mir::classify_canonical_mir_route_admission\(checked\);\s*if admission\.ordinary_map_owned_any_v2\.is_direct_print_result\(\)\s*\{\s*return if default_profile == "verify-canonical-v1"\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_RESULT_L2_VERIFIER_PROFILE\s*\}\s*else\s*\{\s*mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrintResult\s*\.as_str\(\)\s*\};\s*\}\s*if mimi::core::mir::has_ordinary_map_owned_any_v2_direct_tuple_get_unpack_l1_reference_candidate\(\s*checked,?\s*\)\s*\{\s*return if default_profile == "verify-canonical-v1"\s*\{\s*mimi::core::mir::ORDINARY_MAP_DIRECT_TUPLE_GET_UNPACK_L2_VERIFIER_PROFILE\s*\}\s*else\s*\{\s*mimi::core::mir::ORDINARY_MAP_DIRECT_TUPLE_GET_UNPACK_L1_CORE_PROFILE\s*\};\s*\}\s*if mimi::core::mir::has_ordinary_map_owned_any_v2_direct_tuple_get_unpack_l2_candidate\(checked\)\s*\{\s*return mimi::core::mir::ORDINARY_MAP_DIRECT_TUPLE_GET_UNPACK_L2_VERIFIER_PROFILE;\s*\}\s*if mimi::core::mir::has_ordinary_map_set_get_any_result_i32_route_candidate\(checked\)\s*\{\s*return if default_profile == "verify-canonical-v1"\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_L2_VERIFIER_PROFILE\s*\}\s*else\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_EXECUTION_PROFILE\s*\};\s*\}\s*if mimi::core::mir::has_ordinary_map_set_get_any_i32_route_candidate\(checked\)\s*\{\s*return if default_profile == "verify-canonical-v1"\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_L2_VERIFIER_PROFILE\s*\}\s*else\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_EXECUTION_PROFILE\s*\};\s*\}\s*if mimi::core::mir::has_ordinary_map_set_get_string_route_candidate\(checked\)\s*\{\s*return if default_profile == "verify-canonical-v1"\s*\{\s*mimi::core::mir::ORDINARY_MAP_SET_GET_STRING_L2_VERIFIER_PROFILE\s*\}\s*else\s*\{\s*mimi::core::mir::ORDINARY_MAP_SET_GET_STRING_EXECUTION_PROFILE\s*\};\s*\}\s*if mimi::core::mir::has_ordinary_map_set_get_i32_route_candidate\(checked\)\s*\{\s*return if default_profile == "verify-canonical-v1"\s*\{\s*mimi::core::mir::ORDINARY_MAP_SET_GET_I32_L2_VERIFIER_PROFILE\s*\}\s*else\s*\{\s*mimi::core::mir::ORDINARY_MAP_SET_GET_I32_EXECUTION_PROFILE\s*\};\s*\}\s*if mimi::core::mir::has_ordinary_map_set_result_route_candidate\(checked\)\s*\{\s*return if default_profile == "verify-canonical-v1"\s*\{\s*mimi::core::mir::ORDINARY_MAP_SET_RESULT_L2_VERIFIER_PROFILE\s*\}\s*else\s*\{\s*mimi::core::mir::ORDINARY_MAP_SET_RESULT_EXECUTION_PROFILE\s*\};\s*\}\s*if mimi::core::mir::has_ordinary_map_new_result_route_candidate\(checked\)\s*\{\s*return mimi::core::mir::ORDINARY_MAP_NEW_RESULT_EXECUTION_PROFILE;\s*\}\s*if admission\.ordinary_map_owned_any_v2_persistent_root_alias_replacement\(\)\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE;\s*\}\s*if admission\.ordinary_map_owned_any_v2_get_or_default_string\(\)\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE;\s*}'
    main_profile_pattern+="\\s*if admission\\.ordinary_map_owned_any_v2_get_or_default_i32\\(\\)\\s*\\{\\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_I32_PROFILE;\\s*\\}"
    main_profile_pattern+="\\s*if admission\\.ordinary_map_owned_any_v2_removed_key_checked_miss\\(\\)\\s*\\{\\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE;\\s*\\}"
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2_zero_miss_i32\(\)\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE;\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2\s*==\s*mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::KeysSnapshot\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE;\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2_dynamic_keys_get\(\)\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE;\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2_values_crosscall_string_caller_index1\(\)\s*\{\s*return mimi::core::mir::CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1\s*\.as_str\(\);\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2_values_crosscall_string_caller_v1\(\)\s*\{\s*return mimi::core::mir::CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller\s*\.as_str\(\);\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2_values_producer_caller\(\)\s*\{\s*return mimi::core::mir::CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2ValuesProducerCaller\s*\.as_str\(\);\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2_map_root_get_any_caller\(\)\s*\{\s*return mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller\s*\.as_str\(\);\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2_map_root_producer_caller\(\)\s*\{\s*return mimi::core::mir::CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2MapRootProducerCaller\s*\.as_str\(\);\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2\s*==\s*mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::MapEmptyRootRootQueriesV1\s*\{\s*return mimi::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE;\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2\s*==\s*mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::RecordUnit\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE;\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2\s*==\s*mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::IsEmpty\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE;\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2\s*==\s*mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControl\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE;\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2_branch_local_set_diamond\(\)\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE;\s*\}'
    main_profile_pattern+='\s*if admission\.ordinary_map_owned_any_v2\s*==\s*mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE;\s*\}'
    main_profile_pattern+='\s*if admission\s*\.ordinary_map_owner_lifecycle_observer_v1\s*\.is_candidate\(\)\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE\s*\}'
    main_profile_pattern+='\s*else if admission\.ordinary_map_owned_any_v2\.is_direct_print\(\)\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE\s*\}'
    main_profile_pattern+='\s*else if admission\.ordinary_map_owned_any_v2\.is_checked_get\(\)\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE\s*\}'
    main_profile_pattern+='\s*else if admission\.ordinary_map_owned_any_v2\.is_computed_key_get\(\)\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE\s*\}'
    main_profile_pattern+='\s*else if admission\s*\.ordinary_map_owned_any_v2\s*\.is_computed_string_key_owner_chain\(\)\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE\s*\}'
    main_profile_pattern+='\s*else if admission\.ordinary_map_owned_any_v2\.is_values_field_move\(\)\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE\s*\}'
    main_profile_pattern+='\s*else if admission\s*\.ordinary_map_owned_any_v2\s*\.is_values_heterogeneous_string_index1\(\)\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE\s*\}'
    main_profile_pattern+='\s*else if admission\s*\.ordinary_map_owned_any_v2\s*\.is_values_local_index_i32\(\)\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE\s*\}'
    main_profile_pattern+='\s*else if admission\.ordinary_map_owned_any_v2\s*==\s*mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshotRemove\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE\s*\}'
    main_profile_pattern+='\s*else if admission\.ordinary_map_owned_any_v2\s*==\s*mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesSnapshotReplacement\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE\s*\}'
    main_profile_pattern+='\s*else if admission\s*\.ordinary_map_owned_any_v2\s*\.is_values_mixed_snapshot\(\)\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE\s*\}'
    main_profile_pattern+='\s*else if admission\.map_any_owner_complete\(\)\s*\{\s*mimi::core::mir::MAP_ANY_OWNER_V1_ISLAND\s*\}'
    main_profile_pattern+='\s*else if admission\.ordinary_map_owned_any_v2_complete\(\)\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND\s*\}'
    main_profile_pattern+='\s*else if admission\.ordinary_map_owned_get_i32_string_v1_complete\(\)\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND\s*\}'
    main_profile_pattern+='\s*else if admission\.ordinary_map_owner_lifecycle_i32_string_v1_complete\(\)\s*\{\s*mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND\s*\}'
    main_profile_pattern+='\s*else if admission\.map_root_complete\(\)\s*\{\s*mimi::core::mir::MAP_ROOT_ISLAND\s*\}'
    main_profile_pattern+='\s*else if admission\.scalar_export\s*\{\s*mimi::core::mir::SCALAR_EXPORT_ISLAND\s*\}'
    main_profile_pattern+='\s*else if admission\.scalar_actor\s*\{\s*mimi::core::mir::SCALAR_ACTOR_ISLAND\s*\}'
    main_profile_pattern+='\s*else if admission\.scalar_ffi\s*\{\s*"scalar-ffi-v1"\s*\}'
    main_profile_pattern+='\s*else if admission\.scalar_nested\s*\{\s*mimi::core::mir::SCALAR_NESTED_ISLAND\s*\}'
    main_profile_pattern+='\s*else if admission\.scalar_generic_identity_i32_complete\(\)\s*\{\s*mimi::core::mir::SCALAR_GENERIC_IDENTITY_I32_ISLAND\s*\}'
    main_profile_pattern+='\s*else if admission\.scalar_generic_identity_i64_complete\(\)\s*\{\s*mimi::core::mir::SCALAR_GENERIC_IDENTITY_I64_ISLAND\s*\}'
    main_profile_pattern+='\s*else if has_native_data_export_declaration\(checked\) && !admission\.has_candidate\(\)\s*\{\s*NATIVE_DATA_EXPORT_ROUTE_PROFILE\s*\}'
    main_profile_pattern+='\s*else if mimi::codegen::has_static_generic_trait_specialization_candidate\(checked\)\s*\{\s*mimi::codegen::STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE\s*\}\s*else\s*\{\s*default_profile\s*\}'
    main_route_pattern='let observer_admission = admission\.ordinary_map_owner_lifecycle_observer_v1;[\s\S]*?if observer_admission\.has_profile_signal\(\) && !observer_admission\.is_complete\(\)[\s\S]*?if observer_admission\.is_complete\(\)\s*\{\s*return match materialize_canonical_route\(checked, merged_file\)\s*\{\s*Ok\(route\)\s*if ordinary_map_owner_lifecycle_observer_v1_materialization_is_exact\(&route\)\s*=>\s*\{\s*select_ordinary_map_owner_lifecycle_observer_v1_route\(checked, route\.program\)'
    local selector_profile_count
    selector_profile_count="$(printf '%s\n' "$main_profile_context_without_result" | rg -U --count-matches 'MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE|ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE|OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1|OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller([^[:alnum:]_])|OrdinaryMapOwnedAnyV2ValuesProducerCaller|OrdinaryMapOwnedAnyV2MapRootGetAnyCaller|OrdinaryMapOwnedAnyV2MapRootProducerCaller|ORDINARY_MAP_OWNED_ANY_V2_(GET_OR_DEFAULT_I32_PROFILE|GET_OR_DEFAULT_STRING_PROFILE|REMOVED_KEY_CHECKED_MISS_PROFILE|ZERO_MISS_I32_PROFILE|KEYS_SNAPSHOT_PROFILE|DYNAMIC_KEYS_GET_PROFILE|IS_EMPTY_PROFILE|RECORD_UNIT_PROFILE|MAP_SET_DROP_CONTROL_PROFILE|BRANCH_LOCAL_SET_DIAMOND_PROFILE|CLONE_ISOLATION_PROFILE|PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE|DIRECT_PRINT_PROFILE|CHECKED_GET_PROFILE|COMPUTED_KEY_GET_PROFILE|COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE|VALUES_FIELD_MOVE_PROFILE|VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE|VALUES_LOCAL_INDEX_I32_PROFILE|VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE|VALUES_SNAPSHOT_REPLACEMENT_PROFILE|VALUES_MIXED_SNAPSHOT_PROFILE)' || printf '0')"
    local values_crosscall_v1_selector_count
    local values_crosscall_v2_selector_count
    values_crosscall_v1_selector_count="$(printf '%s\n' "$main_profile_context_without_result" | rg -U --count-matches 'OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller\s*\.as_str\(\)' || printf '0')"
    values_crosscall_v2_selector_count="$(printf '%s\n' "$main_profile_context_without_result" | rg -U --count-matches 'OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1\s*\.as_str\(\)' || printf '0')"
    if ! printf '%s\n' "$inner_context" | rg -U "$profile_pattern" >/dev/null \
        || ! printf '%s\n' "$profile_context" | rg -U 'Self::OrdinaryMapOwnerLifecycleObserverV1\s*=>\s*\{\s*ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE\s*\}' >/dev/null \
        || ! rg -U 'pub use super::receipt::\{\s*[^}]*ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE[^}]*\};' "$route_source" >/dev/null \
        || ! rg -U 'pub const ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE:\s*&str\s*=\s*"ordinary-map-owner-lifecycle-observer-v1";' "$receipt_source" >/dev/null \
        || ! printf '%s\n' "$inner_context" | rg -U "$observer_pattern" >/dev/null \
        || [ "$(printf '%s\n' "$observer_nodes_context" | { rg -o 'MirInstructionKind::OrdinaryMapOwnerLifecycleObserver(ToJson|Println)' || true; } | wc -l | tr -d '[:space:]')" -ne 2 ] \
        || ! printf '%s\n' "$observer_nodes_context" | rg -U 'OrdinaryMapOwnerLifecycleObserverToJson\s*\{\s*\.\.\s*\}\s*\|\s*MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln\s*\{\s*\.\.\s*\}' >/dev/null \
        || ! printf '%s\n' "$observer_anchor_context" | rg -F 'ordinary_map_owner_observer_v1_receipts.as_ref()' >/dev/null \
        || ! printf '%s\n' "$observer_anchor_context" | rg -U "$anchor_pattern" >/dev/null \
        || [ "$selector_profile_count" -ne 29 ] \
        || [ "$values_crosscall_v1_selector_count" -ne 1 ] \
        || [ "$values_crosscall_v2_selector_count" -ne 1 ] \
        || ! printf '%s\n' "$main_profile_context" | rg -U 'if admission\s*\.ordinary_map_owned_any_v2\s*\.ordinary_map_owned_any_v2_values_local_index_i32_result_branch\(\)\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_RESULT_BRANCH_PROFILE;\s*\}' >/dev/null \
        || ! printf '%s\n' "$main_profile_context" | rg -U 'if admission\.ordinary_map_owned_any_v2_values_result_crosscall_string_caller_v4\(\)\s*\{\s*return mimi::core::mir::CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCallerIndex1\s*\.as_str\(\);\s*\}' >/dev/null \
        || ! printf '%s\n' "$main_profile_context" | rg -U 'if admission\.ordinary_map_owned_any_v2_values_result_crosscall_string_caller_v3\(\)\s*\{\s*return mimi::core::mir::CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCaller\s*\.as_str\(\);\s*\}' >/dev/null \
        || ! printf '%s\n' "$main_profile_context" | rg -U 'if admission\.ordinary_map_owned_any_v2_values_result_producer_caller\(\)\s*\{\s*return mimi::core::mir::CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2ValuesResultProducerCaller\s*\.as_str\(\);\s*\}' >/dev/null \
        || ! printf '%s\n' "$main_profile_context" | rg -U 'if !admission\.ordinary_map_owned_any_v2\.is_direct_print_result\(\)[\s\S]*?checked\s*\.resource_analyses\(\)\s*\.values\(\)\s*\.any\(\|analysis\| analysis\.has_ordinary_map_owned_get_result_branch_v3_receipt\(\)\)\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND;\s*\}' >/dev/null \
        || ! printf '%s\n' "$main_profile_context" | rg -U 'if admission\.ordinary_map_owned_any_v2\s*==\s*mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControlResult\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_RESULT_BRANCH_PROFILE;\s*\}' >/dev/null \
        || ! printf '%s\n' "$main_profile_context_without_result" | rg -U "$main_profile_pattern" >/dev/null \
        || ! printf '%s\n' "$main_route_context" | rg -U "$main_route_pattern" >/dev/null \
        || ! printf '%s\n' "$materialization_context" | rg -F 'route.materialized_ordinary_map_owner_lifecycle_observer_v1_candidate' >/dev/null \
        || ! printf '%s\n' "$materialization_context" | rg -F 'ordinary_map_owner_lifecycle_observer_v1_route_receipt' >/dev/null \
        || ! printf '%s\n' "$materialization_context" | rg -U 'receipt\s*==\s*&route\.program\.route_receipt\(\s*mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE,' >/dev/null \
        || ! printf '%s\n' "$preflight_context" | rg -U 'admission\s*\.ordinary_map_owner_lifecycle_observer_v1\s*\.is_complete\(\)' >/dev/null \
        || ! printf '%s\n' "$preflight_context" | rg -U 'let validated_observers\s*=\s*mimi::core::mir::validate_ordinary_map_owner_lifecycle_observer_v1_mir_route\(program\)\?;\s*let mir_observers\s*=\s*program\.ordinary_map_owner_lifecycle_observer_v1_receipts\(\)\?;\s*if validated_observers\.is_empty\(\) \|\| validated_observers != mir_observers' >/dev/null \
        || ! printf '%s\n' "$preflight_context" | rg -F 'let profile = mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE;' >/dev/null \
        || ! printf '%s\n' "$preflight_context" | rg -F 'if receipt.profile != profile' >/dev/null \
        || ! printf '%s\n' "$preflight_context" | rg -U 'receipt\.validate_against_program\(program\)\?' >/dev/null \
        || ! printf '%s\n' "$preflight_context" | rg -F 'compile_mir_program_with_route_receipt(program, &receipt)' >/dev/null \
        || ! printf '%s\n' "$preflight_context" | rg -F 'verify_mir_with_route_receipt(program, &receipt, String::new())' >/dev/null \
        || ! printf '%s\n' "$preflight_context" | rg -F 'validate_mir_native(program)' >/dev/null \
        || [ "$(printf '%s\n' "$native_context" | { rg -o 'MirInstructionKind::OrdinaryMapOwnerLifecycleObserver(ToJson|Println)' || true; } | wc -l | tr -d '[:space:]')" -ne 2 ] \
        || ! printf '%s\n' "$native_context" | rg -U 'let observer_profile\s*=\s*crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleObserverV1\s*\.as_str\(\);[\s\S]*?if observer_instruction_count > 0\s*\|\| route_receipt\.is_some_and\(\|receipt\| receipt\.profile == observer_profile\)[\s\S]*?let Some\(receipt\) = route_receipt else \{[\s\S]*?if receipt\.profile != observer_profile[\s\S]*?receipt\.validate_against_program\(program\)[\s\S]*?validate_ordinary_map_owner_lifecycle_observer_v1_mir_route\(\s*program,[\s\S]*?observer_projection\.is_empty\(\)\s*\|\| observer_projection\.len\(\) != observer_instruction_count' >/dev/null \
        || ! printf '%s\n' "$native_receipt_adapter" | rg -U 'pub fn compile_mir_native_with_route_receipt\([\s\S]*?self\.compile_mir_native_with_route_receipt_inner\(program, receipt, None\)' >/dev/null \
        || ! printf '%s\n' "$checked_native_receipt_adapter" | rg -U 'pub fn compile_checked_mir_native_with_route_receipt\([\s\S]*?let native_checker_authorized_route = match checker_authorized_route \{\s*Some\(CheckedOrdinaryMapOwnedAnyV2Route::CheckedGet\)\s*=>\s*None,\s*route\s*=>\s*route,\s*\};\s*let result = self\.compile_mir_native_with_route_receipt_inner\(\s*program,\s*receipt,\s*native_checker_authorized_route,?\s*\);' >/dev/null \
        || [ "$(printf '%s\n' "$build_route_context" | { rg -o 'route_receipt_profile_for_checked\(' || true; } | wc -l | tr -d '[:space:]')" -ne 3 ] \
        || [ "$(printf '%s\n' "$build_route_context" | { rg -o 'canonical\.route_receipt\(receipt_profile\)' || true; } | wc -l | tr -d '[:space:]')" -ne 3 ] \
        || [ "$(printf '%s\n' "$build_route_context" | { rg -o 'compile_checked_mir_native_for_route\(' || true; } | wc -l | tr -d '[:space:]')" -ne 3 ] \
        || ! rg -U 'else\s*\{\s*codegen\.compile_checked_mir_native_with_route_receipt\(checked, canonical, receipt\)\s*\}' "$build_source" >/dev/null; then
        printf 'owner_audit_error=observer_v1_exact_route_binding_missing=main-selector+bytecode-and-native-adapters expected=CanonicalMirRouteProfile-source+exact-profile+program-validation+Checker-receipts+same-route-receipt\n' >&2
        audit_failed=1
        return
    fi
    printf 'native_build_route_receipt_binding=three-route-selections+exact-receipt+checked-native-adapter consumer=src/main/build.rs\n'
    printf 'checked_route_profile_fallback_binding=scalar-export+actor+ffi+nested+generic-i32+i64+native-data-export+static-generic-trait+default consumer=src/main/canonical_dispatch.rs\n'
    printf 'observer_v1_exact_route_binding=CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleObserverV1->ordinary-map-owner-lifecycle-observer-v1->ordered-default-selector+materialization-receipt-equality+checker-MIR-bytecode-native-verifier-preflight+same-bytecode-and-native-receipt-adapters consumer=src/main/canonical_dispatch.rs+src/main/build.rs+src/interp/bytecode/mir.rs+src/codegen/mir/eligibility.rs\n'
}
bytecode_observer_v1_route_receipt_binding

# The Values cross-call String route has two intentionally separate profiles:
# V1 selects literal index zero, while V2 selects literal index one. Keep each
# selector, bytecode adapter, and verifier paired with its own exact profile;
# the V2 enum name has the V1 spelling as a prefix, so counts below use full
# token boundaries and function-local checks rather than substring totals.
values_crosscall_string_caller_v1_v2_route_binding() {
    local dispatcher="$ROOT_DIR/src/main/canonical_dispatch.rs"
    local verifier_source="$ROOT_DIR/src/verifier/mod.rs"
    local verify_cli_source="$ROOT_DIR/src/main/verify.rs"
    local route_profile_context default_selector_context v1_default_route_context v2_default_route_context
    local v1_preflight_context v2_preflight_context
    local v1_verifier_context v2_verifier_context
    local verify_cli_context
    local v1_profile_count v2_profile_count

    route_profile_context="$(sed -n '/^pub(crate) fn route_receipt_profile_for_checked(/,/^}/p' "$dispatcher")"
    default_selector_context="$(sed -n '/^fn select_default_route_inner(/,/^fn select_scalar_actor_route(/p' "$dispatcher")"
    v2_default_route_context="$(printf '%s\n' "$default_selector_context" | sed -n '/if admission\.ordinary_map_owned_any_v2_values_crosscall_string_caller_index1()/,/if admission\.ordinary_map_owned_any_v2_values_crosscall_string_caller_v1()/p')"
    v1_default_route_context="$(printf '%s\n' "$default_selector_context" | sed -n '/if admission\.ordinary_map_owned_any_v2_values_crosscall_string_caller_v1()/,/if admission\.ordinary_map_owned_any_v2_values_producer_caller()/p')"
    v2_preflight_context="$(sed -n '/^pub(crate) fn preflight_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1(/,/^fn select_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(/p' "$dispatcher")"
    v1_preflight_context="$(sed -n '/^pub(crate) fn preflight_ordinary_map_owned_any_v2_values_crosscall_string_caller(/,/^fn select_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(/p' "$dispatcher")"
    v1_verifier_context="$(sed -n '/^pub fn verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(/,/^\/\/\/ Verify the separately versioned Values cross-call/p' "$verifier_source")"
    v2_verifier_context="$(sed -n '/^pub fn verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(/,/^\/\/\/ Verify the exact Map-root producer/p' "$verifier_source")"
    verify_cli_context="$(sed -n '/^pub(crate) fn verify(/,/^    let mut results = if let Some(canonical) = canonical/p' "$verify_cli_source")$(sed -n '/^    let mut results = if let Some(canonical) = canonical/,$p' "$verify_cli_source")"
    v1_profile_count="$(printf '%s\n' "$route_profile_context" | rg -U --count-matches 'OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller\s*\.as_str\(\)' || printf '0')"
    v2_profile_count="$(printf '%s\n' "$route_profile_context" | rg -U --count-matches 'OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1\s*\.as_str\(\)' || printf '0')"

    if [ "$v1_profile_count" -ne 1 ] || [ "$v2_profile_count" -ne 1 ] \
        || ! printf '%s\n' "$route_profile_context" | rg -U 'ordinary_map_owned_any_v2_values_crosscall_string_caller_index1\(\)\s*\{\s*return mimi::core::mir::CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1\s*\.as_str\(\);\s*\}\s*if admission\.ordinary_map_owned_any_v2_values_crosscall_string_caller_v1\(\)\s*\{\s*return mimi::core::mir::CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller\s*\.as_str\(\);' >/dev/null \
        || ! printf '%s\n' "$v2_default_route_context" | rg -U 'ordinary_map_owned_any_v2_values_crosscall_string_caller_index1\(\)[\s\S]*?OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1\s*\.as_str\(\)[\s\S]*?select_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route\(' >/dev/null \
        || printf '%s\n' "$v2_default_route_context" | rg -U 'CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller\s*\.as_str\(' >/dev/null \
        || ! printf '%s\n' "$v1_default_route_context" | rg -U 'ordinary_map_owned_any_v2_values_crosscall_string_caller_v1\(\)[\s\S]*?OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller\s*\.as_str\(\)[\s\S]*?select_ordinary_map_owned_any_v2_values_crosscall_string_caller_route\(' >/dev/null \
        || printf '%s\n' "$v1_default_route_context" | rg -U 'CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1\s*\.as_str\(' >/dev/null; then
        printf 'owner_audit_error=values_crosscall_string_caller_selector_binding_missing=V1-index0+V2-index1 expected=separate-ordered-selector-profiles-and-default-route-adapters\n' >&2
        audit_failed=1
        return
    fi

    for pattern in \
        'let profile =' \
        'ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_PROFILE' \
        'if receipt.profile != profile' \
        'validate_against_program(program)' \
        'if &expected != receipt' \
        'validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(' \
        'verify_program_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route('; do
        if ! printf '%s\n' "$v1_verifier_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=values_crosscall_string_caller_verifier_binding_missing=V1-index0 pattern=%s\n' "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    for pattern in \
        'let profile =' \
        'ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE' \
        'if receipt.profile != profile' \
        'validate_against_program(program)' \
        'if &expected != receipt' \
        'validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(' \
        'verify_program_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route('; do
        if ! printf '%s\n' "$v2_verifier_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=values_crosscall_string_caller_verifier_binding_missing=V2-index1 pattern=%s\n' "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    for pattern in \
        'OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1' \
        'validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(' \
        'compile_mir_program_with_values_crosscall_string_caller_index1_route(' \
        'validate_checked_mir_native_values_crosscall_string_caller_index1_route(' \
        'validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(' \
        'verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route('; do
        if ! printf '%s\n' "$v2_preflight_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=values_crosscall_string_caller_preflight_binding_missing=V2-index1 pattern=%s\n' "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    for pattern in \
        'OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller' \
        'validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(' \
        'compile_mir_program_with_values_crosscall_string_caller_route(' \
        'validate_checked_mir_native_values_crosscall_string_caller_route(' \
        'validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(' \
        'verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route('; do
        if ! printf '%s\n' "$v1_preflight_context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=values_crosscall_string_caller_preflight_binding_missing=V1-index0 pattern=%s\n' "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    if ! printf '%s\n' "$verify_cli_context" | rg -U 'canonical_receipt_profile\s*==\s*mimi::core::mir::CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1\s*\.as_str\(\)\s*\{\s*mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route\(' >/dev/null \
        || ! printf '%s\n' "$verify_cli_context" | rg -U 'canonical_receipt_profile\s*==\s*mimi::core::mir::CanonicalMirRouteProfile::\s*OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller\s*\.as_str\(\)\s*\{\s*mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route\(' >/dev/null; then
        printf 'owner_audit_error=values_crosscall_string_caller_verifier_dispatch_missing=main-verify expected=V1-index0+V2-index1-exact-profile-pairs\n' >&2
        audit_failed=1
        return
    fi
    printf 'values_crosscall_string_caller_route_binding=V1-index0+V2-index1->distinct-ordered-selector+exact-bytecode-token+Checker-MIR-verifier+CLI-profile-dispatch consumer=src/main/canonical_dispatch.rs+src/interp/bytecode/mir.rs+src/verifier/mod.rs+src/main/verify.rs\n'
}

values_crosscall_string_caller_v1_v2_route_binding

bytecode_route_receipt_binding \
    src/interp/bytecode/mir.rs \
    'pub fn compile_mir_program_with_route_receipt(' \
    'fn compile_mir_program_inner(' \
    'compile_mir_program_inner(program, Some(receipt), validated_narrow_route)'
bytecode_route_receipt_binding \
    src/interp/bytecode/mir.rs \
    'fn compile_mir_program_inner(' \
    'fn materialize_canonical_ffi_bindings(' \
    'canonical_ffi_route_receipt: if !has_canonical_ffi_bindings'
bytecode_route_receipt_binding \
    src/interp/bytecode/mir.rs \
    'fn compile_mir_program_inner(' \
    'fn materialize_canonical_ffi_bindings(' \
    'binding.route_receipt = Some(receipt.clone())'

bytecode_route_manifest_adapter_binding() {
    local context
    context="$(sed -n '/pub fn compile_mir_program_with_route_manifest(/,/^fn compile_mir_program_inner(/p' "$ROOT_DIR/src/interp/bytecode/mir.rs")"
    for pattern in \
        'pub fn compile_mir_program_with_route_manifest(' \
        'CanonicalMirRouteReceipt::from_manifest_round_trip(manifest)' \
        'compile_mir_program_with_route_receipt(program, &receipt)'; do
        if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=bytecode_route_manifest_adapter_missing=src/interp/bytecode/mir.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'bytecode_route_manifest_adapter_binding=manifest-round-trip-to-typed-receipt consumer=src/interp/bytecode/mir.rs\n'
}

bytecode_route_manifest_adapter_binding

reference_route_manifest_adapter_binding() {
    local context
    context="$(sed -n '/pub fn try_with_route_manifest(/,/^    pub fn with_step_limit(/p' "$ROOT_DIR/src/core/mir/reference.rs")"
    for pattern in \
        'pub fn try_with_route_manifest(' \
        'CanonicalMirRouteReceipt::from_manifest_round_trip(manifest)' \
        'self.with_route_receipt(&receipt)'; do
        if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=reference_route_manifest_adapter_missing=src/core/mir/reference.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'reference_route_manifest_adapter_binding=manifest-round-trip-to-typed-receipt consumer=src/core/mir/reference.rs\n'
}

reference_route_manifest_adapter_binding

bytecode_route_receipt_vm_guard() {
    local source_file="$1"
    local source
    source="$(cat "$ROOT_DIR/$source_file")"
    for pattern in \
        'canonical FFI binding manifest mixes route receipt identities' \
        'canonical FFI binding route receipt disagrees with program anchor' \
        '.manifest_round_trip()' \
        'canonical FFI route receipt manifest replay failed at VM boundary'; do
        if ! printf '%s\n' "$source" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=bytecode_route_receipt_vm_guard_missing=%s pattern=%s\n' \
                "$source_file" "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'bytecode_route_receipt_vm_guard=program-anchor+identity-consistency+manifest-replay consumer=%s\n' \
        "$source_file"
}

bytecode_route_receipt_vm_guard src/interp/bytecode/vm.rs

# The native LLVM adapter is also a reusable single-program consumer.  Keep an
# independent receipt anchor beside its MIR digest so an equivalent profile
# replay is idempotent while a tampered in-memory route cannot be paired with
# an already emitted module.
native_route_receipt_anchor() {
    local field_source
    local consumer_source
    field_source="$(cat "$ROOT_DIR/src/codegen/mod.rs")"
    consumer_source="$(cat "$ROOT_DIR/src/codegen/mir/eligibility.rs")"
    if ! printf '%s\n' "$field_source" | rg -F "mir_native_route_receipt: Option<crate::core::mir::CanonicalMirRouteReceipt>" >/dev/null; then
        printf 'owner_audit_error=native_route_receipt_anchor_missing=src/codegen/mod.rs field\n' >&2
        audit_failed=1
        return
    fi
    for pattern in \
        'bound.same_semantic_identity(receipt)' \
        '.manifest_round_trip()' \
        'canonical route receipt manifest replay failed' \
        'self.mir_native_route_receipt = Some(receipt.clone())'; do
        if ! printf '%s\n' "$consumer_source" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=native_route_receipt_anchor_missing=src/codegen/mir/eligibility.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'native_route_receipt_anchor=program-anchor+semantic-replay consumer=src/codegen/mir/eligibility.rs\n'
}

native_route_receipt_anchor

# Proof cache identity must consume the same canonical receipt algorithm as
# native and bytecode route admission.  Keep this as a source-level tripwire
# so a verifier-local field list cannot silently drift from the shared MIR
# identity contract.
mir_route_cache_identity_binding() {
    if ! rg -F "pub fn semantic_identity_digest(&self) -> String" \
        "$ROOT_DIR/src/core/mir/receipt.rs" >/dev/null; then
        printf 'owner_audit_error=mir_route_cache_identity_missing=src/core/mir/receipt.rs\n' >&2
        audit_failed=1
        return
    fi
    if ! rg -F ".map(crate::core::mir::CanonicalMirRouteReceipt::semantic_identity_digest)" \
        "$ROOT_DIR/src/verifier/ctx.rs" >/dev/null; then
        printf 'owner_audit_error=mir_route_cache_identity_consumer_missing=src/verifier/ctx.rs\n' >&2
        audit_failed=1
        return
    fi
    printf 'mir_route_cache_identity_binding=shared-receipt-digest consumer=src/verifier/ctx.rs\n'
}

mir_route_cache_identity_binding

# Native typed receipts must cross the same manifest serialization boundary as
# bytecode and verifier receipts.  This catches a future field-order or parser
# drift before LLVM emission can accept a witness that another consumer cannot
# replay.
native_route_manifest_replay_binding() {
    local source
    source="$(cat "$ROOT_DIR/src/codegen/mir/eligibility.rs")"
    for pattern in \
        '.manifest_round_trip()' \
        'canonical route receipt manifest replay failed'; do
        if ! printf '%s\n' "$source" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=native_route_manifest_replay_missing=src/codegen/mir/eligibility.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'native_route_manifest_replay_binding=manifest-parse+identity-check consumer=src/codegen/mir/eligibility.rs\n'
}

native_route_manifest_replay_binding

# The public native manifest entry must be a thin adapter over the typed
# receipt entry.  Keeping this forwarding contract in the owner audit prevents
# a future CLI-facing parser from growing a second emission path that skips
# the native receipt anchor and semantic replay checks above.
native_route_manifest_adapter_binding() {
    local source
    source="$(cat "$ROOT_DIR/src/codegen/mir/eligibility.rs")"
    for pattern in \
        'pub fn compile_mir_native_with_route_manifest(' \
        'CanonicalMirRouteReceipt::from_manifest_round_trip(manifest)' \
        'self.compile_mir_native_with_route_receipt(program, &receipt)'; do
        if ! printf '%s\n' "$source" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=native_route_manifest_adapter_missing=src/codegen/mir/eligibility.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'native_route_manifest_adapter_binding=manifest-to-typed-receipt-forward consumer=src/codegen/mir/eligibility.rs\n'
}

native_route_manifest_adapter_binding

# Bind the public source entry to the hash-bearing checked-program adapter.
# This keeps the caller's BLAKE3 provenance on the same path as the verifier
# receipt; a renamed call or an empty/hashless entry must fail closed.
source_hash_entry_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local call_pattern="$4"
    local hash_pattern="$5"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$call_pattern" >/dev/null; then
        printf 'owner_audit_error=source_hash_entry_call_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$hash_pattern" >/dev/null; then
        printf 'owner_audit_error=source_hash_entry_hash_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'source_hash_entry_binding=blake3-source-hash consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

source_hash_entry_binding \
    src/verifier/mod.rs \
    'pub fn verify_ffi_source(' \
    'pub fn verify_ffi_checked(' \
    'verify_ffi_checked_with_source_hash(' \
    'blake3::hash(source.as_bytes()).to_hex().to_string(),'

# Tie the checked-program consumer's declared hash parameter to the exact
# route adapter argument.  This is intentionally separate from the public
# entry check so a parameter rename or a forwarding omission cannot hide
# behind a still-correct source entry marker.
source_hash_parameter_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local parameter_pattern="$4"
    local invocation_pattern="$5"
    local context compat_context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    compat_context="$(sed -n '/^pub(crate) fn verify_scalar_route(/,/^pub(crate) fn verify_literal_contracts(/p' "$ROOT_DIR/src/verifier/ffi_compat.rs")"
    if ! printf '%s\n' "$context" | rg -F "$parameter_pattern" >/dev/null; then
        printf 'owner_audit_error=source_hash_parameter_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$invocation_pattern" >/dev/null \
        || ! printf '%s\n' "$compat_context" | rg -F "$parameter_pattern" >/dev/null \
        || ! printf '%s\n' "$compat_context" | rg -F 'super::verify_ffi_mir_results_with_route_receipt(mir, source_hash.clone(), &receipt)' >/dev/null \
        || ! printf '%s\n' "$compat_context" | rg -F 'super::validate_mir_result_provenance(' >/dev/null; then
        printf 'owner_audit_error=source_hash_parameter_forward_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'source_hash_parameter_binding=declared-and-forwarded consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

source_hash_parameter_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_checked_with_source_hash(' \
    'pub fn is_z3_available(' \
    'source_hash: String' \
    'ffi_compat::verify_scalar_route(program, &canonical, source_hash)'

# Keep the FFI MIR adapter's proof artifacts sourced from the same hash passed
# into the MIR verifier.  The route-level check above proves the caller passes
# a hash; this check proves the adapter clones it into MIR verification and
# validates every returned artifact against that same value.
mir_verifier_source_provenance_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local execution_pattern="$4"
    local validation_pattern="$5"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$execution_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_verifier_source_provenance_missing=%s::%s phase=mir-call\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$validation_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_verifier_source_provenance_missing=%s::%s phase=artifact-validation\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'mir_verifier_source_provenance_binding=single-source-hash consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

mir_verifier_source_provenance_binding \
    src/verifier/mod.rs \
    'pub fn verify_ffi_mir_with_source_hash(' \
    'pub fn verify_ffi_mir_with_route_receipt(' \
    'verify_ffi_mir_results_with_route_receipt(program, source_hash.clone(), &receipt)?' \
    'validate_mir_result_provenance(&results, &receipt, &source_hash, "verify_ffi_mir")?'

# Keep the shared artifact validator bound to every immutable identity field.
# Source hash, MIR digest, and route receipt are one proof witness; validating
# only one of them would permit a copied artifact to cross a canonical route.
mir_result_identity_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local source_pattern="$4"
    local mir_pattern="$5"
    local receipt_pattern="$6"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$source_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_result_identity_missing=%s::%s field=source-hash\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$mir_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_result_identity_missing=%s::%s field=mir-hash\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$receipt_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_result_identity_missing=%s::%s field=route-receipt\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'mir_result_identity_binding=source-hash+mir-hash+route-receipt consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

mir_result_identity_binding \
    src/verifier/mod.rs \
    'fn validate_mir_result_provenance(' \
    'pub fn verify_mir_with_route_manifest(' \
    'if artifact.source_hash != source_hash' \
    'if artifact.mir_hash != receipt.mir_digest' \
    'if artifact.mir_route_receipt.as_ref() != Some(receipt)'

# Both direct MIR entry points must validate a caller-supplied receipt before
# proof execution. This keeps the public general verifier and FFI verifier on
# the same fail-closed route boundary.
mir_route_receipt_validation_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F '.validate_against_program(program)' >/dev/null; then
        printf 'owner_audit_error=mir_route_receipt_validation_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'mir_route_receipt_validation_binding=validate-against-program consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

mir_route_receipt_validation_binding \
    src/verifier/mod.rs \
    'pub fn verify_mir_with_route_receipt(' \
    'fn validate_mir_result_provenance('
mir_route_receipt_validation_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_mir_with_route_receipt_profile(' \
    'pub fn verify_ffi_mir_with_route_manifest('

# Manifest replay must reconstruct a receipt and then use the same direct MIR
# verifier adapter. Parsing a manifest without forwarding through that adapter
# would allow a CLI-only proof path to diverge from the checked API.
mir_route_manifest_replay_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local manifest_pattern="$4"
    local forwarding_pattern="$5"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$manifest_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_route_manifest_replay_missing=%s::%s phase=manifest-parse\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$forwarding_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_route_manifest_replay_missing=%s::%s phase=direct-adapter\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F 'CanonicalMirRouteReceipt::from_manifest_round_trip(manifest)' >/dev/null; then
        printf 'owner_audit_error=mir_route_manifest_replay_missing=%s::%s phase=manifest-round-trip\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'mir_route_manifest_replay_binding=manifest-round-trip+direct-adapter consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

mir_route_manifest_replay_binding \
    src/verifier/mod.rs \
    'pub fn verify_mir_with_route_manifest(' \
    'pub fn verify_ffi_mir_with_route_manifest(' \
    'CanonicalMirRouteReceipt::from_manifest_round_trip(manifest)' \
    'verify_mir_with_route_receipt(program, &receipt, source_hash)'
mir_route_manifest_replay_binding \
    src/verifier/mod.rs \
    'pub fn verify_ffi_mir_with_route_manifest(' \
    'fn verify_ffi_checked_with_source_hash(' \
    'CanonicalMirRouteReceipt::from_manifest_round_trip(manifest)' \
    'verify_ffi_mir_with_route_receipt(program, &receipt, source_hash)'

# Capability admission must happen before symbolic execution in the direct FFI
# verifier. Compare source-order positions so a future refactor cannot leave a
# provenance validator while moving malformed MIR past the capability gate.
mir_ffi_capability_order_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local capability_pattern="$4"
    local execution_pattern="$5"
    local context
    local capability_line
    local execution_line
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    capability_line="$(printf '%s\n' "$context" | awk -v pat="$capability_pattern" '!found && index($0, pat) { found = NR } END { if (found) print found }')"
    execution_line="$(printf '%s\n' "$context" | awk -v pat="$execution_pattern" '!found && index($0, pat) { found = NR } END { if (found) print found }')"
    if [ -z "$capability_line" ] || [ -z "$execution_line" ]; then
        printf 'owner_audit_error=mir_ffi_capability_order_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if [ "$capability_line" -ge "$execution_line" ]; then
        printf 'owner_audit_error=mir_ffi_capability_order_drift=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'mir_ffi_capability_order_binding=capability-before-execution consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

mir_ffi_capability_order_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_mir_results_with_route_receipt(' \
    'pub fn verify_ffi_mir_with_route_receipt(' \
    'validate_ffi_verifier_mir_capabilities(program)' \
    'mir::verify_ffi_program_with_route_receipt(program, source_hash, receipt)'

# A caller-supplied route receipt is the outer admission boundary. Validate it
# before invoking the source-hash adapter so malformed route metadata cannot
# reach capability/proof work or produce a misleading artifact.
mir_route_receipt_order_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local receipt_pattern="$4"
    local adapter_pattern="$5"
    local context
    local receipt_line
    local adapter_line
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    receipt_line="$(printf '%s\n' "$context" | awk -v pat="$receipt_pattern" '!found && index($0, pat) { found = NR } END { if (found) print found }')"
    adapter_line="$(printf '%s\n' "$context" | awk -v pat="$adapter_pattern" '!found && index($0, pat) { found = NR } END { if (found) print found }')"
    if [ -z "$receipt_line" ] || [ -z "$adapter_line" ]; then
        printf 'owner_audit_error=mir_route_receipt_order_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if [ "$receipt_line" -ge "$adapter_line" ]; then
        printf 'owner_audit_error=mir_route_receipt_order_drift=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'mir_route_receipt_order_binding=receipt-before-adapter consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

mir_route_receipt_order_binding \
    src/verifier/mod.rs \
    'pub fn verify_mir_with_route_receipt(' \
    'fn validate_mir_result_provenance(' \
    '.validate_against_program(program)' \
    'let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?'
mir_route_receipt_order_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_mir_with_route_receipt_profile(' \
    'pub fn verify_ffi_mir_with_route_manifest(' \
    '.validate_against_program(program)' \
    'mir::verify_ffi_program_with_route_receipt(program, source_hash.clone(), receipt)?'

# Keep the public CLI verifier on the same receipt-bearing adapter as the
# library entry point, and keep the top-level error boundary routed through
# the shared formatter.  These checks prevent a future CLI refactor from
# silently dropping route provenance while all direct APIs remain correct.
mir_cli_verifier_route_binding() {
    local source_file="$1"
    local adapter_pattern="$2"
    if ! rg -F "$adapter_pattern" "$ROOT_DIR/$source_file" >/dev/null; then
        printf 'owner_audit_error=mir_cli_verifier_route_missing=%s pattern=%s\n' \
            "$source_file" "$adapter_pattern" >&2
        audit_failed=1
        return
    fi
    printf 'mir_cli_verifier_route_binding=receipt-bearing-adapter consumer=%s\n' \
        "$source_file"
}

mir_cli_error_boundary_binding() {
    local source_file="$1"
    local boundary_pattern="$2"
    if ! rg -F "$boundary_pattern" "$ROOT_DIR/$source_file" >/dev/null; then
        printf 'owner_audit_error=mir_cli_error_boundary_missing=%s pattern=%s\n' \
            "$source_file" "$boundary_pattern" >&2
        audit_failed=1
        return
    fi
    printf 'mir_cli_error_boundary_binding=shared-format-cli-error consumer=%s\n' \
        "$source_file"
}

mir_cli_verifier_route_binding \
    src/main/verify.rs \
    'verify_mir_with_route_receipt(&canonical, &receipt, source_hash)?'
mir_cli_error_boundary_binding \
    src/main.rs \
    'eprintln!("{}", format_cli_error(&e));'

mir_cli_disasm_route_binding() {
    local source_file="$1"
    local renderer_pattern="$2"
    local rejection_pattern="$3"
    local source
    source="$(cat "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$source" | rg -F "$renderer_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_cli_disasm_route_missing=%s phase=renderer\n' \
            "$source_file" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$source" | rg -F "$rejection_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_cli_disasm_route_missing=%s phase=rejection\n' \
            "$source_file" >&2
        audit_failed=1
        return
    fi
    printf 'mir_cli_disasm_route_binding=shared-format-cli-error consumer=%s\n' \
        "$source_file"
}

mir_cli_disasm_route_binding \
    src/main/disasm_cmd.rs \
    'crate::format_cli_error(&error.to_string())' \
    'crate::format_cli_error(&message)'

mir_cli_route_formatter_provenance_binding() {
    local source_file="$1"
    local classifier_pattern="$2"
    local origin_pattern="$3"
    local source
    source="$(cat "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$source" | rg -F "$classifier_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_cli_route_formatter_provenance_missing=%s phase=classifier\n' \
            "$source_file" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$source" | rg -F "$origin_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_cli_route_formatter_provenance_missing=%s phase=origin\n' \
            "$source_file" >&2
        audit_failed=1
        return
    fi
    printf 'mir_cli_route_formatter_provenance_binding=registry-code+mir.route-origin consumer=%s\n' \
        "$source_file"
}

mir_cli_route_formatter_provenance_binding \
    src/main.rs \
    'canonical_mir_route_code_location_in_message(message)' \
    'runtime_system("mir.route")'

if ! rg -q '^[[:space:]]*fn scalar_ffi_c_abi_and_side_effect_order_match_three_consumers\(' \
    "$ROOT_DIR/src/tests/canonical_scalar_ffi.rs"; then
    printf 'owner_audit_error=missing_closed_scalar_zero_owner_evidence\n' >&2
    audit_failed=1
else
    emit_closed_scalar_marker 'closed_scalar_zero_owner_evidence=src/tests/canonical_scalar_ffi.rs::scalar_ffi_c_abi_and_side_effect_order_match_three_consumers'
    if ! sed -n "/^[[:space:]]*fn scalar_ffi_c_abi_and_side_effect_order_match_three_consumers(/,/^[[:space:]]*#\[test\]/p" \
        "$ROOT_DIR/src/tests/canonical_scalar_ffi.rs" | rg -F 'CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route)' >/dev/null; then
        printf 'owner_audit_error=closed_scalar_zero_owner_evidence_missing_canonical_route_assertion\n' >&2
        audit_failed=1
    else
        emit_closed_scalar_marker 'closed_scalar_zero_owner_evidence_marker=CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route)'
    fi
fi

if ! rg -q '^[[:space:]]*fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts\(' \
    "$ROOT_DIR/tests/real_world_cli.rs"; then
    printf 'owner_audit_error=missing_closed_scalar_cli_evidence\n' >&2
    audit_failed=1
else
    emit_closed_scalar_marker 'closed_scalar_cli_evidence=tests/real_world_cli.rs::canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts'
    if ! sed -n "/^[[:space:]]*fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts(/,/^[[:space:]]*#\[test\]/p" \
        "$ROOT_DIR/tests/real_world_cli.rs" | rg 'for contracts in \[true, false\]' >/dev/null; then
        printf 'owner_audit_error=closed_scalar_cli_evidence_missing_contract_matrix\n' >&2
        audit_failed=1
    elif ! sed -n "/^[[:space:]]*fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts(/,/^[[:space:]]*#\[test\]/p" \
        "$ROOT_DIR/tests/real_world_cli.rs" | rg 'for explicit_mir in \[false, true\]' >/dev/null; then
        printf 'owner_audit_error=closed_scalar_cli_evidence_missing_mir_matrix\n' >&2
        audit_failed=1
    elif ! sed -n "/^[[:space:]]*fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts(/,/^[[:space:]]*#\[test\]/p" \
        "$ROOT_DIR/tests/real_world_cli.rs" | rg -F 'func relay(value: i64) -> i64 { value }' >/dev/null; then
        printf 'owner_audit_error=closed_scalar_cli_evidence_missing_pure_nested_helper\n' >&2
        audit_failed=1
    elif ! sed -n "/^[[:space:]]*fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts(/,/^[[:space:]]*#\[test\]/p" \
        "$ROOT_DIR/tests/real_world_cli.rs" | rg 'assert!\(!.*contains\("canonical route disposition: legacy"\)\)' >/dev/null; then
        printf 'owner_audit_error=closed_scalar_cli_evidence_missing_legacy_route_assertion\n' >&2
        audit_failed=1
    else
        cli_evidence_body="$(sed -n "/^[[:space:]]*fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts(/,/^[[:space:]]*#\[test\]/p" "$ROOT_DIR/tests/real_world_cli.rs")"
        missing_abi=0
        for abi_symbol in mir_ffi_i32 mir_ffi_i64 mir_ffi_bool mir_ffi_f32 mir_ffi_f32_code mir_ffi_f64 mir_ffi_store; do
            if ! printf '%s\n' "$cli_evidence_body" | rg "\"${abi_symbol}\"" >/dev/null; then
                printf 'owner_audit_error=closed_scalar_cli_evidence_missing_abi_symbol symbol=%s\n' "$abi_symbol" >&2
                missing_abi=1
            fi
        done
        if [ "$missing_abi" -ne 0 ]; then
            audit_failed=1
        else
            emit_closed_scalar_marker 'closed_scalar_cli_matrix_marker=contracts:[true, false];explicit_mir:[false, true]'
            emit_closed_scalar_marker 'closed_scalar_cli_abi_marker=mir_ffi_i32,mir_ffi_i64,mir_ffi_bool,mir_ffi_f32,mir_ffi_f32_code,mir_ffi_f64,mir_ffi_store;legacy_route_asserted_absent'
            emit_closed_scalar_marker 'closed_scalar_cli_nested_helper_marker=relay(i64);separate_main_ffi_call'
        fi
    fi
fi

expected_closed_scalar_marker_sequence=(
    'closed_scalar_zero_owner_evidence=src/tests/canonical_scalar_ffi.rs::scalar_ffi_c_abi_and_side_effect_order_match_three_consumers'
    'closed_scalar_zero_owner_evidence_marker=CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route)'
    'closed_scalar_cli_evidence=tests/real_world_cli.rs::canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts'
    'closed_scalar_cli_matrix_marker=contracts:[true, false];explicit_mir:[false, true]'
    'closed_scalar_cli_abi_marker=mir_ffi_i32,mir_ffi_i64,mir_ffi_bool,mir_ffi_f32,mir_ffi_f32_code,mir_ffi_f64,mir_ffi_store;legacy_route_asserted_absent'
    'closed_scalar_cli_nested_helper_marker=relay(i64);separate_main_ffi_call'
)
if [ "${closed_scalar_marker_sequence[*]}" != "${expected_closed_scalar_marker_sequence[*]}" ]; then
    printf 'owner_audit_error=closed_scalar_evidence_marker_sequence_drift\n' >&2
    audit_failed=1
else
    printf 'scalar_evidence_marker_sequence_status=ok\n'
fi

require_codegen_closed_route_tripwire() {
    local route_class="$1"
    local test_name="$2"
    shift 2
    local -a route_markers=("$@")
    local marker_summary
    local body
    if [ "${#route_markers[@]}" -eq 0 ]; then
        printf 'owner_audit_error=codegen_closed_route_tripwire_missing_marker_configuration class=%s test=%s\n' \
            "$route_class" "$test_name" >&2
        audit_failed=1
        return
    fi
    body="$(sed -n "/^[[:space:]]*fn ${test_name}(/,/^[[:space:]]*#\\[test\\]/p" \
        "$ROOT_DIR/src/codegen/tests.rs")"
    if [ -z "$body" ]; then
        printf 'owner_audit_error=missing_codegen_closed_route_tripwire class=%s test=%s\n' \
            "$route_class" "$test_name" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$body" | rg 'assert(_eq|_ne)?!' >/dev/null; then
        printf 'owner_audit_error=codegen_closed_route_tripwire_missing_test_assertion class=%s test=%s\n' \
            "$route_class" "$test_name" >&2
        audit_failed=1
        return
    fi
    for route_marker in "${route_markers[@]}"; do
        if ! printf '%s\n' "$body" | rg -F "$route_marker" >/dev/null; then
            printf 'owner_audit_error=codegen_closed_route_tripwire_missing_marker class=%s test=%s marker=%s\n' \
                "$route_class" "$test_name" "$route_marker" >&2
            audit_failed=1
            return
        fi
    done
    printf -v marker_summary '%s,' "${route_markers[@]}"
    marker_summary="${marker_summary%,}"
    printf 'closed_codegen_route_tripwire=%s test=%s markers=%s\n' \
        "$route_class" "$test_name" "$marker_summary"
}

require_codegen_closed_route_tripwire \
    dynamic-trait \
    compile_checked_rejects_dynamic_protocol_method_without_legacy_owner \
    'MIR_ROUTE_COVERAGE_ERROR_CODE'
require_codegen_closed_route_tripwire \
    flow-transition \
    compile_checked_routes_multi_transition_flow_through_canonical_mir \
    'canonical multi-transition Flow must emit main'
require_codegen_closed_route_tripwire \
    trait-impl-specialization \
    compile_checked_rejects_trait_impl_specialization_without_legacy_owner \
    'MIR_ROUTE_COVERAGE_ERROR_CODE'
require_codegen_closed_route_tripwire \
    nested-function-body \
    compile_checked_routes_captured_nested_helper_without_legacy_owner \
    'nested_callable_scopes().len()' \
    'canonical MIR default route' \
    'test_legacy_codegen_function_calls().is_empty()'
require_codegen_closed_route_tripwire \
    export-wrapper \
    compile_checked_routes_scalar_export_wrapper_without_legacy_access \
    'classify_canonical_mir_route_admission(&program).scalar_export' \
    'owner_tripwire_export'
require_codegen_closed_route_tripwire \
    actor-method \
    compile_checked_routes_scalar_actor_through_canonical_mir_without_legacy_access \
    'CanonicalMirRouteProfile::ScalarActor' \
    'route.admission.scalar_actor' \
    'actor_execution_plan()' \
    'OwnerTripwireWorker__read__method'
require_codegen_closed_route_tripwire \
    actor-method-rejected \
    compile_checked_rejects_contract_bearing_actor_without_legacy_owner \
    'scalar_actor_contract_boundary_reason(&program)'
require_codegen_closed_route_tripwire \
    map-any-native-hard-gate \
    compile_checked_fails_closed_on_reachable_map_any_unpack \
    'expect_err("native compilation must not truncate a stored String into i32")' \
    'Some(crate::diagnostic::codes::E0722)' \
    'runtime Any value has no checked tag'
require_codegen_closed_route_tripwire \
    map-any-rejected \
    compile_checked_rejects_map_any_borrowed_access_without_legacy_owner \
    'MIR_ROUTE_COVERAGE_ERROR_CODE'
require_direct_mir_behavior_tripwire() {
    local test_name="$1"
    local body
    body="$(sed -n "/^[[:space:]]*fn ${test_name}(/,/^[[:space:]]*#\\[test\\]/p" \
        "$ROOT_DIR/src/codegen/tests.rs")"
    if [ -z "$body" ]; then
        printf 'owner_audit_error=missing_direct_mir_behavior_tripwire test=%s\n' \
            "$test_name" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$body" | rg -F 'is_exact_s8_flow_transition(&program)' >/dev/null \
        || ! printf '%s\n' "$body" | rg -F '__mimi_transition_Counter__inc__Zero' >/dev/null; then
        printf 'owner_audit_error=direct_mir_behavior_tripwire_missing_flow_and_emission test=%s\n' \
            "$test_name" >&2
        audit_failed=1
        return
    fi
    printf 'direct_mir_behavior_tripwire=%s\n' "$test_name"
}

require_direct_mir_behavior_tripwire \
    compile_checked_routes_exact_s8_flow_through_canonical_mir

# A direct native CodeGenerator API must reject reachable DynamicAnyUnpack
# before it can create an LLVM entry point. The same fixture also proves that
# strict Resolved-native and public subset APIs cannot bypass that gate. Keep
# the exact test in the reproducible probe runner so this source contract is
# paired with an executed direct probe before the audit can be accepted.
map_any_native_codegen_direct_probe_contract() {
    local test_name=compile_checked_fails_closed_on_reachable_map_any_unpack
    local body="$ROOT_DIR/src/codegen/tests.rs"
    local context
    context="$(sed -n "/^[[:space:]]*fn ${test_name}(/,/^[[:space:]]*#\\[test\\]/p" "$body")"
    for marker in \
        '.compile_checked(&program)' \
        'Some(crate::diagnostic::codes::E0722)' \
        'compile_resolved_native(&program)' \
        'compile_resolved_subset(&program' \
        'public subset API must not bypass the reachable Any hard gate'; do
        if ! printf '%s\n' "$context" | rg -F "$marker" >/dev/null; then
            printf 'owner_audit_error=map_any_native_codegen_direct_contract_missing test=%s marker=%s\n' \
                "$test_name" "$marker" >&2
            audit_failed=1
            return
        fi
    done
    printf 'map_any_native_codegen_direct_probe_contract=direct-CheckedProgram-API+E0722+strict-Resolved+subset-hard-gates test=codegen::tests::%s\n' \
        "$test_name"
}

map_any_native_codegen_direct_probe_contract

audit_original_legacy_owner_gates() {
    local build_legacy_context flow_context ffi_context native_checked_context codegen_probe_context
    local native_ast_entry_defs native_ast_body_defs
    local owner production_edge consumer_edge migration coverage scope blocker row path actual_roster=''
    local expected_roster='CodegenLegacyRemainder,FlowVerifierCompatibility,FfiVerifierCompatibility,DualVerifierCompatibility'
    local owner_count=0
    local -a rows=()

    build_legacy_context="$(sed -n '/DefaultMirRoute::Legacy/,/DefaultMirRoute::Rejected/p' \
        "$ROOT_DIR/src/main/build.rs")"
    if ! printf '%s\n' "$build_legacy_context" | rg -F 'codegen.compile_checked(&checked_program)' >/dev/null; then
        printf 'owner_audit_error=CodegenLegacyRemainder_production_call_edge_missing=src/main/build.rs::DefaultMirRoute::Legacy->codegen.compile_checked\n' >&2
        audit_failed=1
    fi
    for path in src/main/run.rs src/main/test.rs src/main/disasm_cmd.rs; do
        if ! rg -Fq 'DefaultMirRoute::Legacy' "$ROOT_DIR/$path" \
            || ! rg -Fq '.compile_file(' "$ROOT_DIR/$path" \
            || ! rg -Fq 'report_legacy_route' "$ROOT_DIR/$path"; then
            printf 'owner_audit_error=CodegenLegacyRemainder_legacy_consumer_edge_missing path=%s\n' "$path" >&2
            audit_failed=1
        fi
    done
    native_ast_entry_defs="$(rg -n \
        '^[[:space:]]*(pub\([^)]*\)[[:space:]]+)?(pub[[:space:]]+)?fn (compile_file|compile_file_inner|compile_file_with_resolved)\(' \
        "$ROOT_DIR/src/codegen/compile.rs" || true)"
    if [ -n "$native_ast_entry_defs" ]; then
        printf 'owner_audit_error=CodegenLegacyRemainder_native_file_ast_entry_remains\n%s\n' \
            "$native_ast_entry_defs" >&2
        audit_failed=1
    fi
    native_ast_body_defs="$(rg -n \
        '^[[:space:]]*(pub\([^)]*\)[[:space:]]+)?(pub[[:space:]]+)?fn (compile_func_legacy|compile_generic_func|compile_func_body|emit_return|emit_implicit_return|declare_func|legacy_param_llvm_type|bind_func_params)\(' \
        "$ROOT_DIR/src/codegen/func.rs" || true)"
    if [ -n "$native_ast_body_defs" ]; then
        printf 'owner_audit_error=CodegenLegacyRemainder_native_ast_body_emitter_remains\n%s\n' \
            "$native_ast_body_defs" >&2
        audit_failed=1
    fi
    native_checked_context="$(sed -n '/pub fn compile_checked(/,/fn try_compile_exact_migrated_mir_island(/p' \
        "$ROOT_DIR/src/codegen/compile.rs")"
    if printf '%s\n' "$native_checked_context" \
        | rg '\.legacy_body_file[[:space:]]*\(|\.compile_file_inner[[:space:]]*\(|\.compile_func_legacy[[:space:]]*\(|\.compile_generic_func[[:space:]]*\(' >/dev/null; then
        printf 'owner_audit_error=CodegenLegacyRemainder_checked_native_boundary_reopened_AST_emitter\n' >&2
        audit_failed=1
    fi
    codegen_probe_context="$(sed -n \
        '/^[[:space:]]*fn native_checked_boundary_preserves_positive_rejection_and_bytecode_compatibility(/,/^[[:space:]]*#\[test\]/p' \
        "$ROOT_DIR/src/codegen/tests.rs")"
    for marker in \
        '.compile_checked(&positive)' \
        '.compile_checked(&compatibility)' \
        'test_legacy_codegen_function_calls().is_empty()' \
        'BytecodeCompiler::new()' \
        'bytecode_program.ast.is_some()' \
        'crate::interp::Value::Int(41)'; do
        if ! printf '%s\n' "$codegen_probe_context" | rg -F "$marker" >/dev/null; then
            printf 'owner_audit_error=CodegenLegacyRemainder_direct_compatibility_probe_missing marker=%s\n' \
                "$marker" >&2
            audit_failed=1
            break
        fi
    done
    printf 'native_codegen_ast_fifth_pass=physically-retired production_file_ast_entry=0 cfg_test_adapter=0 old_body_emitter=0 direct_probe=codegen::tests::native_checked_boundary_preserves_positive_rejection_and_bytecode_compatibility\n'
    printf 'default_bytecode_ast_compatibility=reachable status=open consumers=src/main/run.rs+src/main/test.rs+src/main/disasm_cmd.rs\n'

    if ! rg -Fq 'mimi::verifier::verify_checked_dual(&checked_program' "$ROOT_DIR/src/main/verify.rs" \
        || ! rg -Fq 'mimi::verifier::verify_checked(&checked_program' "$ROOT_DIR/src/main/verify.rs"; then
        printf 'owner_audit_error=FlowVerifierCompatibility_production_call_edge_missing=src/main/verify.rs::verify_checked_dual+verify_checked\n' >&2
        audit_failed=1
    fi
    flow_context="$(sed -n '/^pub fn verify_checked(/,/^\/\/\/ Parse source and verify extern call sites/p' \
        "$ROOT_DIR/src/verifier/mod.rs")"
    if ! printf '%s\n' "$flow_context" | rg -F 'return Ok(verifier.verify_checked(program));' >/dev/null \
        || ! printf '%s\n' "$flow_context" | rg -F 'if actor_compatibility' >/dev/null; then
        printf 'owner_audit_error=FlowVerifierCompatibility_legacy_consumer_edge_missing=verify_checked_actor_compatibility_fallback\n' >&2
        audit_failed=1
    fi

    if ! rg -Fq 'verify_ffi_checked_with_source_hash(' "$ROOT_DIR/src/main/verify.rs"; then
        printf 'owner_audit_error=FfiVerifierCompatibility_production_call_edge_missing=src/main/verify.rs->verify_ffi_checked_with_source_hash\n' >&2
        audit_failed=1
    fi
    ffi_context="$(sed -n '/^pub fn verify_ffi_checked_with_source_hash(/,/^fn verify_ffi_checked_with_source_hash_and_z3_override(/p' \
        "$ROOT_DIR/src/verifier/mod.rs")"
    if ! printf '%s\n' "$ffi_context" | rg -F 'verify_ffi_checked_with_source_hash_and_z3_override(program, source_hash, None)' >/dev/null \
        || ! rg -Fq 'verify_ffi_checked_body_with_source_hash_and_z3_override(' "$ROOT_DIR/src/verifier/mod.rs"; then
        printf 'owner_audit_error=FfiVerifierCompatibility_legacy_consumer_edge_missing=checked_ffi_body_consumer\n' >&2
        audit_failed=1
    fi

    if ! rg -Fq 'mimi::verifier::verify_checked_dual(&checked_program' "$ROOT_DIR/src/main/verify.rs" \
        || ! rg -Fq 'let mut resolved_results = primary.verify_checked(program);' "$ROOT_DIR/src/verifier/mod.rs" \
        || ! rg -Fq 'mir_request::verify(&request, program' "$ROOT_DIR/src/verifier/mod.rs"; then
        printf 'owner_audit_error=DualVerifierCompatibility_production_or_consumer_edge_missing=default-dual-entry+Resolved-primary+MIR-secondary\n' >&2
        audit_failed=1
    fi

    rows+=(
        'CodegenLegacyRemainder|src/main/build.rs::DefaultMirRoute::Legacy->codegen.compile_checked[checked-native-MIR-or-Resolved-or-coverage;no-surface-AST-body]|src/codegen/compile.rs::CodeGenerator.compile_checked->MIR-or-Resolved-checked-native-consumer[old-body-edge-absent]|open|partial|native-AST-fifth-pass+test-adapters+orphan-body-physically-retired;original-Codegen-capability-coverage-remains-partial|original-Codegen-supported-capability-migration-and-behavior-regression-not-fully-proven'
        'FlowVerifierCompatibility|src/main/verify.rs::verify_checked_dual->verifier.verify_checked_dual;verify_checked|src/verifier/mod.rs::verify_checked[actor_compatibility]->Verifier.verify_checked|open|partial|selected-flow-parity-tests;actor-compatibility-fallback-remains|whole-Flow-proof-coverage-is-unproven-and-actor-compatibility-fallback-remains'
        'FfiVerifierCompatibility|src/main/verify.rs::ExportedAbiBodyCompatibility->verify_ffi_checked_with_source_hash|src/verifier/mod.rs::verify_ffi_checked_with_source_hash->verify_ffi_checked_body_with_source_hash_and_z3_override|open|partial|ScalarFfi+verifier-only-String-record-slice;unsupported-Ensures-deferred|unsupported-call-contexts-and-Ensures-remain-deferred'
        'DualVerifierCompatibility|src/main/verify.rs::default-verify->verify_checked_dual|src/verifier/mod.rs::verify_checked_dual->primary.Verifier.verify_checked+secondary.mir_request.verify|open|partial|selected-Resolved-plus-MIR-pairs;all-owner-peer-coverage-unproven|peer-coverage-divergence-and-deferred-boundaries-remain-open'
    )
    printf 'legacy_gate_owner_schema=canonical-mir-original-owner-gate-v1\n'
    for row in "${rows[@]}"; do
        IFS='|' read -r owner production_edge consumer_edge migration coverage scope blocker <<< "$row"
        owner_count=$((owner_count + 1))
        actual_roster+="${actual_roster:+,}${owner}"
        if [ "$migration" != open ] || [ "$coverage" != partial ] \
            || [ -z "$production_edge" ] || [ -z "$consumer_edge" ] \
            || [ -z "$scope" ] || [ -z "$blocker" ]; then
            printf 'owner_audit_error=original_legacy_owner_not_honestly_open owner=%s migration=%s coverage=%s\n' \
                "$owner" "$migration" "$coverage" >&2
            audit_failed=1
        fi
        printf 'legacy_gate_owner=%s production_call_edge=%s legacy_consumer_edge=%s capability_migration=%s coverage_status=%s coverage_scope=%s blocker=%s\n' \
            "$owner" "$production_edge" "$consumer_edge" "$migration" "$coverage" "$scope" "$blocker"
    done
    printf 'legacy_gate_owner_count=%s\n' "$owner_count"
    if [ "$owner_count" -ne 4 ]; then
        printf 'owner_audit_error=original_legacy_owner_gate_count expected=4 actual=%s\n' "$owner_count" >&2
        audit_failed=1
    fi
    if [ "$actual_roster" != "$expected_roster" ]; then
        printf 'owner_audit_error=original_legacy_owner_roster_mismatch expected=%s actual=%s\n' \
            "$expected_roster" "$actual_roster" >&2
        audit_failed=1
    fi
}

audit_owner_roster() {
    local owner_count=0
    local owner path_description source_file marker test_name execution_marker
    local actual_roster=''
    local expected_roster='DefaultBytecodeAstCompatibility,NativeCheckedCompatibilityBoundary,CheckedVerifierActorProofCompatibility,FfiVerifierOnlyOwnerSlice'
    local -a roster=()

    # This current compatibility-path roster supplements the original four
    # gate owners emitted by audit_original_legacy_owner_gates; it never
    # replaces or renames them.
    roster+=(
        'DefaultBytecodeAstCompatibility|default Legacy route in run/test/disasm; public raw-AST compiler API|tests/legacy_owner_boundary_api.rs|default_bytecode_compatibility_route_executes_its_named_legacy_disposition_without_artifacts|OWNER_ARTIFACTS_NONE=source-fixture-only|OWNER_PROBE_EXECUTED=default-bytecode-ast-compatibility'
        'NativeCheckedCompatibilityBoundary|public CheckedProgram API; canonical native lowering or explicit rejection, no raw-AST emitter|tests/legacy_owner_boundary_api.rs|native_checked_api_rejects_dynamic_protocol_receivers_without_main_artifact|OWNER_ARTIFACTS_NONE=dynamic-main-absent;OWNER_CANONICAL_ROUTE=checked-native-main-emitted|OWNER_PROBE_EXECUTED=native-checked-api'
        'CheckedVerifierActorProofCompatibility|single/dual CheckedProgram proof route for Actor proof obligations|tests/legacy_owner_boundary_api.rs|checked_verifier_actor_proof_compatibility_preserves_contracts_and_rejects_mir_without_artifacts|OWNER_ARTIFACTS_NONE=source-fixtures-only|OWNER_PROBE_EXECUTED=checked-verifier-actor-proof-route'
        'FfiVerifierOnlyOwnerSlice|exact isolated checked String-record FFI proof route; never an execution route|tests/legacy_owner_boundary_api.rs|ffi_verifier_only_owner_proves_and_defers_without_execution_or_file_artifacts|OWNER_ARTIFACTS_NONE=source-fixtures-only|OWNER_PROBE_EXECUTED=ffi-verifier-only-owner'
    )

    for row in "${roster[@]}"; do
        IFS='|' read -r owner path_description source_file test_name marker execution_marker <<< "$row"
        owner_count=$((owner_count + 1))
        actual_roster+="${actual_roster:+,}${owner}"
        if ! rg -q "^[[:space:]]*fn ${test_name}\\(" "$ROOT_DIR/$source_file"; then
            printf 'owner_audit_error=%s direct_route_probe_missing=%s::%s\n' \
                "$owner" "$source_file" "$test_name" >&2
            audit_failed=1
            continue
        fi
        if ! sed -n "/^[[:space:]]*fn ${test_name}(/,/^[[:space:]]*#\\[test\\]/p" \
            "$ROOT_DIR/$source_file" | rg -F "$marker" >/dev/null \
            || ! sed -n "/^[[:space:]]*fn ${test_name}(/,/^[[:space:]]*#\\[test\\]/p" \
                "$ROOT_DIR/$source_file" | rg -F "$execution_marker" >/dev/null; then
            printf 'owner_audit_error=%s direct_probe_contract_marker_missing=%s::%s artifact=%s execution=%s\n' \
                "$owner" "$source_file" "$test_name" "$marker" "$execution_marker" >&2
            audit_failed=1
            continue
        fi
        printf 'owner=%s reachability=production kind=compatibility-route-or-boundary description=%s probe_test=%s::%s artifact_assertion=%s execution_marker=%s\n' \
            "$owner" "$path_description" "$source_file" "$test_name" "$marker" "$execution_marker"
    done

    if [ "$owner_count" -ne 4 ]; then
        printf 'owner_audit_error=classified_production_path_count expected=4 actual=%s\n' "$owner_count" >&2
        audit_failed=1
    fi
    if [ "$actual_roster" != "$expected_roster" ]; then
        printf 'owner_audit_error=production_path_roster_mismatch expected=%s actual=%s\n' \
            "$expected_roster" "$actual_roster" >&2
        audit_failed=1
    fi

    if ! rg -Fq 'pub fn compile_file(' "$ROOT_DIR/src/interp/bytecode/compiler.rs" \
        || ! rg -q 'DefaultMirRoute::Legacy' "$ROOT_DIR/src/main/run.rs" \
        || ! rg -q 'DefaultMirRoute::Legacy' "$ROOT_DIR/src/main/test.rs" \
        || ! rg -q 'DefaultMirRoute::Legacy' "$ROOT_DIR/src/main/disasm_cmd.rs"; then
        printf 'owner_audit_error=default_bytecode_ast_compatibility_route_unbound\n' >&2
        audit_failed=1
    fi
    if ! rg -Fq 'pub fn compile_checked(' "$ROOT_DIR/src/codegen/compile.rs" \
        || rg -n 'BytecodeCompiler|\.compile_file\(' "$ROOT_DIR/src/main/build.rs" >/dev/null; then
        printf 'owner_audit_error=native_checked_boundary_route_unbound_or_reentered_bytecode\n' >&2
        audit_failed=1
    fi
    if ! rg -Fq 'pub fn verify_checked(' "$ROOT_DIR/src/verifier/mod.rs" \
        || ! rg -Fq 'pub fn verify_checked_dual(' "$ROOT_DIR/src/verifier/mod.rs" \
        || ! rg -q 'actor-proof-obligations-compatibility' "$ROOT_DIR/src/main/canonical_dispatch.rs"; then
        printf 'owner_audit_error=checked_verifier_actor_proof_route_unbound\n' >&2
        audit_failed=1
    fi
    if ! rg -q 'verifier-ffi-owner-slice-v1' "$ROOT_DIR/src/verifier/mod.rs" \
        || ! rg -q 'VerifierOnlyFfiStringRecordOwnerSlice' "$ROOT_DIR/src/main/canonical_dispatch.rs"; then
        printf 'owner_audit_error=ffi_verifier_only_owner_route_unbound\n' >&2
        audit_failed=1
    fi

    printf 'classified_production_path_count=%s\n' "$owner_count"
    printf 'reachable_legacy_ast_owner_count=1 owner=DefaultBytecodeAstCompatibility\n'
    printf 'reachable_checked_proof_compatibility_owner_count=2 owners=CheckedVerifierActorProofCompatibility,FfiVerifierOnlyOwnerSlice\n'
    printf 'checked_native_boundary_count=1 path=NativeCheckedCompatibilityBoundary kind=canonical-or-reject-no-AST-fallback\n'
    printf 'production_path_roster=%s\n' "$actual_roster"
}

audit_original_legacy_owner_gates
audit_owner_roster
printf 'owner_retirement_status=reachable-compatibility-surfaces-remain;route-and-boundary-evidence-only\n'

raw_ast_refs="$(rg -n \
    --glob '*.rs' \
    --glob '!**/tests.rs' \
    --glob '!src/tests/**' \
    '[[:alnum:]_)]\.raw_ast\(' \
    "$SRC_DIR" || true)"
raw_ast_count=0
if [ -n "$raw_ast_refs" ]; then
    raw_ast_count="$(printf '%s\n' "$raw_ast_refs" | wc -l)"
fi
printf 'production_raw_ast_call_sites=%s\n' "$raw_ast_count"
if [ "$raw_ast_count" -ne "$EXPECTED_PRODUCTION_RAW_AST_CALL_SITES" ]; then
    printf 'owner_audit_error=production_raw_ast_call_sites expected=%s actual=%s\n' \
        "$EXPECTED_PRODUCTION_RAW_AST_CALL_SITES" "$raw_ast_count" >&2
    audit_failed=1
fi

legacy_refs="$(rg -n \
    --glob '*.rs' \
    --glob '!**/tests.rs' \
    --glob '!src/tests/**' \
    'compile_func_legacy\(' \
    "$SRC_DIR" || true)"
legacy_count=0
if [ -n "$legacy_refs" ]; then
    legacy_count="$(printf '%s\n' "$legacy_refs" | wc -l)"
fi
legacy_call_sites="$(printf '%s\n' "$legacy_refs" | rg -v 'fn compile_func_legacy' || true)"
legacy_call_count=0
if [ -n "$legacy_call_sites" ]; then
    legacy_call_count="$(printf '%s\n' "$legacy_call_sites" | wc -l)"
fi
printf 'production_compile_func_legacy_call_sites=%s\n' "$legacy_call_count"
if [ "$legacy_call_count" -ne "$EXPECTED_PRODUCTION_COMPILE_FUNC_LEGACY_CALL_SITES" ]; then
    printf 'owner_audit_error=production_compile_func_legacy_call_sites expected=%s actual=%s\n' \
        "$EXPECTED_PRODUCTION_COMPILE_FUNC_LEGACY_CALL_SITES" "$legacy_call_count" >&2
    audit_failed=1
fi
native_ast_body_refs="$(rg -n \
    --glob '*.rs' \
    --glob '!**/tests.rs' \
    --glob '!src/tests/**' \
    'compile_file_with_resolved\(|compile_file_inner\(|compile_func_body\(|emit_implicit_return\(|compile_func_legacy\(|compile_generic_func\(' \
    "$ROOT_DIR/src/codegen" || true)"
if [ -n "$native_ast_body_refs" ]; then
    printf 'owner_audit_error=native_codegen_ast_body_refs_remain\n%s\n' \
        "$native_ast_body_refs" >&2
    audit_failed=1
fi
native_ast_test_refs="$(rg -n \
    --glob '*.rs' \
    'compile_file_with_resolved\(|compile_file_inner\(|compile_func_body\(|emit_implicit_return\(|compile_func_legacy\(|compile_generic_func\(' \
    "$SRC_DIR" || true)"
if [ -n "$native_ast_test_refs" ]; then
    printf 'owner_audit_error=native_codegen_ast_test_or_call_refs_remain\n%s\n' \
        "$native_ast_test_refs" >&2
    audit_failed=1
fi
printf 'native_codegen_ast_test_adapters=physically-absent old-body-call-refs=0\n'

if ! rg -Fq 'self.clear_partial_body(llvm_function)' "$ROOT_DIR/src/codegen/resolved/mod.rs" \
    || ! rg -Fq 'pub(crate) fn clear_partial_body' "$ROOT_DIR/src/codegen/func.rs"; then
    printf 'owner_audit_error=resolved_partial_body_cleanup_missing\n' >&2
    audit_failed=1
fi
printf 'resolved_partial_body_cleanup=clear-on-emission-failure consumer=src/codegen/resolved/mod.rs\n'

# R6-15 removed the former direct-expression-only scalar FFI admission helper.
# Keep its names in this audit so a future compatibility refactor cannot
# silently reintroduce a second FFI route policy.
scalar_ffi_legacy_refs="$(rg -n \
    --glob '*.rs' \
    --glob '!**/tests.rs' \
    --glob '!src/tests/**' \
    'contains_scalar_ffi_contract_candidate|direct_scalar_ffi_callees|scalar_ffi_contract_expr' \
    "$SRC_DIR" || true)"
scalar_ffi_legacy_count=0
if [ -n "$scalar_ffi_legacy_refs" ]; then
    scalar_ffi_legacy_count="$(printf '%s\n' "$scalar_ffi_legacy_refs" | wc -l)"
    printf '%s\n' "$scalar_ffi_legacy_refs"
fi
printf 'scalar_ffi_direct_expression_legacy_refs=%s\n' "$scalar_ffi_legacy_count"
if [ "$scalar_ffi_legacy_count" -ne "$EXPECTED_SCALAR_FFI_DIRECT_EXPRESSION_LEGACY_REFS" ]; then
    printf 'owner_audit_error=scalar_ffi_direct_expression_legacy_refs expected=%s actual=%s\n' \
        "$EXPECTED_SCALAR_FFI_DIRECT_EXPRESSION_LEGACY_REFS" "$scalar_ffi_legacy_count" >&2
    audit_failed=1
fi

if [ "$static_only" -eq 1 ]; then
    if [ "$audit_failed" -ne 0 ]; then
        printf 'static_audit_status=failed\n'
        exit 1
    fi
    printf 'static_audit_status=ok\n'
    exit 0
fi

run_focused_owner_probe() {
    local owner="$1"
    local probe_name="$2"
    local expected_outcome="$3"
    local artifact_assertion="$4"
    local expected_marker="$5"
    shift 5
    dynamic_probe_count=$((dynamic_probe_count + 1))
    dynamic_owner_seen["$owner"]=1
    if [ -n "${dynamic_probe_seen[$probe_name]:-}" ]; then
        printf 'owner_audit_error=duplicate_dynamic_probe name=%s owner=%s\n' \
            "$probe_name" "$owner" >&2
        audit_failed=1
    else
        dynamic_probe_seen["$probe_name"]="$owner"
    fi
    local probe_dir stdout_file stderr_file exit_file command_file context_file
    local result_line probe_exit probe_validation source_manifest_hash command_hash
    local tool_cargo tool_rustc tool_llvm_config tool_cc tool_z3_library probe_transcript
    local -a probe_command

    probe_dir="$AUDIT_EVIDENCE_DIR/$probe_name"
    mkdir -- "$probe_dir"
    stdout_file="$probe_dir/stdout.log"
    stderr_file="$probe_dir/stderr.log"
    exit_file="$probe_dir/exit-status.txt"
    command_file="$probe_dir/command.sh"
    context_file="$probe_dir/execution-context.txt"
    probe_command=(env -u RUSTFLAGS -u LD_PRELOAD LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper CARGO_TARGET_DIR="$canonical_target_dir" cargo test "$@" -- --exact --test-threads=1 --nocapture)

    {
        printf '#!/usr/bin/env bash\nset -euo pipefail\ncd -- %q\nexec' "$ROOT_DIR"
        printf ' %q' "${probe_command[@]}"
        printf '\n'
    } > "$command_file"
    chmod 700 "$command_file"
    write_probe_source_hashes "$probe_dir/source-hashes.sha256"
    command_hash="$(sha256sum -- "$command_file" | awk '{print $1}')"
    source_manifest_hash="$(cat "$probe_dir/source-hashes.sha256.sha256")"
    {
        printf 'probe=%s\n' "$probe_name"
        printf 'owner=%s\nexpected_outcome=%s\nartifact_assertion=%s\n' \
            "$owner" "$expected_outcome" "$artifact_assertion"
        printf 'started_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf 'repository=%s\n' "$ROOT_DIR"
        printf 'target_dir=%s\n' "$canonical_target_dir"
        printf 'git_head=%s\n' "$(git -C "$ROOT_DIR" rev-parse HEAD)"
        printf 'git_branch=%s\n' "$(git -C "$ROOT_DIR" branch --show-current)"
        printf 'git_source_status_sha256=%s\n' \
            "$(git -C "$ROOT_DIR" status --porcelain=v1 -- src tests std Cargo.toml Cargo.lock build.rs scripts/audit-mir-legacy-owners.sh | sha256sum | awk '{print $1}')"
        printf 'command_file=%s\ncommand_sha256=%s\n' "$command_file" "$command_hash"
        printf 'source_hash_manifest=%s\nsource_hash_manifest_sha256=%s\n' \
            "$probe_dir/source-hashes.sha256" "$source_manifest_hash"
        printf 'stdout_file=%s\nstderr_file=%s\n' "$stdout_file" "$stderr_file"
        printf 'RUSTFLAGS=unset\nLD_PRELOAD=unset\nLLVM_SYS_181_PREFIX=/tmp/llvm-wrapper\n'
        printf 'CARGO_TARGET_DIR=%s\n' "$canonical_target_dir"
        tool_cargo="$(command -v cargo)"
        tool_rustc="$(command -v rustc)"
        printf 'tool_cargo_path=%s\n' "$tool_cargo"
        printf 'tool_cargo_sha256=%s\n' "$(sha256sum -- "$tool_cargo" | awk '{print $1}')"
        printf 'tool_rustc_path=%s\n' "$tool_rustc"
        printf 'tool_rustc_sha256=%s\n' "$(sha256sum -- "$tool_rustc" | awk '{print $1}')"
        printf 'toolchain_rustc='
        rustc --version
        printf 'toolchain_cargo='
        cargo --version
        tool_llvm_config=/tmp/llvm-wrapper/bin/llvm-config
        printf 'tool_llvm_config_path=%s\n' "$tool_llvm_config"
        if [ -x "$tool_llvm_config" ]; then
            printf 'tool_llvm_config_sha256=%s\n' "$(sha256sum -- "$tool_llvm_config" | awk '{print $1}')"
            printf 'toolchain_llvm_config='
            "$tool_llvm_config" --version
        else
            printf 'tool_llvm_config_status=missing\n'
        fi
        tool_cc="$(command -v cc || true)"
        if [ -n "$tool_cc" ]; then
            printf 'tool_cc_path=%s\n' "$tool_cc"
            printf 'tool_cc_sha256=%s\n' "$(sha256sum -- "$tool_cc" | awk '{print $1}')"
            printf 'toolchain_cc='
            "$tool_cc" --version | sed -n '1p'
        else
            printf 'tool_cc_status=missing\n'
        fi
        tool_z3_library="$(ldconfig -p 2>/dev/null | awk 'index($0, "libz3.so (") && !found { path = $NF; found = 1 } END { if (found) print path }')"
        if [ -n "$tool_z3_library" ] && [ -f "$tool_z3_library" ]; then
            printf 'tool_z3_library_path=%s\n' "$tool_z3_library"
            printf 'tool_z3_library_sha256=%s\n' "$(sha256sum -- "$tool_z3_library" | awk '{print $1}')"
        else
            printf 'tool_z3_library_status=not-found-in-ldconfig\n'
        fi
    } > "$context_file" 2>&1

    printf 'dynamic_probe_start=%s\n' "$probe_name"
    if "${probe_command[@]}" > "$stdout_file" 2> "$stderr_file"; then
        probe_exit=0
    else
        probe_exit=$?
    fi
    printf '%s\n' "$probe_exit" > "$exit_file"
    result_line="$( { rg '^test result:' "$stdout_file" || true; rg '^test result:' "$stderr_file" || true; } | tail -n 1 )"
    probe_transcript="$probe_dir/combined-output.log"
    cat -- "$stdout_file" "$stderr_file" > "$probe_transcript"
    if [ "$probe_exit" -eq 0 ] && [[ "$result_line" == *"1 passed; 0 failed"* ]] \
        && ! rg -F 'skipped (Z3 not available)' "$probe_transcript" >/dev/null \
        && ! rg -F 'SKIP: Z3 not available' "$probe_transcript" >/dev/null \
        && rg -F "$expected_marker" "$probe_transcript" >/dev/null \
        && rg -F "$artifact_assertion" "$probe_transcript" >/dev/null; then
        probe_validation=passed
        printf 'dynamic_probe_status=passed owner=%s probe=%s exit=%s summary=%s artifacts=%s evidence=%s command_sha256=%s source_sha256=%s\n' \
            "$owner" "$probe_name" "$probe_exit" "$result_line" "$artifact_assertion" "$probe_dir" \
            "$command_hash" "$source_manifest_hash"
    else
        probe_validation=failed
        audit_failed=1
        printf 'dynamic_probe_status=failed owner=%s probe=%s exit=%s validation=exactly-one-executed-passing-test-and-execution+artifact-markers summary=%s execution_marker=%s artifact_marker=%s evidence=%s command_sha256=%s source_sha256=%s\n' \
            "$owner" "$probe_name" "$probe_exit" "${result_line:-missing-test-result}" "$expected_marker" "$artifact_assertion" "$probe_dir" \
            "$command_hash" "$source_manifest_hash" >&2
    fi
    {
        printf 'cargo_exit_status=%s\n' "$probe_exit"
        printf 'probe_validation=%s\n' "$probe_validation"
        printf 'owner=%s\nexpected_outcome=%s\nartifact_assertion=%s\nrequired_execution_marker=%s\n' \
            "$owner" "$expected_outcome" "$artifact_assertion" "$expected_marker"
        printf 'test_result=%s\n' "${result_line:-missing-test-result}"
        printf 'probe_transcript_sha256=%s\n' "$(sha256sum -- "$probe_transcript" | awk '{print $1}')"
        printf 'finished_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    } > "$probe_dir/result.txt"
}

dynamic_probe_count=0
declare -A dynamic_owner_seen=()
declare -A dynamic_probe_seen=()

run_focused_owner_probe \
    DefaultBytecodeAstCompatibility \
    bytecode-default-legacy-route \
    'outside-profile default run succeeds and explicit MIR rejects without an output artifact' \
    'OWNER_ARTIFACTS_NONE=source-fixture-only' \
    'OWNER_PROBE_EXECUTED=default-bytecode-ast-compatibility' \
    --features llvm18-host-dynamic --test legacy_owner_boundary_api \
    default_bytecode_compatibility_route_executes_its_named_legacy_disposition_without_artifacts

run_focused_owner_probe \
    DefaultBytecodeAstCompatibility \
    bytecode-ast-direct-api-dyn-rejection \
    'direct raw-AST compile rejects dynamic Protocol field and expression receivers' \
    'OWNER_ARTIFACTS_NONE=bytecode-program-not-returned' \
    'OWNER_PROBE_EXECUTED=bytecode-ast-direct-api' \
    --features llvm18-host-dynamic --test legacy_owner_boundary_api \
    bytecode_ast_compatibility_direct_api_rejects_dynamic_protocol_receivers

run_focused_owner_probe \
    NativeCheckedCompatibilityBoundary \
    native-checked-direct-api-dyn-rejection \
    'direct checked native compile rejects dynamic Protocol receivers without emitting main' \
    'OWNER_ARTIFACTS_NONE=dynamic-main-absent;OWNER_CANONICAL_ROUTE=checked-native-main-emitted' \
    'OWNER_PROBE_EXECUTED=native-checked-api' \
    --features llvm18-host-dynamic --test legacy_owner_boundary_api \
    native_checked_api_rejects_dynamic_protocol_receivers_without_main_artifact

run_focused_owner_probe \
    NativeCheckedCompatibilityBoundary \
    default-cli-dyn-field-expression-rejection \
    'default and explicit CLI routes reject both dynamic receiver shapes; build artifacts are absent' \
    'OWNER_ARTIFACTS_NONE=source-fixtures-only' \
    'OWNER_PROBE_EXECUTED=default-cli-dynamic-protocol-routes' \
    --features llvm18-host-dynamic --test legacy_owner_boundary_api \
    default_and_explicit_routes_reject_dynamic_protocol_receiver_shapes_without_artifacts

run_focused_owner_probe \
    CheckedVerifierActorProofCompatibility \
    actor-proof-checked-compatibility-route \
    'default checked verifier preserves proven/disproven Actor-adjacent top-level contracts; explicit MIR rejects' \
    'OWNER_ARTIFACTS_NONE=source-fixtures-only' \
    'OWNER_PROBE_EXECUTED=checked-verifier-actor-proof-route' \
    --features llvm18-host-dynamic --test legacy_owner_boundary_api \
    checked_verifier_actor_proof_compatibility_preserves_contracts_and_rejects_mir_without_artifacts

run_focused_owner_probe \
    CheckedVerifierActorProofCompatibility \
    checked-verifier-direct-api-dyn-rejection \
    'single, dual, and configured source verifier APIs reject both dynamic receiver identities without proof outputs' \
    'OWNER_ARTIFACTS_NONE=all-apis-returned-errors' \
    'OWNER_PROBE_EXECUTED=checked-verifier-actor-proof-api' \
    --features llvm18-host-dynamic --test legacy_owner_boundary_api \
    checked_verifier_apis_reject_dynamic_protocol_receivers_without_proof_results

run_focused_owner_probe \
    FfiVerifierOnlyOwnerSlice \
    ffi-verifier-only-direct-and-default-route \
    'isolated FFI owner proves/disproves, preserves unsupported NITS without artifact, and cannot execute' \
    'OWNER_ARTIFACTS_NONE=source-fixtures-only;PROOF_ARTIFACTS=expected-for-proven-and-disproven-only' \
    'OWNER_PROBE_EXECUTED=ffi-verifier-only-owner' \
    --features llvm18-host-dynamic --test legacy_owner_boundary_api \
    ffi_verifier_only_owner_proves_and_defers_without_execution_or_file_artifacts

expected_dynamic_probes=(
    bytecode-default-legacy-route
    bytecode-ast-direct-api-dyn-rejection
    native-checked-direct-api-dyn-rejection
    default-cli-dyn-field-expression-rejection
    actor-proof-checked-compatibility-route
    checked-verifier-direct-api-dyn-rejection
    ffi-verifier-only-direct-and-default-route
)
for expected_probe in "${expected_dynamic_probes[@]}"; do
    if [ -z "${dynamic_probe_seen[$expected_probe]:-}" ]; then
        printf 'owner_audit_error=required_dynamic_probe_missing name=%s\n' \
            "$expected_probe" >&2
        audit_failed=1
    fi
done
if [ "$dynamic_probe_count" -ne "${#expected_dynamic_probes[@]}" ]; then
    printf 'owner_audit_error=dynamic_probe_count expected=%s actual=%s\n' \
        "${#expected_dynamic_probes[@]}" "$dynamic_probe_count" >&2
    audit_failed=1
fi
if [ "${#dynamic_probe_seen[@]}" -ne "${#expected_dynamic_probes[@]}" ]; then
    printf 'owner_audit_error=dynamic_probe_name_coverage expected=%s actual=%s\n' \
        "${#expected_dynamic_probes[@]}" "${#dynamic_probe_seen[@]}" >&2
    audit_failed=1
fi
if [ "${#dynamic_owner_seen[@]}" -ne 4 ]; then
    printf 'owner_audit_error=dynamic_probe_path_coverage expected=4 actual=%s\n' \
        "${#dynamic_owner_seen[@]}" >&2
    audit_failed=1
fi
printf 'dynamic_probe_status=executed probes=%s classified_paths=%s required_names=%s\n' \
    "$dynamic_probe_count" "${#dynamic_owner_seen[@]}" "${#expected_dynamic_probes[@]}"

if [ "$audit_failed" -ne 0 ]; then
    printf 'route_boundary_audit_status=failed\n'
    exit 1
fi
printf 'route_boundary_audit_status=passed\n'
