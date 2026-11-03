#!/usr/bin/env bash
# Emit a reproducible static inventory of the retained surface-body owners.
# Dynamic zero-access probes live in the Rust test suite; this script keeps the
# production call-site count and owner labels auditable without parsing output
# from a compiler build.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="$ROOT_DIR/src"

# Production code must not enter any retained surface-body owner.  The
# compatibility adapters remain available to the cfg(test) module only; the
# production inventory below excludes that module explicitly and reports its
# references separately.  A new production accessor or legacy call therefore
# fails this audit instead of silently widening the raw-AST surface.
readonly EXPECTED_PRODUCTION_OWNER_ACCESSOR_CALL_SITES=0
readonly EXPECTED_PRODUCTION_LEGACY_BODY_CALL_SITES=0
readonly EXPECTED_PRODUCTION_RAW_AST_CALL_SITES=0
readonly EXPECTED_PRODUCTION_COMPILE_FUNC_LEGACY_CALL_SITES=0
readonly EXPECTED_PRODUCTION_COMPILE_GENERIC_FUNC_CALL_SITES=0
readonly EXPECTED_SCALAR_FFI_DIRECT_EXPRESSION_LEGACY_REFS=0
# The deletion conditions are part of the compatibility contract. Keep a
# digest beside each prose condition so scope changes are detected by the
# gate and reviewed together with the migration that changes the condition.
readonly EXPECTED_CODEGEN_OWNER_CONDITION_DIGEST=fc5d620ce5d0ca2182f85bcc629ab0729cd0f162b511cfccd0b6bf9bb682a7b3
readonly EXPECTED_FFI_VERIFIER_OWNER_CONDITION_DIGEST=aa3f8f0d35d38b92bcc71fde640abfa53989bfa506c43d8964c70e4c5a211d95
readonly EXPECTED_OWNER_CONDITION_SET_DIGEST=76abec5ddfe605f4a2bd791b1871b748ca3f42cb4b7a1bd7ab23020df60988f3
audit_failed=0
closed_scalar_marker_sequence=()

emit_closed_scalar_marker() {
    local marker="$1"
    closed_scalar_marker_sequence+=("$marker")
    printf '%s\n' "$marker"
}

printf 'schema=canonical-mir-legacy-owner-audit-v1\n'
printf 'root=%s\n' "$ROOT_DIR"

# Keep one executable evidence case for each retired production owner.  Every
# case proves either a canonical route or an explicit MIR-COVERAGE-001/NITS
# rejection and an empty legacy-access receipt. Removing that test without a
# replacement must fail this audit rather than silently erasing migration
# evidence.
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
    'Vec::<crate::core::LegacyBodyConsumer>::new()'
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
retired_flow_dual_owner_evidence() {
    local verifier_dir="$SRC_DIR/verifier"
    local parity_tests="$verifier_dir/flow_retirement_tests.rs"
    local scalar transition kinds body test_name

    if [ -e "$verifier_dir/flow.rs" ]; then
        printf 'owner_audit_error=retired_flow_ast_owner_file_still_exists\n' >&2
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
            printf 'owner_audit_error=retired_flow_dual_symbol_remains marker=%s\n' "$marker" >&2
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
    printf 'retired_flow_dual_owners=physical;ast-owner=deleted;file-adapter=deleted;transition-vir-adapter=deleted;MIR-positive-negative-single-dual-evidence=%s+%s+%s\n' \
        "$scalar" "$transition" "$kinds"
}

retired_flow_dual_owner_evidence

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
        'test_legacy_body_access().is_empty()' \
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
        'cross-specialization receiver ABI drift must fail closed' \
        'test_legacy_body_access().is_empty()'; do
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

# The retained body accessor and AST adapters are deliberately test-only. Keep
# their location explicit so moving one back into production cannot be hidden
# by the production exclusion used by the call-site inventory.
test_only_legacy_body_adapter_context() {
    local context
    if ! rg -U -q '#\[cfg\(test\)\]\n    pub\(crate\) fn legacy_body_file\(' \
        "$ROOT_DIR/src/core/resolved/mod.rs"; then
        printf 'owner_audit_error=legacy_body_file_accessor_lost_cfg_test\n' >&2
        audit_failed=1
        return
    fi
    if ! rg -U -q '#\[cfg\(test\)\]\nmod tests;' "$ROOT_DIR/src/codegen/mod.rs"; then
        printf 'owner_audit_error=test_only_codegen_adapter_module_not_cfg_test\n' >&2
        audit_failed=1
        return
    fi
    context="$(sed -n '/pub(crate) fn legacy_body_file(/,/pub(crate) fn has_imports(/p' \
        "$ROOT_DIR/src/core/resolved/mod.rs")"
    if ! printf '%s\n' "$context" | rg -F 'Arc::clone(&self.legacy_file)' >/dev/null; then
        printf 'owner_audit_error=test_only_legacy_body_accessor_context_missing\n' >&2
        audit_failed=1
        return
    fi
    printf 'test_only_legacy_body_adapter_context=core::CheckedProgram::legacy_body_file+codegen::tests\n'
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

test_only_legacy_body_adapter_context

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

# Native Codegen's retained surface-file adapter is test-only. The Bytecode
# source compiler is a separate, explicitly named compatibility consumer for
# `run`/`test`/`disasm`: each production call must sit behind the dispatcher’s
# `DefaultMirRoute::Legacy` arm, while the native build has no such ingress.
# Keep this distinction visible so removing LLVM legacy edges is not confused
# with silently deleting the interpreter's graph-outside compatibility route.
codegen_legacy_consumer_boundary() {
    if ! rg -U -q '#\[cfg\(test\)\]\n    pub\(crate\) fn compile_file\(' \
        "$ROOT_DIR/src/codegen/compile.rs"; then
        printf 'owner_audit_error=codegen_compile_file_not_cfg_test\n' >&2
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
    printf 'codegen_legacy_consumer_boundary=llvm_compile_file_cfg_test;native_build_no_bytecode_legacy;run-test-disasm_explicit_legacy_route\n'
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
    boundary_line="$(printf '%s\n' "$context" | rg -n 'scalar_ffi_boundary_reason\(program\)' | head -n1 | cut -d: -f1)"
    route_line="$(printf '%s\n' "$context" | rg -n 'try_compile_exact_migrated_mir_island\(program\)' | head -n1 | cut -d: -f1)"
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
    local tail_expression
    local structure_line
    local begin_line
    local worker_symbol_line
    local normal_symbol_line
    local worker_execute_line
    local normal_execute_line
    structural_context="$(sed -n '/fn preflight_canonical_program_structure(/,/^    }/p' "$vm")"
    tail_expression="$(printf '%s\n' "$structural_context" \
        | awk 'NF { previous = last; last = $0 } END { print previous }' \
        | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
    if [ -z "$structural_context" ] \
        || [ "$tail_expression" != 'self.validate_canonical_ffi_program()' ]; then
        printf 'owner_audit_error=bytecode_vm_ffi_preflight_marker_missing marker=preflight_canonical_program_structure-tail-self.validate_canonical_ffi_program()\n' >&2
        audit_failed=1
        return
    fi
    execution_context="$(sed -n '/fn with_canonical_actor_execution/,/Counters are observational/p' "$vm")"
    structure_line="$(printf '%s\n' "$execution_context" | awk 'index($0, "self.preflight_canonical_program_structure()?") { print NR; exit }')"
    begin_line="$(printf '%s\n' "$execution_context" | awk 'index($0, ".begin_execution(&self.program, self.max_children)?") { print NR; exit }')"
    worker_symbol_line="$(printf '%s\n' "$execution_context" | awk 'index($0, "self.preflight_canonical_ffi_symbols()?") { print NR; exit }')"
    normal_symbol_line="$(printf '%s\n' "$execution_context" | awk 'index($0, "match self.preflight_canonical_ffi_symbols()") { print NR; exit }')"
    worker_execute_line="$(printf '%s\n' "$execution_context" | awk -v symbol_line="$worker_symbol_line" 'NR > symbol_line && index($0, "return execute(self);") { print NR; exit }')"
    normal_execute_line="$(printf '%s\n' "$execution_context" | awk -v symbol_line="$normal_symbol_line" 'NR > symbol_line && index($0, "Ok(()) => execute(self),") { print NR; exit }')"
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
    printf 'bytecode_vm_ffi_preflight_order=canonical-receipts-before-actor-root+host-symbols-before-execution\n'
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

# The retained AST snapshot is a test-only adapter. Canonical lowering now
# uses CheckedProgram::source_registry() for NodeId construction; no
# production source may read `legacy_file` directly. Keep the test-only
# accessor shape checked separately so an accidental cfg removal fails closed.
direct_legacy_file_reads_are_owner_only() {
    local reads unapproved
    reads="$(rg -n '\.legacy_file\b' "$SRC_DIR" \
        --glob '*.rs' \
        --glob '!**/tests.rs' \
        --glob '!**/test.rs' \
        --glob '!src/tests/**' || true)"
    unapproved="$(printf '%s\n' "$reads" | rg -v \
        'src/core/resolved/mod\.rs:[0-9]+: *Arc::clone\(&self\.legacy_file\)$' || true)"
    if [ -n "$unapproved" ]; then
        printf 'owner_audit_error=production_direct_legacy_file_reads\n%s\n' \
            "$unapproved" >&2
        audit_failed=1
        return
    fi
    if ! rg -U -q '#\[cfg\(test\)\]\n    pub\(crate\) fn legacy_body_file\(' \
        "$ROOT_DIR/src/core/resolved/mod.rs"; then
        printf 'owner_audit_error=legacy_body_file_accessor_lost_cfg_test\n' >&2
        audit_failed=1
        return
    fi
    printf 'production_direct_legacy_file_accessors=0 source-metadata-reads=0 body_bypasses=0\n'
    printf 'test_only_legacy_file_accessor=src/core/resolved/mod.rs::legacy_body_file\n'
}

direct_legacy_file_reads_are_owner_only

checked_program_legacy_file_residency() {
    local source="$ROOT_DIR/src/core/resolved/mod.rs"
    local checked_program_context
    checked_program_context="$(sed -n '/^pub struct CheckedProgram {/,/^}/p' "$source")"
    if ! printf '%s\n' "$checked_program_context" \
        | rg -U '#\[cfg\(test\)\]\s+legacy_file:\s*Arc<File>,' >/dev/null; then
        printf 'owner_audit_error=checked_program_legacy_file_field_lost_cfg_test\n' >&2
        audit_failed=1
        return
    fi
    if ! rg -U '#\[cfg\(test\)\]\s+let legacy_file_clone = File\s*\{\s*[^}]*\};' \
        "$source" >/dev/null; then
        printf 'owner_audit_error=checked_program_legacy_file_construction_lost_cfg_test\n' >&2
        audit_failed=1
        return
    fi
    if ! rg -U '#\[cfg\(test\)\]\s+legacy_file:\s*Arc::new\(legacy_file_clone\),' \
        "$source" >/dev/null; then
        printf 'owner_audit_error=checked_program_legacy_file_initializer_lost_cfg_test\n' >&2
        audit_failed=1
        return
    fi
    printf 'checked_program_legacy_file_residency=field+construction+initializer-cfg-test-only\n'
}

checked_program_legacy_file_residency

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
        # The FFI verifier selects one of two receipt labels at runtime. The
        # branch mapping is checked separately below; here bind that selected
        # label to the canonical graph that the receipt actually describes.
        receipt_pattern='canonical.route_receipt(receipt_profile)'
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
    'verify_ffi_mir_with_route_receipt(&canonical, &receipt, source_hash)' \
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
    local context
    local expected_mapping
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "CanonicalMirRouteProfile::${profile_name}" >/dev/null; then
        printf 'owner_audit_error=consumer_receipt_profile_missing=%s::%s label=%s profile=%s\n' \
            "$source_file" "$start_function" "$receipt_label" "$profile_name" >&2
        audit_failed=1
        return
    fi
    expected_mapping='let receipt_profile = "verify-ffi-v1";'
    if ! printf '%s\n' "$context" | rg -U -F "$expected_mapping" >/dev/null; then
        printf 'owner_audit_error=consumer_receipt_profile_mapping_missing=%s::%s expected=verify-ffi-v1-on-admitted-scalar-ffi\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F 'canonical.route_receipt(receipt_profile)' >/dev/null; then
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
    local context
    context="$(sed -n '/^fn verify_isolated_ffi_callers(/,/^fn order_ffi_contract_sites_by_identity(/p' "$ROOT_DIR/src/verifier/mod.rs")"
    for pattern in \
        'canonical.route_receipt("verifier-ffi-owner-slice-v1")' \
        'canonical.validate_ffi_source_census()' \
        'ResolvedFfiContractRoot::Ready { node_id }' \
        'contract.ensures_root.as_ref() == Some(node_id)' \
        'selected_ensures.insert(instruction.clone())' \
        'order_ffi_contract_sites_by_identity(' \
        'verify_ffi_mir_with_selected_ensures_route_receipt(' \
        'modeled_ensures_sites.extend(selected_ensures_sites)'; do
        if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=owner_slice_ffi_receipt_binding_missing=src/verifier/mod.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'owner_slice_ffi_receipt_binding=verifier-ffi-owner-slice-v1+Requires+selected-Ready-Ensures+phase-identity+checked-source-census consumer=src/verifier/mod.rs::verify_isolated_ffi_callers(\n'
}

owner_slice_ffi_receipt_binding

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
    adapter_context="$(sed -n '/^pub fn compile_mir_program_with_route_receipt(/,/^pub fn compile_mir_program_with_route_manifest(/p' "$source")"
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
    profile_line="$(printf '%s\n' "$adapter_context" | rg -n -m1 -U "$direct_print_profile_pattern" | cut -d: -f1 || true)"
    validator_line="$(printf '%s\n' "$adapter_context" | rg -n -m1 -U "$direct_print_validator_pattern" | cut -d: -f1 || true)"
    token_line="$(printf '%s\n' "$adapter_context" | rg -n -m1 -U "$direct_print_token_pattern" | cut -d: -f1 || true)"
    receipt_line="$(printf '%s\n' "$adapter_context" | rg -n -m1 -U "$receipt_validation_pattern" | cut -d: -f1 || true)"
    handoff_line="$(printf '%s\n' "$adapter_context" | rg -n -m1 -U "$inner_handoff_pattern" | cut -d: -f1 || true)"
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
    adapter_context="$(sed -n "/^pub fn ${adapter}(/,/^${next_marker}/p" "$ROOT_DIR/src/interp/bytecode/mir.rs")"
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

    printf 'bytecode_ordinary_map_owned_any_v2_route_binding=19-dedicated-adapters->ValidatedOrdinaryMapOwnedAnyV2Route-third-argument+exact-profile-guard consumer=src/interp/bytecode/mir.rs::compile_mir_program_inner(\n'
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
    local main_profile_context main_route_context materialization_context preflight_context
    local native_context native_receipt_adapter checked_native_receipt_adapter build_route_context
    local profile_pattern observer_pattern anchor_pattern main_profile_pattern main_route_pattern
    inner_context="$(sed -n '/^fn compile_mir_program_inner(/,/^fn materialize_canonical_ffi_bindings(/p' "$source")"
    observer_nodes_context="$(printf '%s\n' "$inner_context" | sed -n '/let has_observer_mir_nodes =/,/let ordinary_map_owner_observer_v1_receipts =/p')"
    observer_anchor_context="$(printf '%s\n' "$inner_context" | sed -n '/if let (Some(route_receipt), Some(checker_receipts)) = (/,/observer_bindings,/p')"
    profile_context="$(sed -n '/^impl CanonicalMirRouteProfile {/,/^    \/\/\/ Return whether checker-owned/p' "$route_source")"
    main_profile_context="$(sed -n '/^pub(crate) fn route_receipt_profile_for_checked(/,/^}/p' "$dispatcher")"
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
    main_profile_pattern='let admission\s*=\s*mimi::core::mir::classify_canonical_mir_route_admission\(checked\);\s*if mimi::core::mir::has_ordinary_map_set_get_i32_route_candidate\(checked\)\s*\{\s*return if default_profile == "verify-canonical-v1"\s*\{\s*mimi::core::mir::ORDINARY_MAP_SET_GET_I32_L2_VERIFIER_PROFILE\s*\}\s*else\s*\{\s*mimi::core::mir::ORDINARY_MAP_SET_GET_I32_EXECUTION_PROFILE\s*\};\s*\}\s*if mimi::core::mir::has_ordinary_map_set_result_route_candidate\(checked\)\s*\{\s*return if default_profile == "verify-canonical-v1"\s*\{\s*mimi::core::mir::ORDINARY_MAP_SET_RESULT_L2_VERIFIER_PROFILE\s*\}\s*else\s*\{\s*mimi::core::mir::ORDINARY_MAP_SET_RESULT_EXECUTION_PROFILE\s*\};\s*\}\s*if mimi::core::mir::has_ordinary_map_new_result_route_candidate\(checked\)\s*\{\s*return mimi::core::mir::ORDINARY_MAP_NEW_RESULT_EXECUTION_PROFILE;\s*\}\s*if admission\.ordinary_map_owned_any_v2_persistent_root_alias_replacement\(\)\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE;\s*\}\s*if admission\.ordinary_map_owned_any_v2_get_or_default_string\(\)\s*\{\s*return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE;\s*}'
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
    selector_profile_count="$(printf '%s\n' "$main_profile_context" | rg -U --count-matches 'MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE|ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE|OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1|OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller([^[:alnum:]_])|OrdinaryMapOwnedAnyV2ValuesProducerCaller|OrdinaryMapOwnedAnyV2MapRootGetAnyCaller|OrdinaryMapOwnedAnyV2MapRootProducerCaller|ORDINARY_MAP_OWNED_ANY_V2_(GET_OR_DEFAULT_I32_PROFILE|GET_OR_DEFAULT_STRING_PROFILE|REMOVED_KEY_CHECKED_MISS_PROFILE|ZERO_MISS_I32_PROFILE|KEYS_SNAPSHOT_PROFILE|DYNAMIC_KEYS_GET_PROFILE|IS_EMPTY_PROFILE|RECORD_UNIT_PROFILE|MAP_SET_DROP_CONTROL_PROFILE|BRANCH_LOCAL_SET_DIAMOND_PROFILE|CLONE_ISOLATION_PROFILE|PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE|DIRECT_PRINT_PROFILE|CHECKED_GET_PROFILE|COMPUTED_KEY_GET_PROFILE|COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE|VALUES_FIELD_MOVE_PROFILE|VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE|VALUES_LOCAL_INDEX_I32_PROFILE|VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE|VALUES_SNAPSHOT_REPLACEMENT_PROFILE|VALUES_MIXED_SNAPSHOT_PROFILE)' || printf '0')"
    local values_crosscall_v1_selector_count
    local values_crosscall_v2_selector_count
    values_crosscall_v1_selector_count="$(printf '%s\n' "$main_profile_context" | rg -U --count-matches 'OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller\s*\.as_str\(\)' || printf '0')"
    values_crosscall_v2_selector_count="$(printf '%s\n' "$main_profile_context" | rg -U --count-matches 'OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1\s*\.as_str\(\)' || printf '0')"
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
        || ! printf '%s\n' "$main_profile_context" | rg -U "$main_profile_pattern" >/dev/null \
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
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$parameter_pattern" >/dev/null; then
        printf 'owner_audit_error=source_hash_parameter_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$invocation_pattern" >/dev/null; then
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
    'verify_ffi_mir_with_route_receipt(&canonical, &receipt, source_hash)'

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
    capability_line="$(printf '%s\n' "$context" | rg -n -F "$capability_pattern" | head -n 1 | cut -d: -f1 || true)"
    execution_line="$(printf '%s\n' "$context" | rg -n -F "$execution_pattern" | head -n 1 | cut -d: -f1 || true)"
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
    receipt_line="$(printf '%s\n' "$context" | rg -n -F "$receipt_pattern" | head -n 1 | cut -d: -f1 || true)"
    adapter_line="$(printf '%s\n' "$context" | rg -n -F "$adapter_pattern" | head -n 1 | cut -d: -f1 || true)"
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
        "$ROOT_DIR/src/tests/canonical_scalar_ffi.rs" | rg 'test_legacy_body_access\(\)\.is_empty\(\)' >/dev/null; then
        printf 'owner_audit_error=closed_scalar_zero_owner_evidence_missing_empty_legacy_assertion\n' >&2
        audit_failed=1
    else
        emit_closed_scalar_marker 'closed_scalar_zero_owner_evidence_marker=test_legacy_body_access().is_empty()'
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
    'closed_scalar_zero_owner_evidence_marker=test_legacy_body_access().is_empty()'
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
    'MIR_ROUTE_COVERAGE_ERROR_CODE' \
    'test_legacy_body_access().is_empty()'
require_codegen_closed_route_tripwire \
    flow-transition \
    compile_checked_routes_multi_transition_flow_through_canonical_mir \
    'test_legacy_body_access().is_empty()' \
    'canonical multi-transition Flow must emit main'
require_codegen_closed_route_tripwire \
    trait-impl-specialization \
    compile_checked_rejects_trait_impl_specialization_without_legacy_owner \
    'MIR_ROUTE_COVERAGE_ERROR_CODE' \
    'test_legacy_body_access().is_empty()'
require_codegen_closed_route_tripwire \
    nested-function-body \
    compile_checked_routes_captured_nested_helper_without_legacy_owner \
    'nested_callable_scopes().len()' \
    'canonical MIR default route' \
    'test_legacy_body_access().is_empty()' \
    'test_legacy_codegen_function_calls().is_empty()'
require_codegen_closed_route_tripwire \
    export-wrapper \
    compile_checked_routes_scalar_export_wrapper_without_legacy_access \
    'test_legacy_body_access().is_empty()' \
    'owner_tripwire_export'
require_codegen_closed_route_tripwire \
    actor-method \
    compile_checked_routes_scalar_actor_through_canonical_mir_without_legacy_access \
    'CanonicalMirRouteProfile::ScalarActor' \
    'Vec::<crate::core::LegacyBodyConsumer>::new()' \
    'OwnerTripwireWorker__read__method'
require_codegen_closed_route_tripwire \
    actor-method-rejected \
    compile_checked_rejects_contract_bearing_actor_without_legacy_owner \
    'MIR_ROUTE_COVERAGE_ERROR_CODE' \
    'test_legacy_body_access().is_empty()'
require_codegen_closed_route_tripwire \
    map-any-owned \
    compile_checked_routes_closed_map_any_owner_v1_without_legacy_access \
    'test_legacy_body_access().is_empty()' \
    'MapAnyOwnerAdmission::CompleteCoverage'
require_codegen_closed_route_tripwire \
    map-any-rejected \
    compile_checked_rejects_map_any_borrowed_access_without_legacy_owner \
    'MIR_ROUTE_COVERAGE_ERROR_CODE' \
    'test_legacy_body_access().is_empty()'
require_direct_mir_no_legacy_tripwire() {
    local test_name="$1"
    local body
    body="$(sed -n "/^[[:space:]]*fn ${test_name}(/,/^[[:space:]]*#\\[test\\]/p" \
        "$ROOT_DIR/src/codegen/tests.rs")"
    if [ -z "$body" ]; then
        printf 'owner_audit_error=missing_direct_mir_no_legacy_tripwire test=%s\n' \
            "$test_name" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$body" | rg 'reset_test_legacy_body_access\(\)' >/dev/null; then
        printf 'owner_audit_error=direct_mir_no_legacy_tripwire_missing_reset test=%s\n' \
            "$test_name" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$body" | rg 'test_legacy_body_access\(\)\.is_empty\(\)' >/dev/null; then
        printf 'owner_audit_error=direct_mir_no_legacy_tripwire_missing_empty_assertion test=%s\n' \
            "$test_name" >&2
        audit_failed=1
        return
    fi
    printf 'direct_mir_no_legacy_tripwire=%s\n' "$test_name"
}

require_direct_mir_no_legacy_tripwire \
    compile_checked_routes_exact_s8_flow_through_canonical_mir

owner_count=0
condition_inventory=''
for owner in \
    CodegenLegacyRemainder \
    FfiVerifierCompatibility; do
    owner_count=$((owner_count + 1))
    matches="$(rg -n \
        --glob '*.rs' \
        --glob '!**/tests.rs' \
        --glob '!src/tests/**' \
        "legacy_body_file\\([^)]*LegacyBodyConsumer::${owner}" \
        "$SRC_DIR" || true)"
    count=0
    if [ -n "$matches" ]; then
        count="$(printf '%s\n' "$matches" | wc -l)"
    fi
    printf 'owner=%s production_accessor_call_sites=%s\n' "$owner" "$count"
    if [ -n "$matches" ]; then
        printf '%s\n' "$matches"
    fi
    if [ "$count" -ne "$EXPECTED_PRODUCTION_OWNER_ACCESSOR_CALL_SITES" ]; then
        printf 'owner_audit_error=%s expected_accessor_call_sites=%s actual=%s\n' \
            "$owner" "$EXPECTED_PRODUCTION_OWNER_ACCESSOR_CALL_SITES" "$count" >&2
        audit_failed=1
    fi
    case "$owner" in
        CodegenLegacyRemainder)
            dependency_class='legacy-codegen-remainder'
            deletion_blocker='cfg(test) compatibility adapters and the surface body compiler remain retained'
            condition='delete after cfg(test) legacy codegen adapters and the surface body compiler are removed; the production owner is already closed'
            ;;
        FfiVerifierCompatibility)
            dependency_class='ffi-declaration-compatibility'
            deletion_blocker='canonical MIR proofs remain limited to admitted ScalarFfi and verifier-only owner slices; uncovered declarations and call-site contexts remain, with test-only AST oracles retained'
            condition='retire test oracles after canonical MIR receipts and proofs cover every FFI declaration and call-site obligation'
            ;;
    esac
    if [ "$count" -eq 0 ]; then
        production_access_status='sealed'
        accessor_blocker='no production accessor'
    else
        production_access_status='open'
        accessor_blocker='production accessor remains reachable'
    fi
    # Accessor reachability is only one deletion prerequisite. Keep the
    # readiness gate conservative until a migration explicitly proves the
    # owner-specific condition above; a zero accessor count alone must never
    # be reported as permission to delete the owner.
    deletion_ready=0
    condition_digest="$(printf '%s' "$condition" | sha256sum | awk '{print $1}')"
    case "$owner" in
        CodegenLegacyRemainder)
            expected_condition_digest="$EXPECTED_CODEGEN_OWNER_CONDITION_DIGEST"
            ;;
        FfiVerifierCompatibility)
            expected_condition_digest="$EXPECTED_FFI_VERIFIER_OWNER_CONDITION_DIGEST"
            ;;
    esac
    printf 'owner=%s owner_deletion_condition_digest=%s\n' "$owner" "$condition_digest"
    if [ "$condition_digest" != "$expected_condition_digest" ]; then
        printf 'owner_audit_error=%s owner_deletion_condition_digest expected=%s actual=%s\n' \
            "$owner" "$expected_condition_digest" "$condition_digest" >&2
        audit_failed=1
    fi
    condition_inventory+="${owner}=${condition_digest}"$'\n'
    printf 'owner=%s production_access_status=%s dependency_class=%s owner_deletion_ready=%s owner_deletion_blocker=%s; %s\n' \
        "$owner" "$production_access_status" "$dependency_class" "$deletion_ready" "$accessor_blocker" "$deletion_blocker"
    printf 'owner_deletion_condition=%s\n' "$condition"
done
condition_set_digest="$(printf '%s' "$condition_inventory" | sha256sum | awk '{print $1}')"
printf 'owner_deletion_condition_set_digest=%s\n' "$condition_set_digest"
if [ "$condition_set_digest" != "$EXPECTED_OWNER_CONDITION_SET_DIGEST" ]; then
    printf 'owner_audit_error=owner_deletion_condition_set_digest expected=%s actual=%s\n' \
        "$EXPECTED_OWNER_CONDITION_SET_DIGEST" "$condition_set_digest" >&2
    audit_failed=1
fi

body_refs="$(rg -n \
    --glob '*.rs' \
    --glob '!**/tests.rs' \
    --glob '!src/tests/**' \
    'legacy_body_file\(' \
    "$SRC_DIR" | rg -v ':[0-9]+:[[:space:]]*(//|.*fn legacy_body_file)' || true)"
body_count=0
if [ -n "$body_refs" ]; then
    body_count="$(printf '%s\n' "$body_refs" | wc -l)"
fi
printf 'production_legacy_body_file_call_sites=%s\n' "$body_count"
if [ "$body_count" -ne "$EXPECTED_PRODUCTION_LEGACY_BODY_CALL_SITES" ]; then
    printf 'owner_audit_error=production_legacy_body_file_call_sites expected=%s actual=%s\n' \
        "$EXPECTED_PRODUCTION_LEGACY_BODY_CALL_SITES" "$body_count" >&2
    audit_failed=1
fi

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
legacy_body_compiler_context="$(sed -n \
    '/Test-only legacy surface-AST function body compiler/,/^[[:space:]]*fn compile_func_legacy_clean(/p' \
    "$ROOT_DIR/src/codegen/func.rs")"
if ! printf '%s\n' "$legacy_body_compiler_context" | rg -F '#[cfg(test)]' >/dev/null \
    || ! printf '%s\n' "$legacy_body_compiler_context" | rg -F 'pub(super) fn compile_func_legacy(' >/dev/null \
    || ! printf '%s\n' "$legacy_body_compiler_context" | rg -F 'fn compile_func_legacy_inner(' >/dev/null \
    || ! printf '%s\n' "$legacy_body_compiler_context" | rg -F 'fn compile_func_legacy_clean(' >/dev/null; then
    printf 'owner_audit_error=compile_func_legacy_cfg_test_adapter_shape_missing\n' >&2
    audit_failed=1
fi
printf 'test_only_codegen_legacy_body_compiler=src/codegen/func.rs::compile_func_legacy+inner+clean\n'

# Generic monomorphization is retained only as a cfg(test) adapter in
# codegen/func.rs. Keep the production inventory separate from that block so
# a new resolved/generic caller cannot hide behind the adapter exclusion.
generic_func_adapter_context="$(sed -n \
    '/The only caller is the cfg(test) compatibility adapter below/,/^[[:space:]]*fn compile_generic_func_inner(/p' \
    "$ROOT_DIR/src/codegen/func.rs")"
if ! printf '%s\n' "$generic_func_adapter_context" | rg -F '#[cfg(test)]' >/dev/null \
    || ! printf '%s\n' "$generic_func_adapter_context" | rg -F 'self.compile_generic_func(function, type_map)' >/dev/null; then
    printf 'owner_audit_error=compile_generic_func_cfg_test_adapter_shape_missing\n' >&2
    audit_failed=1
fi
generic_func_adapter_start="$(rg -n 'pub\(super\) fn compile_generic_func\(' \
    "$ROOT_DIR/src/codegen/func.rs" | head -n1 | cut -d: -f1)"
generic_func_adapter_end="$(rg -n '^[[:space:]]*fn compile_generic_func_inner\(' \
    "$ROOT_DIR/src/codegen/func.rs" | head -n1 | cut -d: -f1)"
generic_func_outside_adapter=''
while IFS=: read -r line_number line_text; do
    [ -z "$line_number" ] && continue
    if [ -z "$generic_func_adapter_start" ] || [ -z "$generic_func_adapter_end" ] \
        || [ "$line_number" -lt "$generic_func_adapter_start" ] \
        || [ "$line_number" -ge "$generic_func_adapter_end" ]; then
        generic_func_outside_adapter+="src/codegen/func.rs:${line_number}:${line_text}"$'\n'
    fi
done < <(rg -n 'compile_generic_func\(' "$ROOT_DIR/src/codegen/func.rs" || true)
if [ -n "$generic_func_outside_adapter" ]; then
    printf 'owner_audit_error=compile_generic_func_outside_cfg_test_adapter\n%s' \
        "$generic_func_outside_adapter" >&2
    audit_failed=1
fi
generic_refs="$(rg -n \
    --glob '*.rs' \
    --glob '!**/tests.rs' \
    --glob '!src/tests/**' \
    --glob '!src/codegen/func.rs' \
    'compile_generic_func\(' \
    "$SRC_DIR" || true)"
generic_call_count=0
if [ -n "$generic_refs" ]; then
    generic_call_count="$(printf '%s\n' "$generic_refs" | rg -v 'fn compile_generic_func' | wc -l)"
fi
printf 'production_compile_generic_func_call_sites=%s\n' "$generic_call_count"
if [ "$generic_call_count" -ne "$EXPECTED_PRODUCTION_COMPILE_GENERIC_FUNC_CALL_SITES" ]; then
    printf 'owner_audit_error=production_compile_generic_func_call_sites expected=%s actual=%s\n' \
        "$EXPECTED_PRODUCTION_COMPILE_GENERIC_FUNC_CALL_SITES" "$generic_call_count" >&2
    audit_failed=1
fi
generic_adapter_ref_count=0
if [ -n "$generic_func_adapter_context" ]; then
    generic_adapter_ref_count="$(printf '%s\n' "$generic_func_adapter_context" \
        | rg -c 'compile_generic_func\(' || true)"
fi
printf 'test_only_codegen_func_generic_adapter_refs=%s excluded_from_production=1\n' \
    "$generic_adapter_ref_count"

# The cfg(test) adapters are intentionally excluded from the production
# inventory above. Keep their count visible so a future edit cannot silently
# broaden the exclusion to an untracked production file.
test_legacy_refs="$(rg -n 'compile_func_legacy\(|compile_generic_func\(|legacy_body_file\(' \
    "$ROOT_DIR/src/codegen/tests.rs" || true)"
test_legacy_count=0
if [ -n "$test_legacy_refs" ]; then
    test_legacy_count="$(printf '%s\n' "$test_legacy_refs" | wc -l)"
fi
printf 'test_only_codegen_legacy_adapter_refs=%s excluded_from_production=1\n' "$test_legacy_count"

# The resolved emitter deliberately leaves a failed/partial body for the
# fifth pass to rebuild. Removing the top-level call without replacing this
# status handoff would strand an invalid LLVM body or drop supported code.
resolved_failure_recompile_guard() {
    local context
    context="$(sed -n '/fn compile_func_legacy_clean(/,/^    fn /p' \
        "$ROOT_DIR/src/codegen/func.rs")"
    for pattern in \
        'function.count_basic_blocks() != 0' \
        'resolved_failed_functions.contains(&func.name)' \
        'resolved_failed_functions.contains(&llvm_symbol)' \
        'self.clear_partial_body(function)'; do
        if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=resolved_failure_recompile_guard_missing=src/codegen/func.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'resolved_failure_recompile_guard=resolved-success-skip+failed-body-clear-and-recompile consumer=src/codegen/func.rs::compile_func_legacy_clean\n'
}

resolved_failure_recompile_guard
printf 'owner_count=%s\n' "$owner_count"

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

printf 'dynamic_probe_status=not_run\n'
printf 'dynamic_probe_test=%s\n' \
    'codegen::tests::compile_checked_routes_generic_ignored_bool_binding_projection_without_legacy_access'
printf 'dynamic_probe_test=%s\n' \
    'verifier::tests::public_checked_verifier_routes_recoverable_cross_state_flow_to_mir'
printf 'dynamic_probe_test=%s\n' \
    'verifier::tests::typed_ffi_symbolic_string_contexts_defer_without_legacy_access'
printf 'dynamic_probe_test=%s\n' \
    'verifier::tests::canonical_mir_bool_result_postconditions_keep_flow_verdicts_through_dual'
printf 'dynamic_probe_command=%s\n' \
    'run each dynamic_probe_test as an exact serial cargo test with --features llvm18-host-dynamic --lib --test-threads=1'

if [ "$audit_failed" -ne 0 ]; then
    printf 'audit_status=failed\n'
    exit 1
fi
printf 'audit_status=ok\n'
