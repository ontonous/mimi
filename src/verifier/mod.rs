pub(crate) mod checked_power;
mod ctx;
// The remaining surface-AST verifier helpers are test-only. Production
// verification enters through CheckedProgram and the canonical MIR/Resolved
// consumers below; the retired Flow AST state machine and its batch adapter
// are not compiled into the test harness either.
#[cfg(test)]
mod expr;
#[cfg(test)]
mod ffi_helper_tests;
mod ffi_literal_profile;
#[cfg(test)]
mod flow_compat_projection_tests;
mod flow_mir;
#[cfg(test)]
mod flow_retirement_tests;
#[cfg(test)]
mod func;
#[cfg(test)]
mod helpers;
mod mir;
mod mir_capability;
mod mir_request;
pub(crate) mod resolved_expr;
#[cfg(test)]
pub mod vir;

#[cfg(test)]
pub(crate) use ctx::SolverSession;
#[cfg(test)]
pub(crate) use ctx::Z3VarMap;
pub use ctx::{
    Counterexample, ProofArtifact, TrustedSubsetDomain, VerifStatus, VerificationResult, Verifier,
};

/// A separate verifier-only disposition for a checked semantic projection.
/// It is deliberately not a `VerificationResult`: it says only that the
/// dedicated source/MIR projection validated, and makes no runtime ownership
/// theorem or contract-proof claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct L2SemanticProjectionValidated {
    pub profile: String,
    pub mir_digest: String,
    pub route_identity_digest: String,
}

#[cfg(test)] // §11-#48 regression tests (audit_fix_verifier.rs)
pub(crate) use expr::encode_match_bool;
/// Convert a canonical route verifier error into the shared diagnostic
/// representation without changing the public `Result<_, String>` API used by
/// existing callers.  Registered MIR route codes are recognized anywhere in
/// an adapter-wrapped message using the shared token-boundary classifier;
/// ordinary verifier failures remain generic.
pub fn mir_route_error_to_diagnostic(message: impl Into<String>) -> crate::diagnostic::Diagnostic {
    crate::diagnostic::mir_route_error_diagnostic(message, crate::span::Span::UNKNOWN)
}

/// Verify a previously validated canonical MIR program with the experimental
/// MIR-only scalar contract engine.  The input boundary intentionally has no
/// AST/ResolvedBody parameter and never falls back to another verifier.
pub fn verify_mir(
    program: &crate::core::mir::reference::MirProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    if mir::mir_program_has_ordinary_map_owned_any_v2_map_set_drop_control_result_signal(program) {
        return Err(format!(
            "{}: MapSetDropControlResult requires its exact CheckedProgram-bound verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if mir::mir_program_has_ordinary_map_owned_any_v2_direct_print_result_signal(program) {
        return Err(format!(
            "{}: nested Result DirectPrint requires its exact CheckedProgram-bound L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if mir::mir_program_has_ordinary_map_direct_tuple_get_unpack_l2_signal(program) {
        return Err(format!(
            "{}: direct tuple Get L2 requires its exact CheckedProgram-aware semantic-projection verifier API",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if mir::mir_program_has_ordinary_map_set_get_any_i32_signal(program) {
        return Err(format!(
            "{}: ordinary Map Set/GetAny-i32 requires its exact CheckedProgram-aware L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if mir::mir_program_has_ordinary_map_set_get_string_result_signal(program) {
        return Err(format!(
            "{}: ordinary Map Set/Get<String> Result operations require the exact CheckedProgram-aware L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if mir_program_has_ordinary_map_owned_any_v2_set_any_result_signal(program) {
        return Err(format!(
            "{}: ordinary Map/Any SetAnyResult requires its exact CheckedProgram-bound ComputedStringKeyOwnerChain L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if mir::mir_program_has_ordinary_map_new_result_signal(program) {
        return Err(format!(
            "{}: ordinary Map NewResult is not admitted by default MIR verification; use its dedicated Checker-aware L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2DynamicArg0 { .. }
                )
            })
        })
    }) {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 DynamicKeysGet requires its exact CheckedProgram-aware verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if mir_program_has_ordinary_map_owned_any_v2_values_crosscall_string_caller_signal(program) {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 Values cross-call String caller requires the exact Checker-aware verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if mir_program_has_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_signal(
        program,
    ) {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 Values Result caller V3/V4 requires its exact CheckedProgram-bound versioned verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if crate::core::mir::validate_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_mir_route(
        program,
    )
    .is_ok()
    {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 Values heterogeneous String index-one requires the exact CheckedProgram source-digest binding",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if mir_program_has_ordinary_map_owned_any_v2_removed_key_checked_miss_signal(program) {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 RemovedKeyCheckedMiss requires the exact Checker-aware verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    // Source-bound Values profiles are deliberately absent from this raw
    // selector. Their admission requires the paired CheckedProgram/MIR
    // source-digest check; raw V2 MIR remains in the broad diagnostic island
    // while readiness is closed.
    let profile =
        if crate::core::mir::validate_map_empty_root_root_queries_v1_mir_route(program).is_ok() {
            crate::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE
        } else if crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_mir_route(
            program,
        )
        .is_ok()
        {
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE
        } else if crate::core::mir::validate_ordinary_map_owner_lifecycle_observer_v1_mir_route(program)
            .is_ok()
        {
            crate::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE
        } else if crate::core::mir::validate_ordinary_map_owned_any_v2_direct_print_mir_route(program)
            .is_ok()
        {
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE
        } else if crate::core::mir::validate_ordinary_map_owned_any_v2_computed_key_get_mir_route(program)
            .is_ok()
        {
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE
        } else if crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_mir_route(
            program,
        )
        .is_ok()
        {
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE
        } else if crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_mir_route(
            program,
        )
        .is_ok()
        {
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE
        } else if crate::core::mir::validate_ordinary_map_owned_any_v2_values_snapshot_replacement_mir_route(program)
            .is_ok()
        {
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE
        } else if crate::core::mir::validate_ordinary_map_owned_any_v2_values_field_move_mir_route(program)
            .is_ok()
        {
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
        } else if mir_program_has_ordinary_map_owned_any_v2_actions(program) {
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND
        } else {
            "verifier-mir-v1"
        };
    let receipt = program.route_receipt(profile);
    verify_mir_with_route_receipt(program, &receipt, source_hash)
}

fn verify_mir_results_with_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    source_hash: String,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<Vec<VerificationResult>, String> {
    mir::verify_program_with_route_receipt(program, source_hash, receipt)
}

/// Verify canonical MIR after checking the caller-supplied route receipt.
/// This keeps the general contract verifier on the same immutable graph
/// identity as bytecode, native, and FFI-only verification consumers. Core
/// MIR audit-only profiles are rejected here before the verifier can treat
/// a no-obligation program as successfully consumed.
pub fn verify_mir_with_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    if receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_RESULT_BRANCH_PROFILE
        || mir::mir_program_has_ordinary_map_owned_any_v2_map_set_drop_control_result_signal(
            program,
        )
    {
        return Err(format!(
            "{}: MapSetDropControlResult requires its exact CheckedProgram-bound verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if receipt.profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrintResult
            .as_str()
        || receipt.profile
            == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_RESULT_L2_VERIFIER_PROFILE
        || mir::mir_program_has_ordinary_map_owned_any_v2_direct_print_result_signal(program)
    {
        return Err(format!(
            "{}: nested Result DirectPrint requires its exact CheckedProgram-bound L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if receipt.profile == crate::core::mir::ORDINARY_MAP_DIRECT_TUPLE_GET_UNPACK_L2_VERIFIER_PROFILE
        || mir::mir_program_has_ordinary_map_direct_tuple_get_unpack_l2_signal(program)
    {
        return Err(format!(
            "{}: direct tuple Get L2 requires its exact CheckedProgram-aware semantic-projection verifier API",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_GET_ANY_TUPLE_L2_VERIFIER_PROFILE
        || program.functions().values().any(|function| {
            function
                .ownership
                .ordinary_map_owned_get_any_tuple_l2_projection()
                .is_some()
        })
    {
        return Err(format!(
            "{}: legacy tuple GetAny L2 requires its exact CheckedProgram-aware VerifierOnly entry point",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_EXECUTION_PROFILE
        || receipt.profile
            == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_L1_CORE_PROFILE
        || receipt.profile
            == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_L2_VERIFIER_PROFILE
        || mir::mir_program_has_ordinary_map_set_get_any_i32_signal(program)
    {
        return Err(format!(
            "{}: ordinary Map Set/GetAny-i32 requires its exact CheckedProgram-aware L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_EXECUTION_PROFILE
        || receipt.profile
            == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_L1_CORE_PROFILE
        || receipt.profile
            == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_L2_VERIFIER_PROFILE
        || mir::mir_program_has_ordinary_map_set_get_any_result_i32_signal(program)
    {
        return Err(format!(
            "{}: Result-valued ordinary Map Set/GetAny-i32 requires its exact CheckedProgram-aware Result L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if receipt.profile == crate::core::mir::ORDINARY_MAP_SET_GET_STRING_L2_VERIFIER_PROFILE
        || mir::mir_program_has_ordinary_map_set_get_string_result_signal(program)
    {
        return Err(format!(
            "{}: ordinary Map Set/Get<String> Result operations require the exact CheckedProgram-aware L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if receipt.profile == crate::core::mir::ORDINARY_MAP_SET_RESULT_L1_CORE_PROFILE {
        return Err(format!(
            "{}: Core MIR route profile '{}' is audit-only and has no verifier capability",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            crate::core::mir::ORDINARY_MAP_SET_RESULT_L1_CORE_PROFILE
        ));
    }
    if receipt.profile == crate::core::mir::ORDINARY_MAP_SET_RESULT_L2_VERIFIER_PROFILE
        || mir::mir_program_has_ordinary_map_set_result_signal(program)
    {
        return Err(format!(
            "{}: ordinary Map SetResult requires the exact CheckedProgram-aware L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE
        || mir_program_has_ordinary_map_owned_any_v2_set_any_result_signal(program)
    {
        return Err(format!(
            "{}: ordinary Map/Any SetAnyResult requires its exact CheckedProgram-bound ComputedStringKeyOwnerChain L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if receipt.profile == crate::core::mir::ORDINARY_MAP_NEW_RESULT_L1_CORE_PROFILE {
        return Err(format!(
            "{}: Core MIR route profile '{}' is audit-only and has no verifier capability",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            crate::core::mir::ORDINARY_MAP_NEW_RESULT_L1_CORE_PROFILE
        ));
    }
    if receipt.profile == crate::core::mir::ORDINARY_MAP_NEW_RESULT_L2_VERIFIER_PROFILE
        || mir::mir_program_has_ordinary_map_new_result_signal(program)
    {
        return Err(format!(
            "{}: ordinary Map NewResult requires the exact CheckedProgram-aware L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    validate_ordinary_map_owned_any_v2_receipt_binding(program, receipt)?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    let results = enforce_ordinary_map_owned_any_v2_mir_readiness(program, receipt, results);
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_route_receipt",
    )?;
    Ok(results)
}

/// Validate the exact CheckedProgram-bound legacy tuple GetAny L2 receipt and
/// verifier-only projection. The result is `NoObligations` when the checked
/// callable has no contracts; this does not claim executable tuple ownership.
pub fn verify_mir_with_ordinary_map_owned_get_any_tuple_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let results = mir::verify_program_with_ordinary_map_owned_get_any_tuple_l2_verifier_route(
        checked,
        program,
        source_hash.clone(),
        receipt,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_get_any_tuple_l2_verifier_route",
    )?;
    Ok(results)
}

/// Validate the direct tuple Get Checker receipt through its dedicated
/// VerifierOnly semantic projection. This returns a distinct disposition and
/// never constructs `VerificationResult`, `NoObligations`, or `Proven`.
pub fn verify_checked_program_ordinary_map_direct_tuple_get_unpack_l2(
    checked: &crate::core::CheckedProgram,
) -> Result<L2SemanticProjectionValidated, String> {
    let (program, receipt) =
        crate::core::mir::materialize_ordinary_map_direct_tuple_get_unpack_l2_verifier_route(
            checked,
        )?;
    mir::verify_program_with_ordinary_map_direct_tuple_get_unpack_l2_verifier_route(
        checked, &program, &receipt,
    )
}

/// Verify the isolated explicit Map NewResult L2 profile with the live
/// CheckedProgram, exact route receipt, contracts, proof worklist, and source
/// census bound through the complete verifier pass.
pub fn verify_mir_with_ordinary_map_new_result_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let results = mir::verify_program_with_ordinary_map_new_result_l2_verifier_route(
        checked,
        program,
        source_hash.clone(),
        receipt,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_new_result_l2_verifier_route",
    )?;
    Ok(results)
}

pub fn verify_mir_with_ordinary_map_set_get_i32_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let results = mir::verify_program_with_ordinary_map_set_get_i32_l2_verifier_route(
        checked,
        program,
        source_hash.clone(),
        receipt,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_set_get_i32_l2_verifier_route",
    )?;
    Ok(results)
}

/// Verify the exact CheckedProgram-bound generic `map_get` / i32-unpack
/// result profile. Unlike the public capability preflight, this entry
/// requires the direct Ensures proof obligation and rejects a no-obligation
/// or non-definitive observation.
pub fn verify_mir_with_ordinary_map_owned_any_v2_set_get_any_i32_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let results =
        mir::verify_program_with_ordinary_map_owned_any_v2_set_get_any_i32_l2_verifier_route(
            checked,
            program,
            source_hash.clone(),
            receipt,
        )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_set_get_any_i32_l2_verifier_route",
    )?;
    Ok(results)
}

/// Verify only the independently receipted Result-valued GetAny L2 route.
/// The dedicated MIR entry binds its Checker source receipt and requires a
/// definitive proof artifact; the generic MIR-only route stays closed.
pub fn verify_mir_with_ordinary_map_owned_any_v2_set_get_any_result_i32_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let results =
        mir::verify_program_with_ordinary_map_owned_any_v2_set_get_any_result_i32_l2_verifier_route(
            checked,
            program,
            source_hash.clone(),
            receipt,
        )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_set_get_any_result_i32_l2_verifier_route",
    )?;
    Ok(results)
}

/// Verify the isolated String payload Set/Get owner-v2 profile only after
/// binding the exact CheckedProgram receipt, MIR graph, TypeDesc catalog,
/// proof worklist, and source census. Generic MIR-only APIs remain closed.
pub fn verify_mir_with_ordinary_map_set_get_string_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let results = mir::verify_program_with_ordinary_map_set_get_string_l2_verifier_route(
        checked,
        program,
        source_hash.clone(),
        receipt,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_set_get_string_l2_verifier_route",
    )?;
    Ok(results)
}

/// Verify the exact ordinary Map SetResult projection through its Checker
/// receipt and full source census. Contract-bearing requests run the symbolic
/// proof over both Result outcomes; contract-free requests return `NoObligations`
/// without claiming that the status/tag domain was proven.
pub fn verify_mir_with_ordinary_map_set_result_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let results = mir::verify_program_with_ordinary_map_set_result_l2_verifier_route(
        checked,
        program,
        source_hash.clone(),
        receipt,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_set_result_l2_verifier_route",
    )?;
    Ok(results)
}

/// Validate the nested Result-match DirectPrint L2 route from the exact
/// CheckedProgram receipt and MIR graph. This is intentionally a neutral
/// `NoObligations` observation only: it rejects programs with contracts and
/// does not claim Result-tag, runtime ownership, or stdout proof.
pub fn verify_mir_with_ordinary_map_owned_any_v2_direct_print_result_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let results = mir::verify_program_with_ordinary_map_owned_any_v2_direct_print_result_route(
        checked,
        program,
        source_hash.clone(),
        receipt,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_direct_print_result_route",
    )?;
    Ok(results)
}

/// Consume the exact three-path MapSetDropControl Result branch through its
/// Checker-bound neutral route. This returns only `NoObligations`; it does not
/// prove Result status/tag semantics.
pub fn verify_mir_with_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let results = mir::verify_program_with_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
        checked,
        program,
        source_hash.clone(),
        receipt,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route",
    )?;
    Ok(results)
}

/// Verify the nested Result Values local-index slice through its exact
/// Checker-bound source and MIR receipt, independent Bytecode/native
/// consumers, and the route-scoped whole-program capability gate. The final
/// symbolic result remains authoritative: reachable E0800 traps stay
/// `Disproven` and cannot be folded into the normal return domain.
pub fn verify_mir_with_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_RESULT_BRANCH_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: ValuesLocalIndexI32 Result-branch verifier requires exact route profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: ValuesLocalIndexI32 Result-branch route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let checked_receipt =
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
            checked, program,
        )
        .map_err(|message| {
            format!(
                "{}: ValuesLocalIndexI32 Result-branch Checker/MIR route rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    if &checked_receipt != receipt {
        return Err(format!(
            "{}: ValuesLocalIndexI32 Result-branch receipt differs from the exact CheckedProgram/MIR route",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::interp::bytecode::compile_mir_program_with_values_local_index_i32_result_branch_route(
        checked, program,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: ValuesLocalIndexI32 Result-branch Bytecode preflight rejected the route: {errors:?}"
        )
    })?;
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(
        &context,
        "verifier_ordinary_map_owned_any_v2_values_local_index_i32_result_branch",
    );
    generator
        .compile_checked_mir_native_with_values_local_index_i32_result_branch_route(
            checked, program, receipt,
        )
        .map_err(|errors| {
            format!(
                "MIR-CAPABILITY-001: ValuesLocalIndexI32 Result-branch native preflight rejected the route: {errors:?}"
            )
        })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: ValuesLocalIndexI32 Result-branch whole-program capability rejected the route: {errors:?}"
        )
    })?;
    let results =
        mir::verify_program_with_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
            checked,
            program,
            source_hash.clone(),
            receipt,
        )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route",
    )?;
    Ok(results)
}

/// Verify the exact Checker-sealed KeysSnapshot profile. The generic MIR
/// receipt API stays closed because it cannot bind this owner-v2 operation
/// back to a CheckedProgram; this entry validates that source pair, preflights
/// the AST-free bytecode consumer, then runs the symbolic verifier.
pub fn verify_mir_with_ordinary_map_owned_any_v2_keys_snapshot_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: KeysSnapshot verifier requires exact route profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: KeysSnapshot canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_route(checked, program)
            .map_err(|message| {
                format!(
                    "{}: KeysSnapshot Checker/MIR route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
    if &expected != receipt {
        return Err(format!(
            "{}: KeysSnapshot route receipt differs from the Checker-bound receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_mir_route(program).map_err(
        |message| {
            format!(
                "{}: KeysSnapshot MIR witness rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        },
    )?;

    // This checked-source-aware AST-free compiler is deliberately before the
    // capability gate: a verifier-only green result cannot admit a profile
    // whose executable consumer still rejects its MIR shape.
    crate::interp::bytecode::mir::compile_mir_program_with_keys_snapshot_route(checked, program)
        .map_err(|errors| {
            format!(
                "MIR-CAPABILITY-001: KeysSnapshot AST-free bytecode consumer rejected the route: {errors:?}"
            )
        })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_keys_snapshot_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: KeysSnapshot verifier capability rejected the graph: {errors:?}"
        )
    })?;

    // Do not route this through verify_mir_with_route_receipt: that public
    // raw-receipt API correctly rejects KeysSnapshot because it has no
    // CheckedProgram with which to establish source authority.
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_keys_snapshot_route",
    )?;
    Ok(results)
}

/// Verify the exact Checker-sealed argv[0] → dynamic key → Keys/GetAny
/// profile. The generic receipt API stays closed because it cannot establish
/// source authority; this path validates the CheckedProgram/MIR pair and
/// preflights the AST-free Bytecode, native, and capability consumers before
/// running symbolic verification.
pub fn verify_mir_with_ordinary_map_owned_any_v2_dynamic_keys_get_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: DynamicKeysGet verifier requires exact route profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: DynamicKeysGet canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected = crate::core::mir::validate_ordinary_map_owned_any_v2_dynamic_keys_get_route(
        checked, program,
    )
    .map_err(|message| {
        format!(
            "{}: DynamicKeysGet Checker/MIR route rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    if &expected != receipt {
        return Err(format!(
            "{}: DynamicKeysGet route receipt differs from the CheckedProgram-bound receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_dynamic_keys_get_mir_route(program)
        .map_err(|message| {
            format!(
                "{}: DynamicKeysGet MIR witness rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    crate::interp::bytecode::mir::compile_mir_program_with_dynamic_keys_get_route(
        checked, program,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: DynamicKeysGet AST-free bytecode consumer rejected the route: {errors:?}"
        )
    })?;
    crate::codegen::mir::validate_mir_native_dynamic_keys_get_route(program, receipt).map_err(
        |errors| {
            format!(
                "MIR-CAPABILITY-001: DynamicKeysGet native consumer rejected the route: {errors:?}"
            )
        },
    )?;
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(
        &context,
        "verifier_ordinary_map_owned_any_v2_dynamic_keys_get",
    );
    generator
        .compile_checked_mir_native_with_dynamic_keys_get_route(checked, program, receipt)
        .map_err(|errors| {
            format!(
                "MIR-CAPABILITY-001: DynamicKeysGet CheckedProgram native consumer rejected the route: {errors:?}"
            )
        })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_dynamic_keys_get_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: DynamicKeysGet verifier capability rejected the graph: {errors:?}"
        )
    })?;

    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_dynamic_keys_get_route",
    )?;
    Ok(results)
}

/// Verify the exact CheckedProgram-sealed computed String-key owner chain.
/// This source-qualified route joins its typed checker receipt, exact MIR
/// owner/action census, VM anchor, native route, and verifier capability
/// before returning any symbolic result.
pub fn verify_mir_with_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: ComputedStringKeyOwnerChain verifier requires exact route profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: ComputedStringKeyOwnerChain canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
            checked, program,
        )
        .map_err(|message| {
            format!(
                "{}: ComputedStringKeyOwnerChain Checker/MIR route rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    if &expected != receipt {
        return Err(format!(
            "{}: ComputedStringKeyOwnerChain receipt differs from the CheckedProgram-bound route",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_mir_route(
        program,
    )
    .map_err(|message| {
        format!(
            "{}: ComputedStringKeyOwnerChain MIR witness rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    crate::interp::bytecode::compile_mir_program_with_computed_string_key_owner_chain_route(
        checked, program,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: ComputedStringKeyOwnerChain AST-free bytecode consumer rejected the route: {errors:?}"
        )
    })?;
    crate::codegen::mir::validate_mir_native_computed_string_key_owner_chain_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: ComputedStringKeyOwnerChain native consumer rejected the route: {errors:?}"
        )
    })?;
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(
        &context,
        "verifier_ordinary_map_owned_any_v2_computed_string_key_owner_chain",
    );
    generator.suppress_canonical_mir_native_census();
    generator
        .compile_checked_mir_native_with_computed_string_key_owner_chain_route(
            checked, program, receipt,
        )
        .map_err(|errors| {
            format!(
                "MIR-CAPABILITY-001: ComputedStringKeyOwnerChain CheckedProgram native consumer rejected the route: {errors:?}"
            )
        })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: ComputedStringKeyOwnerChain verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results =
        mir::verify_program_with_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
            checked,
            program,
            source_hash.clone(),
            receipt,
        )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route",
    )?;
    Ok(results)
}

/// Verify the exact Checker-sealed EmptyRoot profile. A raw MIR receipt is
/// insufficient because this profile contains owner-v2 Values operations;
/// only this boundary joins the checked source proof to the MIR before the
/// symbolic verifier consumes the route receipt.
pub fn verify_mir_with_map_empty_root_root_queries_v1_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile = crate::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: MapEmptyRootRootQueriesV1 verifier requires exact route profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: MapEmptyRootRootQueriesV1 canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected_receipt = program.route_receipt(profile);
    if receipt != &expected_receipt {
        return Err(format!(
            "{}: MapEmptyRootRootQueriesV1 supplied route receipt differs from the exact MIR profile receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_map_empty_root_root_queries_v1_route(checked, program).map_err(
        |message| {
            format!(
                "{}: MapEmptyRootRootQueriesV1 Checker/MIR receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        },
    )?;
    validate_mir_capabilities(program).map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: MapEmptyRootRootQueriesV1 verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_map_empty_root_root_queries_v1_route",
    )?;
    Ok(results)
}

/// Verify the narrow owner-v2 IsEmpty slice with the Checker graph that
/// authorizes its operation receipt. The raw MIR API intentionally cannot
/// admit this operation because its input boundary has no source-level
/// Checker authority.
pub fn verify_mir_with_ordinary_map_owned_any_v2_is_empty_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    if receipt.profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE {
        return Err(format!(
            "{}: IsEmpty verifier requires exact route profile '{}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE,
            receipt.profile
        ));
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: IsEmpty canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_is_empty_route(checked, program).map_err(
        |message| {
            format!(
                "{}: IsEmpty Checker/MIR operation receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        },
    )?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_is_empty_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!("MIR-CAPABILITY-001: IsEmpty verifier capability rejected the graph: {errors:?}")
    })?;
    let results = mir::verify_program_with_ordinary_map_owned_any_v2_is_empty_route(
        checked,
        program,
        source_hash.clone(),
        receipt,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_is_empty_route",
    )?;
    Ok(results)
}

/// Verify the exact mixed-payload RecordUnit route only with its Checker-bound
/// source and MIR receipts. Broad or generic owner-v2 receipts cannot
/// authorize this profile.
pub fn verify_mir_with_ordinary_map_owned_any_v2_record_unit_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: RecordUnit verifier requires exact route profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: RecordUnit canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_record_unit_route(checked, program)
        .map_err(|message| {
            format!(
                "{}: RecordUnit Checker/MIR operation receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_record_unit_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!("MIR-CAPABILITY-001: RecordUnit verifier capability rejected the graph: {errors:?}")
    })?;
    let results = mir::verify_program_with_ordinary_map_owned_any_v2_record_unit_route(
        checked,
        program,
        source_hash.clone(),
        receipt,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_record_unit_route",
    )?;
    Ok(results)
}

/// Verify the exact no-query MapSetDropControl slice only with its
/// Checker-bound route receipt. A raw MIR receipt cannot authorize this
/// profile, even though the profile contains no observation operation.
pub fn verify_mir_with_ordinary_map_owned_any_v2_map_set_drop_control_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    if receipt.profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE {
        return Err(format!(
            "{}: MapSetDropControl verifier requires exact route profile '{}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: MapSetDropControl canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_map_set_drop_control_route(
        checked, program,
    )
    .map_err(|message| {
        format!(
            "{}: MapSetDropControl Checker/MIR operation receipt rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    validate_mir_capabilities(program).map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: MapSetDropControl verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_map_set_drop_control_route",
    )?;
    Ok(results)
}

/// Verify the exact arm-local persistent Set diamond only with its
/// Checker-bound route receipt. A raw MIR or broad owner-v2 receipt cannot
/// establish the source CFG join proof.
pub fn verify_mir_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    if receipt.profile
        != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE
    {
        return Err(format!(
            "{}: BranchLocalSetDiamond verifier requires exact route profile '{}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: BranchLocalSetDiamond canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
        checked, program,
    )
    .map_err(|message| {
        format!(
            "{}: BranchLocalSetDiamond Checker/MIR operation receipt rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_mir_route(
        program,
    )
    .map_err(|message| {
        format!(
            "{}: BranchLocalSetDiamond MIR operation census rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: BranchLocalSetDiamond verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route",
    )?;
    Ok(results)
}

/// Verify the narrow zero-miss i32 slice only with its Checker-bound route
/// receipt. Raw MIR has no authority to decide whether an i32 Any value is a
/// selected payload or the canonical miss sentinel.
pub fn verify_mir_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    if receipt.profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE {
        return Err(format!(
            "{}: ZeroMissI32 verifier requires exact route profile '{}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE,
            receipt.profile
        ));
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: ZeroMissI32 canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_route(checked, program)
        .map_err(|message| {
            format!(
                "{}: ZeroMissI32 Checker/MIR operation receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: ZeroMissI32 verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_zero_miss_i32_route",
    )?;
    Ok(results)
}

/// Verify the CheckedGet profile only when its route receipt is rejoined to
/// the exact Checker source and canonical MIR graph. The generic raw-receipt
/// verifier intentionally continues to reject any canonical i32 miss because
/// it cannot establish this source authority.
pub fn verify_mir_with_ordinary_map_owned_any_v2_checked_get_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: CheckedGet verifier requires exact route profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: CheckedGet canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected = crate::core::mir::validate_ordinary_map_owned_any_v2_checked_get_source_route(
        checked, program,
    )
    .map_err(|message| {
        format!(
            "{}: CheckedGet Checker/MIR route rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    if &expected != receipt {
        return Err(format!(
            "{}: CheckedGet route receipt differs from the Checker-bound receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_checked_get_route(program).map_err(
        |message| {
            format!(
                "{}: CheckedGet MIR witness rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        },
    )?;

    // Keep the AST-free executable consumer and both verifier/native
    // capability boundaries ahead of symbolic success. The narrow source
    // receipt authorizes only this CheckedGet route; the generic raw verifier
    // remains closed to the i32-miss receipt carried by the graph.
    let bytecode = crate::interp::bytecode::mir::compile_mir_program_with_route_receipt(
        program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: CheckedGet AST-free bytecode consumer rejected the route: {errors:?}"
        )
    })?;
    if bytecode.ast.is_some() {
        return Err(
            "MIR-CAPABILITY-001: CheckedGet bytecode consumer unexpectedly retained an AST".into(),
        );
    }
    validate_mir_capabilities(program).map_err(|errors| {
        format!("MIR-CAPABILITY-001: CheckedGet verifier capability rejected the graph: {errors:?}")
    })?;
    crate::codegen::mir::validate_mir_native_checked_get_route(program, receipt).map_err(
        |errors| {
            format!(
                "MIR-CAPABILITY-001: CheckedGet native route capability rejected the graph: {errors:?}"
            )
        },
    )?;

    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_checked_get_route",
    )?;
    Ok(results)
}

/// Verify the dedicated Checker-bound `get_or_default<i32>` route. Raw MIR
/// route receipts cannot authorize its cross-call payload cast or borrowed
/// Map-root bridge.
pub fn verify_mir_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    mir::verify_program_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
        checked,
        program,
        source_hash,
        receipt,
    )
}

/// Verify the distinct String GetOrDefault profile only with the live
/// CheckedProgram receipt that authorizes its Map payload selection, default
/// pack path, and canonical i32 miss sentinel.
pub fn verify_mir_with_ordinary_map_owned_any_v2_get_or_default_string_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    mir::verify_program_with_ordinary_map_owned_any_v2_get_or_default_string_route(
        checked,
        program,
        source_hash,
        receipt,
    )
}

/// Verify the persistent-remove checked-miss profile only with its exact
/// Checker-bound receipt. This entry preserves the Remove-successor lineage
/// and independent canonical-i32-miss proof all the way through capability
/// validation and proof provenance.
pub fn verify_mir_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    if receipt.profile
        != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE
    {
        return Err(format!(
            "{}: RemovedKeyCheckedMiss verifier requires exact route profile '{}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE,
            receipt.profile
        ));
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: RemovedKeyCheckedMiss canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
        checked, program,
    )
    .map_err(|message| {
        format!(
            "{}: RemovedKeyCheckedMiss Checker/MIR operation receipt rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: RemovedKeyCheckedMiss verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route",
    )?;
    Ok(results)
}

/// Verify the narrow owner-v2 CloneIsolation slice with the Checker graph
/// that authorizes its source and destination roots. A raw MIR receipt cannot
/// establish the source-level isolation proof on its own.
pub fn verify_mir_with_ordinary_map_owned_any_v2_clone_isolation_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    if receipt.profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE {
        return Err(format!(
            "{}: CloneIsolation verifier requires exact route profile '{}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE,
            receipt.profile
        ));
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: CloneIsolation canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_clone_isolation_route(checked, program)
        .map_err(|message| {
            format!(
                "{}: CloneIsolation Checker/MIR operation receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    validate_mir_capabilities(program).map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: CloneIsolation verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_clone_isolation_route",
    )?;
    Ok(results)
}

/// Verify the exact Checker-sealed PersistentRootAliasReplacement profile.
/// The source-aware route validator binds both persistent Map roots and the
/// replacement operations to this CheckedProgram; raw MIR receipts cannot
/// authorize the verifier route.
pub fn verify_mir_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: PersistentRootAliasReplacement verifier requires exact route profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: PersistentRootAliasReplacement canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
        checked, program,
    )
    .map_err(|message| {
        format!(
            "{}: PersistentRootAliasReplacement Checker/MIR route rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    let canonical_receipt = program.route_receipt(profile);
    if !canonical_receipt.same_semantic_identity(receipt) {
        return Err(format!(
            "{}: PersistentRootAliasReplacement route receipt differs from the Checker-bound receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    validate_mir_capabilities(program).map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: PersistentRootAliasReplacement verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route",
    )?;
    Ok(results)
}

/// Verify the narrowly receipted direct Values producer-to-caller transfer.
/// The Checker receipt authorizes only a Values-producing function, one
/// direct caller, optional length observation, whole-owner Drop, and scalar
/// return. Indexed unpack across this call boundary has no Checker source
/// receipt and remains rejected.
pub fn verify_mir_with_ordinary_map_owned_any_v2_values_producer_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    if receipt.profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_PRODUCER_CALLER_PROFILE
    {
        return Err(format!(
            "{}: Values producer/caller verifier requires exact route profile '{}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_PRODUCER_CALLER_PROFILE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: Values producer/caller canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected_receipt =
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_route(
            checked, program,
        )
        .map_err(|message| {
            format!(
                "{}: Values producer/caller Checker/MIR route rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    if &expected_receipt != receipt {
        return Err(format!(
            "{}: Values producer/caller supplied route receipt differs from the Checker-bound receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_mir_route(program)
        .map_err(|message| {
            format!(
                "{}: Values producer/caller MIR operation census rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_producer_caller_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: Values producer/caller verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_values_producer_caller_route",
    )?;
    Ok(results)
}

/// Verify the exact Checker-sealed Values producer and direct caller that
/// selects one owned String from the returned snapshot. The source/MIR owner
/// bridge is validated before symbolic execution; the broad V2 readiness gate
/// remains closed for this staged profile.
pub fn verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: Values cross-call String caller verifier requires exact route profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: Values cross-call String caller canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
            checked, program,
        )
        .map_err(|message| {
            format!(
                "{}: Values cross-call String caller Checker/MIR route rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    if &expected != receipt {
        return Err(format!(
            "{}: Values cross-call String caller route receipt differs from the Checker-bound receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_mir_route(
        program,
    )
    .map_err(|message| {
        format!(
            "{}: Values cross-call String caller MIR operation census rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: Values cross-call String caller verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results =
        mir::verify_program_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
            checked,
            program,
            source_hash.clone(),
            receipt,
        )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route",
    )?;
    Ok(results)
}

/// Verify the separately versioned Values cross-call profile that returns
/// two sorted String entries and selects the second entry with literal index
/// one. The Checker source seal, V2 route receipt, ownership bridge, and exact
/// capability receipt must all identify the same producer and caller.
pub fn verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: Values cross-call String caller index-one verifier requires exact V2 route profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: Values cross-call String caller index-one canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
            checked, program,
        )
        .map_err(|message| {
            format!(
                "{}: Values cross-call String caller index-one Checker/MIR route rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    if &expected != receipt {
        return Err(format!(
            "{}: Values cross-call String caller index-one route receipt differs from the Checker-bound V2 receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_mir_route(
        program,
    )
    .map_err(|message| {
        format!(
            "{}: Values cross-call String caller index-one MIR operation census rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: Values cross-call String caller index-one V2 capability receipt rejected the graph: {errors:?}"
        )
    })?;
    let results =
        mir::verify_program_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
            checked,
            program,
            source_hash.clone(),
            receipt,
        )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route",
    )?;
    Ok(results)
}

/// Verify the exact V3 Result-branch Values producer and literal index-zero
/// String caller route. The V3 receipt is joined to Checker source identity,
/// its independent producer/caller MIR proof receipt, verifier capability,
/// and the actual MIR proof pass; it cannot borrow straight-line V1/V2 success.
pub fn verify_mir_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: Values Result caller verifier requires exact V3 profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: Values Result caller V3 route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected = crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
        checked, program,
    )
    .map_err(|message| {
        format!(
            "{}: Values Result caller V3 Checker/MIR route rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    if &expected != receipt {
        return Err(format!(
            "{}: Values Result caller V3 receipt differs from the exact CheckedProgram-bound receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_mir_route(
        program,
    )
    .map_err(|message| {
        format!(
            "{}: Values Result caller V3 MIR proof census rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: Values Result caller V3 capability rejected the graph: {errors:?}"
        )
    })?;
    let results = mir::verify_program_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
        checked,
        program,
        source_hash.clone(),
        receipt,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route",
    )?;
    Ok(results)
}

/// Verify the independent V4 Result-branch Values caller, which has two
/// sorted Strings and selects literal index one. Its profile, route receipt,
/// and proof evidence are separate from V3 and from straight-line V1/V2.
pub fn verify_mir_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_INDEX1_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: Values Result caller verifier requires exact V4 index-one profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: Values Result caller V4 route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected = crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
        checked, program,
    )
    .map_err(|message| {
        format!(
            "{}: Values Result caller V4 Checker/MIR route rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    if &expected != receipt {
        return Err(format!(
            "{}: Values Result caller V4 receipt differs from the exact CheckedProgram-bound receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_mir_route(
        program,
    )
    .map_err(|message| {
        format!(
            "{}: Values Result caller V4 MIR proof census rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: Values Result caller V4 capability rejected the graph: {errors:?}"
        )
    })?;
    let results = mir::verify_program_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_route(
        checked,
        program,
        source_hash.clone(),
        receipt,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_route",
    )?;
    Ok(results)
}

/// Verify the exact source-bound heterogeneous Values slice: UTF-8-sorted
/// entries `b -> String`, then `a -> i32`, selecting String index one. The raw
/// MIR receipt API intentionally cannot authorize this owner transfer.
pub fn verify_mir_with_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: Values heterogeneous String index-one verifier requires exact route profile '{profile}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: Values heterogeneous String index-one canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected = crate::core::mir::validate_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
        checked, program,
    )
    .map_err(|message| {
        format!(
            "{}: Values heterogeneous String index-one Checker/MIR route rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    if &expected != receipt {
        return Err(format!(
            "{}: Values heterogeneous String index-one route receipt differs from the Checker-bound receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_mir_route(
        program,
    )
    .map_err(|message| {
        format!(
            "{}: Values heterogeneous String index-one MIR operation census rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    crate::interp::bytecode::mir::compile_mir_program_with_values_heterogeneous_string_index1_route(
        checked, program,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: Values heterogeneous String index-one bytecode consumer rejected the route: {errors:?}"
        )
    })?;
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(
        &context,
        "verifier_ordinary_map_owned_any_v2_values_heterogeneous_string_index1",
    );
    generator.suppress_canonical_mir_native_census();
    generator
        .compile_checked_mir_native_with_values_heterogeneous_string_index1_route(
            checked, program, receipt,
        )
        .map_err(|errors| {
            format!(
                "MIR-CAPABILITY-001: Values heterogeneous String index-one native consumer rejected the route: {errors:?}"
            )
        })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: Values heterogeneous String index-one verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route",
    )?;
    Ok(results)
}

/// Verify the exact Map-root producer return and its unique direct caller.
/// The CheckedProgram source seal, canonical MIR route receipt, and derived
/// cross-function owner bridge must all identify the same zero-argument
/// producer and scalar-observing caller before the MIR contract engine runs.
pub fn verify_mir_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_PRODUCER_CALLER_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: Map-root producer/caller verifier requires exact route profile '{}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            profile,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: Map-root producer/caller canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_route(
            checked, program,
        )
        .map_err(|message| {
            format!(
                "{}: Map-root producer/caller Checker/MIR route rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    if &expected != receipt {
        return Err(format!(
            "{}: Map-root producer/caller supplied route receipt differs from the Checker-bound receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_mir_route(
        program,
    )
    .map_err(|message| {
        format!(
            "{}: Map-root producer/caller MIR operation census rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: Map-root producer/caller verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results =
        mir::verify_program_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
            checked,
            program,
            source_hash.clone(),
            receipt,
        )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_map_root_producer_caller_route",
    )?;
    Ok(results)
}

/// Verify the exact Checker-sealed MapRoot → direct GetAny → checked
/// String-unpack bridge. The source seal, route receipt, operation receipt,
/// and strict capability gate must identify the same graph before symbolic
/// execution. This remains an exact island and does not open broad V2
/// production readiness.
pub fn verify_mir_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    source_hash: String,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<Vec<VerificationResult>, String> {
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_GET_ANY_CALLER_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "{}: Map-root GetAny-caller verifier requires exact route profile '{}', found '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            profile,
            receipt.profile
        ));
    }
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: Map-root GetAny-caller canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
            checked, program,
        )
        .map_err(|message| {
            format!(
                "{}: Map-root GetAny-caller Checker/MIR route rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    if &expected != receipt {
        return Err(format!(
            "{}: Map-root GetAny-caller supplied route receipt differs from the Checker-bound receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_mir_route(program)
        .map_err(|message| {
            format!(
                "{}: Map-root GetAny-caller MIR operation census rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
        checked, program, receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: Map-root GetAny-caller verifier capability rejected the graph: {errors:?}"
        )
    })?;
    let results = mir::verify_program_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
        checked,
        program,
        source_hash.clone(),
        receipt,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route",
    )?;
    Ok(results)
}

/// Return the raw symbolic MIR observations for a test-only failure-model
/// assertion. Unlike the public API, this deliberately does not apply the
/// production-readiness downgrade; callers must still bind the exact MIR and
/// route receipt, and this helper must never be used by production code.
#[cfg(test)]
pub(crate) fn verify_mir_failure_model_with_route_receipt_for_test(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    if receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE
        || mir_program_has_ordinary_map_owned_any_v2_set_any_result_signal(program)
    {
        return Err(format!(
            "{}: ordinary Map/Any SetAnyResult requires its exact CheckedProgram-bound ComputedStringKeyOwnerChain L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    validate_ordinary_map_owned_any_v2_receipt_binding(program, receipt)?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "test-only raw MIR failure-model observation",
    )?;
    Ok(results)
}

/// Internal verifier diagnostic path. It validates graph identity and keeps
/// raw symbolic observations so the CheckedProgram layer can add one
/// source-owner NITS row after correlating MIR owner IDs with source names.
/// Public execution-verifier APIs must use `verify_mir_with_route_receipt`.
fn verify_mir_diagnostic_with_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    if receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE
        || mir_program_has_ordinary_map_owned_any_v2_set_any_result_signal(program)
    {
        return Err(format!(
            "{}: ordinary Map/Any SetAnyResult requires its exact CheckedProgram-bound ComputedStringKeyOwnerChain L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    validate_ordinary_map_owned_any_v2_receipt_binding(program, receipt)?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(&results, receipt, &source_hash, "verifier_diagnostic")?;
    Ok(results)
}

fn mir_program_has_ordinary_map_owned_any_v2_actions(
    program: &crate::core::mir::reference::MirProgram,
) -> bool {
    program.functions().values().any(|function| {
        function
            .ownership
            .has_checker_ordinary_map_owned_any_v2_actions()
    })
}

fn mir_program_has_ordinary_map_owned_any_v2_set_any_result_signal(
    program: &crate::core::mir::reference::MirProgram,
) -> bool {
    program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2SetAnyResult { .. }
                )
            })
        })
    })
}

fn mir_program_has_ordinary_map_owned_any_v2_is_empty_action(
    program: &crate::core::mir::reference::MirProgram,
) -> bool {
    program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { .. }
                )
            })
        })
    })
}

fn mir_program_has_ordinary_map_owned_any_v2_zero_miss_i32_signal(
    program: &crate::core::mir::reference::MirProgram,
) -> bool {
    if crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_mir_route(program).is_ok()
    {
        return true;
    }
    let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    program
        .ordinary_map_owned_any_v2_unpack_result_receipts()
        .is_ok_and(|receipts| {
            receipts.iter().any(|receipt| {
                receipt.conversion_to == i32_type
                    && receipt.result_type == i32_type
                    && receipt.payload_source
                        == crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::CanonicalI32Miss
            })
        })
}

fn mir_program_has_ordinary_map_owned_any_v2_removed_key_checked_miss_signal(
    program: &crate::core::mir::reference::MirProgram,
) -> bool {
    if crate::core::mir::validate_ordinary_map_owned_any_v2_removed_key_checked_miss_mir_route(
        program,
    )
    .is_ok()
    {
        return true;
    }
    let has_remove = program.functions().values().any(|function| {
        function
            .ownership
            .has_checker_ordinary_map_owned_any_v2_remove_action()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { .. }
                    )
                })
            })
    });
    let has_canonical_miss = program
        .ordinary_map_owned_any_v2_unpack_result_receipts()
        .is_ok_and(|receipts| {
            receipts.iter().any(|receipt| {
                receipt.payload_source
                    == crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::CanonicalI32Miss
            })
        });
    has_remove && has_canonical_miss
}

fn mir_program_has_ordinary_map_owned_any_v2_clone_action(
    program: &crate::core::mir::reference::MirProgram,
) -> bool {
    program.functions().values().any(|function| {
        function
            .ownership
            .has_checker_ordinary_map_owned_any_v2_clone_action()
    })
}

fn mir_program_has_ordinary_map_owned_any_v2_values_producer_caller_signal(
    program: &crate::core::mir::reference::MirProgram,
) -> bool {
    let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
    program.functions().values().any(|function| {
        function.result == values_ty
            || function.blocks.values().any(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| match &instruction.kind {
                        crate::core::mir::MirInstructionKind::Call {
                            result,
                            callee: crate::core::ir::ResolvedCallee::Function(target),
                            ..
                        } => {
                            result.as_ref().is_some_and(|result| {
                                function
                                    .values
                                    .get(result)
                                    .is_some_and(|value| value.ty == values_ty)
                            }) || program
                                .functions()
                                .get(target)
                                .is_some_and(|target| target.result == values_ty)
                        }
                        _ => false,
                    })
            })
    })
}

fn mir_program_has_ordinary_map_owned_any_v2_values_crosscall_string_caller_signal(
    program: &crate::core::mir::reference::MirProgram,
) -> bool {
    crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_mir_route(
        program,
    )
    .is_ok()
        || program
            .ordinary_map_owned_any_v2_values_crosscall_string_caller_receipts()
            .is_ok_and(|receipts| !receipts.is_empty())
}

fn mir_program_has_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_signal(
    program: &crate::core::mir::reference::MirProgram,
) -> bool {
    let has_result_set = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
                )
            })
        })
    });
    let has_values_unpack = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack { .. }
                )
            })
        })
    });
    (has_result_set && has_values_unpack)
        || crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_mir_route(
            program,
        )
        .is_ok()
        || crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_mir_route(
            program,
        )
        .is_ok()
        || program
            .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_receipts()
            .is_ok_and(|receipts| !receipts.is_empty())
}

fn mir_program_has_record_unit_string_field_output(
    program: &crate::core::mir::reference::MirProgram,
) -> bool {
    program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::BuiltinCall {
                        string_field_contract: Some(_),
                        ..
                    }
                )
            })
        })
    })
}

fn validate_ordinary_map_owned_any_v2_receipt_binding(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), String> {
    if receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_RESULT_BRANCH_PROFILE
        || mir::mir_program_has_ordinary_map_owned_any_v2_map_set_drop_control_result_signal(
            program,
        )
    {
        return Err(format!(
            "{}: MapSetDropControlResult requires its exact CheckedProgram-bound verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE
    {
        return Err(format!(
            "{}: PersistentRootAliasReplacement requires the exact CheckedProgram-aware verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    let receipt_is_record_unit =
        receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE;
    let program_has_record_unit_output = mir_program_has_record_unit_string_field_output(program);
    if receipt_is_record_unit || program_has_record_unit_output {
        return Err(format!(
            "{}: RecordUnit String-field output requires the exact CheckedProgram-aware verifier route (receipt_profile='{}', program_record_unit_output={program_has_record_unit_output})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_dynamic_keys_get =
        receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE;
    let program_has_dynamic_keys_get = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2DynamicArg0 { .. }
                )
            })
        })
    });
    if receipt_is_dynamic_keys_get || program_has_dynamic_keys_get {
        return Err(format!(
            "{}: DynamicKeysGet argv[0] requires the exact CheckedProgram-aware verifier route (receipt_profile='{}', program_dynamic_keys_get={program_has_dynamic_keys_get})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_get_or_default_i32 =
        receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_I32_PROFILE;
    let receipt_is_get_or_default_string = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE;
    let program_has_get_or_default_i32 = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. }
                )
            })
        })
    });
    let program_has_get_or_default_string = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultString { .. }
                )
            })
        })
    });
    if receipt_is_get_or_default_i32 || program_has_get_or_default_i32 {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 GetOrDefault<i32> requires the exact CheckedProgram-aware verifier route (receipt_profile='{}', program_get_or_default_i32={program_has_get_or_default_i32})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    if receipt_is_get_or_default_string || program_has_get_or_default_string {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 GetOrDefault<String> requires the exact CheckedProgram-aware verifier route (receipt_profile='{}', program_get_or_default_string={program_has_get_or_default_string})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_map_set_drop_control =
        receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE;
    let program_has_map_set_drop_control =
        crate::core::mir::validate_ordinary_map_owned_any_v2_map_set_drop_control_mir_route(
            program,
        )
        .is_ok();
    if receipt_is_map_set_drop_control || program_has_map_set_drop_control {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 MapSetDropControl requires the exact Checker-aware verifier route (receipt_profile='{}', program_control={program_has_map_set_drop_control})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_branch_local_set_diamond = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE;
    let program_has_branch_local_set_diamond =
        crate::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_mir_route(
            program,
        )
        .is_ok();
    if receipt_is_branch_local_set_diamond || program_has_branch_local_set_diamond {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 BranchLocalSetDiamond requires the exact Checker-aware verifier route (receipt_profile='{}', program_diamond={program_has_branch_local_set_diamond})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_is_empty =
        receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE;
    let program_has_is_empty = mir_program_has_ordinary_map_owned_any_v2_is_empty_action(program);
    if receipt_is_is_empty || program_has_is_empty {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 IsEmpty requires the exact Checker-aware verifier route (receipt_profile='{}', program_is_empty={program_has_is_empty})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_zero_miss_i32 =
        receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE;
    let program_has_zero_miss_i32 =
        mir_program_has_ordinary_map_owned_any_v2_zero_miss_i32_signal(program);
    if receipt_is_zero_miss_i32 || program_has_zero_miss_i32 {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 ZeroMissI32 requires the exact Checker-aware verifier route (receipt_profile='{}', program_has_miss_i32={program_has_zero_miss_i32})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_removed_key_checked_miss = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE;
    let program_has_removed_key_checked_miss =
        mir_program_has_ordinary_map_owned_any_v2_removed_key_checked_miss_signal(program);
    if receipt_is_removed_key_checked_miss || program_has_removed_key_checked_miss {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 RemovedKeyCheckedMiss requires the exact Checker-aware verifier route (receipt_profile='{}', program_has_removed_key_checked_miss={program_has_removed_key_checked_miss})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_clone_isolation =
        receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE;
    let program_has_clone_isolation =
        mir_program_has_ordinary_map_owned_any_v2_clone_action(program);
    if receipt_is_clone_isolation || program_has_clone_isolation {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 CloneIsolation requires the exact Checker-aware verifier route (receipt_profile='{}', program_has_clone={program_has_clone_isolation})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_values_producer_caller = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_PRODUCER_CALLER_PROFILE;
    let program_has_values_producer_caller =
        mir_program_has_ordinary_map_owned_any_v2_values_producer_caller_signal(program);
    if receipt_is_values_producer_caller || program_has_values_producer_caller {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 Values producer/caller requires the exact Checker-aware verifier route (receipt_profile='{}', program_values_producer_caller={program_has_values_producer_caller})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_values_crosscall_string_caller = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_PROFILE;
    let receipt_is_values_crosscall_string_caller_index1 = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE;
    let receipt_is_values_result_crosscall_string_caller = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_PROFILE;
    let receipt_is_values_result_crosscall_string_caller_index1 = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_INDEX1_PROFILE;
    let program_has_values_crosscall_string_caller =
        mir_program_has_ordinary_map_owned_any_v2_values_crosscall_string_caller_signal(program);
    if receipt_is_values_crosscall_string_caller
        || receipt_is_values_crosscall_string_caller_index1
        || program_has_values_crosscall_string_caller
    {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 Values cross-call String caller requires its exact versioned Checker-aware verifier route (receipt_profile='{}', program_crosscall_string_caller={program_has_values_crosscall_string_caller})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    if receipt_is_values_result_crosscall_string_caller
        || receipt_is_values_result_crosscall_string_caller_index1
        || mir_program_has_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_signal(
            program,
        )
    {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 Values Result caller requires its exact V3/V4 CheckedProgram-aware verifier API (receipt_profile='{}')",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_values_heterogeneous_string_index1 = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE;
    let program_has_values_heterogeneous_string_index1 = crate::core::mir::
        validate_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_mir_route(program)
            .is_ok();
    if receipt_is_values_heterogeneous_string_index1
        || program_has_values_heterogeneous_string_index1
    {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 Values heterogeneous String index-one requires the exact Checker-aware verifier route (receipt_profile='{}', program_heterogeneous_values={program_has_values_heterogeneous_string_index1})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let program_has_v2 = mir_program_has_ordinary_map_owned_any_v2_actions(program);
    let receipt_is_checked_get =
        receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE;
    let receipt_is_values_field_move =
        receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE;
    let receipt_is_values_local_index_i32 = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE;
    let receipt_is_values_snapshot_replacement = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE;
    let receipt_is_values_mixed_snapshot = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE;
    let receipt_is_values_mixed_snapshot_remove = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE;
    let receipt_is_computed_key_get =
        receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE;
    let receipt_is_computed_string_key_owner_chain = receipt.profile
        == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE;
    let program_has_computed_string_key_owner_chain = crate::core::mir::
        validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_mir_route(program)
        .is_ok();
    if receipt_is_computed_string_key_owner_chain || program_has_computed_string_key_owner_chain {
        return Err(format!(
            "{}: ComputedStringKeyOwnerChain requires its exact CheckedProgram-aware verifier route (receipt_profile='{}', program_computed_string_key_owner_chain={program_has_computed_string_key_owner_chain})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_direct_print =
        receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE;
    if receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND
        && crate::core::mir::validate_ordinary_map_owned_any_v2_direct_print_mir_route(program)
            .is_ok()
    {
        return Err(format!(
            "{}: exact DirectPrint MIR requires its dedicated route receipt; the broad owner-v2 receipt cannot authorize it",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    let receipt_is_empty_root =
        receipt.profile == crate::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE;
    let program_has_empty_root = program.functions().values().any(|function| {
        function
            .ownership
            .has_map_empty_root_root_queries_v1_receipt()
    });
    if receipt_is_empty_root || program_has_empty_root {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 EmptyRoot Values requires the exact Checker-aware verifier route (receipt_profile='{}', program_empty_root={program_has_empty_root})",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    let receipt_is_v2 = receipt_is_empty_root
        || receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND
        || receipt_is_checked_get
        || receipt_is_direct_print
        || receipt_is_values_field_move
        || receipt_is_values_local_index_i32
        || receipt_is_values_snapshot_replacement
        || receipt_is_values_mixed_snapshot
        || receipt_is_values_mixed_snapshot_remove
        || receipt_is_computed_key_get;
    if program_has_v2 != receipt_is_v2 {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 MIR and route receipt profile must be bound together (program_v2={program_has_v2}, receipt_profile='{}')",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    if receipt_is_checked_get {
        crate::core::mir::validate_ordinary_map_owned_any_v2_checked_get_route(program)
            .map_err(|message| {
                format!(
                    "{}: CheckedGet route receipt does not match its exact operation shape: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
    }
    if receipt_is_direct_print {
        crate::core::mir::validate_ordinary_map_owned_any_v2_direct_print_mir_route(program)
            .map_err(|message| {
                format!(
                    "{}: DirectPrint route receipt does not match its exact operation shape: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
    }
    if receipt_is_values_field_move {
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_field_move_mir_route(program).map_err(
            |message| {
                format!(
                    "{}: Values field-move route receipt does not match its exact operation shape: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            },
        )?;
    }
    if receipt_is_values_local_index_i32 {
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_mir_route(
            program,
        )
        .map_err(|message| {
            format!(
                "{}: Values local-index-i32 route receipt does not match its exact operation shape: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    }
    if receipt_is_values_snapshot_replacement {
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_snapshot_replacement_mir_route(program)
            .map_err(|message| {
                format!(
                    "{}: Values snapshot-replacement route receipt does not match its exact operation shape: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
    }
    if receipt_is_values_mixed_snapshot {
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_mir_route(
            program,
        )
        .map_err(|message| {
            format!(
                "{}: Values mixed-snapshot route receipt does not match its exact operation shape: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    }
    if receipt_is_values_mixed_snapshot_remove {
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_mir_route(
            program,
        )
        .map_err(|message| {
            format!(
                "{}: Values mixed-snapshot-remove route receipt does not match its exact operation shape: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    }
    if receipt_is_computed_key_get {
        crate::core::mir::validate_ordinary_map_owned_any_v2_computed_key_get_mir_route(program)
            .map_err(|message| {
                format!(
                    "{}: computed-key Get route receipt does not match its exact operation shape: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
    }
    Ok(())
}

/// Enforce that one MIR verifier result batch carries one immutable execution
/// identity. A verifier emits one result per contract-bearing function and
/// one result per FFI call-site; checking only the first result would allow a
/// future aggregation change to silently mix source, MIR, or route witnesses
/// across the batch. Results without an artifact are inconclusive observations
/// and therefore have no provenance to validate.
fn validate_mir_result_provenance(
    results: &[VerificationResult],
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: &str,
    consumer: &str,
) -> Result<(), String> {
    for (index, result) in results.iter().enumerate() {
        let Some(artifact) = result.artifact.as_ref() else {
            continue;
        };
        if artifact.engine != ProofArtifact::ENGINE_MIR {
            return Err(format!(
                "MIR-PROVENANCE-001: {consumer} result {index} for '{}' carries engine '{}'",
                result.func_name, artifact.engine
            ));
        }
        if artifact.semantics_version != ProofArtifact::SEMANTICS_VERSION {
            return Err(format!(
                "MIR-PROVENANCE-001: {consumer} result {index} for '{}' carries unsupported semantics_version {}",
                result.func_name, artifact.semantics_version
            ));
        }
        let ordinary_mir_scope = artifact.proof_profile == ProofArtifact::PROFILE_MIR_V1
            && (artifact.obligation_scope == ProofArtifact::SCOPE_MIR_FUNCTION_CONTRACTS_V1
                || artifact.obligation_scope == ProofArtifact::SCOPE_MIR_DEFAULT_ROOT_V1);
        let ffi_mir_scope = artifact.proof_profile
            == ProofArtifact::PROFILE_MIR_FFI_EXPERIMENTAL_V1
            && (artifact.obligation_scope == ProofArtifact::SCOPE_FFI_REQUIRES_ONLY_V1
                || artifact.obligation_scope == ProofArtifact::SCOPE_FFI_REQUIRES_ENSURES_V1
                || artifact.obligation_scope == ProofArtifact::SCOPE_LITERAL_FFI_DECLARATION_V1);
        let expected_float_model = if ordinary_mir_scope {
            crate::core::mir::types::MIR_VERIFIER_FLOAT_MODEL
        } else if ffi_mir_scope {
            crate::core::mir::types::MIR_FFI_VERIFIER_FLOAT_MODEL
        } else {
            return Err(format!(
                "MIR-PROVENANCE-001: {consumer} result {index} for '{}' carries unsupported proof identity ({})",
                result.func_name,
                artifact.identity_label()
            ));
        };
        if artifact.float_model != expected_float_model {
            return Err(format!(
                "MIR-PROVENANCE-001: {consumer} result {index} for '{}' carries float model '{}' inconsistent with {}",
                result.func_name, artifact.float_model, artifact.proof_profile
            ));
        }
        if artifact.source_hash != source_hash {
            return Err(format!(
                "MIR-PROVENANCE-001: {consumer} result {index} for '{}' carries a different source_hash",
                result.func_name
            ));
        }
        if artifact.mir_hash != receipt.mir_digest {
            return Err(format!(
                "MIR-PROVENANCE-001: {consumer} result {index} for '{}' carries a different mir_hash",
                result.func_name
            ));
        }
        if artifact.mir_route_receipt.as_ref() != Some(receipt) {
            return Err(format!(
                "MIR-PROVENANCE-001: {consumer} result {index} for '{}' carries a different route receipt",
                result.func_name
            ));
        }
    }
    Ok(())
}

/// Replay a canonical MIR verification from the line-oriented receipt
/// manifest emitted by `mimi mir --receipt`. Parsing is strict and the
/// reconstituted receipt is still checked against the immutable MIR graph
/// before any proof observation is produced.
pub fn verify_mir_with_route_manifest(
    program: &crate::core::mir::reference::MirProgram,
    manifest: &str,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let receipt = crate::core::mir::CanonicalMirRouteReceipt::from_manifest_round_trip(manifest)
        .map_err(|message| {
            format!(
                "{}: canonical route manifest rejected: {message}",
                crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE
            )
        })?;
    verify_mir_with_route_receipt(program, &receipt, source_hash)
}

/// Decide whether verifier observations are compatible with an execution
/// route.  This is not a proof verdict: `NotInTrustedSubset` remains visible
/// to callers and is only tolerated for the two runtime-only boundaries whose
/// native and bytecode consumers have an executable MIR contract — the
/// recoverable Flow/f64 island and the multi-target Flow union face.
pub fn canonical_execution_route_verifier_ready(
    results: &[VerificationResult],
    allow_recoverable_float_boundary: bool,
    allow_flow_union_boundary: bool,
) -> bool {
    if !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY
        && results.iter().any(|result| {
            result.artifact.as_ref().is_some_and(|artifact| {
                artifact.mir_route_receipt.as_ref().is_some_and(|receipt| {
                    receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND
                })
            })
        })
    {
        return false;
    }
    let runtime_only_boundary = |result: &VerificationResult| {
        result.status == VerifStatus::NotInTrustedSubset
            && ((allow_recoverable_float_boundary
                && result
                    .message
                    .contains(crate::core::mir::types::MIR_VERIFIER_FLOAT_BOUNDARY_CODE))
                || (allow_flow_union_boundary
                    && result
                        .message
                        .contains(crate::core::mir::types::MIR_VERIFIER_FLOW_UNION_BOUNDARY_CODE)))
    };
    results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        ) || runtime_only_boundary(result)
    })
}

/// Check whether a canonical MIR program is fully consumable by the current
/// MIR verifier capability.  This is a structural route gate, not a contract
/// verdict; callers use it before selecting a default producer/consumer
/// island.
pub fn validate_mir_capabilities(
    program: &crate::core::mir::reference::MirProgram,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities(program)
}

/// Validate only the direct tuple Get L2 semantic projection capability. It
/// is deliberately separate from generic MIR capability and execution APIs.
pub fn validate_mir_capabilities_with_ordinary_map_direct_tuple_get_unpack_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_direct_tuple_get_unpack_l2_verifier_route(
        checked,
        program,
        route_receipt,
    )
}

/// Preflight the nested Result-match DirectPrint route against its exact
/// CheckedProgram receipt, expected MIR graph, TypeDesc catalog, and route
/// identity. The generic MIR capability path remains closed to this profile.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_direct_print_result_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_direct_print_result_route(
        checked,
        program,
        route_receipt,
    )
}

/// Preflight the exact three-path MapSetDropControl Result route. The generic
/// MIR capability gate stays closed, and this route refuses contract proof
/// obligations because Result status/tag semantics are not modeled.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
        checked,
        program,
        route_receipt,
    )
}

/// Check the exact CheckedProgram-bound Set/GetAny-i32 L2 verifier
/// projection without running contracts. This preflight is suitable for
/// execution-route selection on the contract-free L1 source shape; only the
/// dedicated proof entry below may return a contract verdict.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_set_get_any_i32_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_set_get_any_i32_l2_verifier_route(
        checked,
        program,
        route_receipt,
    )
}

/// Preflight the independent Result-valued GetAny L2 projection without
/// running a contract proof. The exact CheckedProgram receipt and route
/// identity are checked by this capability gate; only the dedicated proof
/// entry above can return a contract verdict.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_set_get_any_result_i32_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_set_get_any_result_i32_l2_verifier_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the complete MIR verifier capability gate for the narrowly admitted
/// AnyOwner-v2 DirectPrint profile. This consumes and validates the route
/// receipt and the source-bound direct-print operation proof.
pub fn validate_mir_capabilities_with_direct_print_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_direct_print_route_receipt(
        program,
        route_receipt,
    )
}

/// Run the whole-program MIR capability gate for the narrow Checker-sealed
/// owner-v2 IsEmpty profile. The checked source and MIR route receipt are
/// both required; generic capability validation remains closed to IsEmpty.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_is_empty_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_is_empty_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the whole-program capability gate for the source-bound RecordUnit
/// profile, including its exact borrowed String output operation.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_record_unit_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_record_unit_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the strict whole-program verifier capability gate for the
/// Checker-sealed BranchLocalSetDiamond slice.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the whole-program verifier capability gate for the narrow Checker-
/// sealed zero-miss i32 profile. Generic capability validation stays closed
/// to a canonical i32 miss unpack.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the dedicated CheckedProgram-bound capability gate for the fused
/// `get_or_default<i32>` operation. The generic capability API remains
/// closed to this cross-call DynamicAny unpack.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the dedicated CheckedProgram-bound capability gate for the distinct
/// GetOrDefault<String> profile. The generic capability API remains closed
/// to this String-payload Any cast and its canonical i32 miss token.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_get_or_default_string_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_get_or_default_string_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the exact CheckedProgram-bound capability gate for DynamicKeysGet.
/// Generic capability validation remains closed to argv projection and
/// runtime-key Keys snapshots.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_dynamic_keys_get_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_dynamic_keys_get_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the source-bound whole-program capability gate for the computed
/// String-key owner-chain profile. The generic route cannot authorize its
/// Map/Any action sequence or late independent GetAny snapshot.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the dedicated capability gate for the persistent-remove checked-miss
/// profile. Generic MIR capability validation remains closed to its canonical
/// i32 miss authority.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the whole-program capability gate for the narrow CheckedProgram-
/// bound Values producer/caller transfer. The raw capability API remains
/// closed to cross-function Values ownership.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_producer_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_producer_caller_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the whole-program capability gate for the exact Checker-bound Values
/// cross-call String extraction. The raw capability API remains closed to
/// cross-function Values ownership and indexed extraction.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the whole-program verifier capability gate for the distinct V2
/// cross-call String profile with two sorted entries and literal index one.
/// This receipt does not authorize the V1 index-zero route or generic MIR.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the whole-program verifier capability gate for the exact V3
/// Result-branch producer and index-zero String caller. This API uses a
/// separate Result-aware flag and cannot authorize straight-line V1/V2.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the independent V4 Result-branch verifier capability gate for two
/// sorted entries and literal index one.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the whole-program verifier capability gate for the exact
/// Checker-sealed immutable i32-zero Values index route.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_local_index_i32_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_local_index_i32_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the whole-program verifier capability gate for the distinct nested
/// Result form of the immutable i32-zero Values index route.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the whole-program verifier capability gate for the exact Checker-
/// sealed heterogeneous Values String index-one route.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the whole-program verifier capability gate for the exact
/// Checker-sealed owned Keys snapshot and its index-zero String copy.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_keys_snapshot_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_keys_snapshot_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the whole-program verifier capability gate for the narrow
/// Checker-sealed Map-root producer return and its unique direct caller. The
/// raw capability API stays closed to aggregate root returns.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
        checked,
        program,
        route_receipt,
    )
}

/// Run the whole-program capability gate for the exact Checker-sealed
/// Map-root → GetAny → checked String-unpack transfer. The raw capability
/// API remains closed to cross-function Map-root ownership.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &crate::core::mir::reference::MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
        checked,
        program,
        route_receipt,
    )
}

fn parse_memory_source(source: &str, label: &str) -> Result<crate::ast::File, String> {
    let tokens = crate::lexer::Lexer::new(source).tokenize()?;
    crate::parser::Parser::new_memory(tokens, "verifier.source", label, source)
        .map_err(|error| error.to_string())?
        .parse_file()
        .map_err(|error| error.message)
}

/// Build the retained FFI compatibility input while omitting only the injected
/// stdlib prelude bodies. The prelude remains present in `CheckedProgram` for
/// type resolution and route construction; its implementation details are not
/// user proof targets. The public checked contract route never uses this AST
/// snapshot; only the still-retained FFI declaration adapter does.
/// Keep checked extern declarations available to the ordinary Flow body
/// verifier while removing their foreign Requires/Ensures from that verifier's
/// assumption and declaration-proof surface. Actor compatibility verification
/// checks called Requires through the dedicated FFI owner and never treats a
/// foreign Ensures as evidence about an implementation.
fn is_actor_proof_compatibility(program: &crate::core::CheckedProgram) -> bool {
    crate::core::mir::scalar_actor_contract_boundary_reason(program).is_some()
}

/// Turn a checker-owned typed verification boundary into an explicit result.
///
/// A deferred request is not permission to reopen the retained surface AST:
/// the boundary already names the owner and the unsupported proof shape.  The
/// public checked verifier therefore reports one deterministic
/// `NotInTrustedSubset` result per owner and leaves the legacy body owner
/// unreachable.  Canonical MIR profiles are selected before this adapter and
/// never produce a deferred result.
fn deferred_verification_results(
    program: &crate::core::CheckedProgram,
    boundaries: &[crate::core::mir::verification::MirVerificationBoundary],
) -> Vec<VerificationResult> {
    use std::collections::BTreeMap;

    let mut reasons = BTreeMap::<crate::core::NodeId, Vec<&str>>::new();
    for boundary in boundaries {
        reasons
            .entry(boundary.owner.clone())
            .or_default()
            .push(boundary.reason.as_str());
    }
    // A request-level boundary makes the whole proof batch deferred. Preserve
    // every affected callable owner in the result set, including callers that
    // only carry a checker-owned precondition obligation and therefore do not
    // have a direct contract node of their own.
    for (owner, callable) in program.callables() {
        let prelude = program.functions().get(owner).is_some_and(|function| {
            program
                .source_registry()
                .key(function.origin.user_span().source_id)
                .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi")
        });
        if prelude
            || (!callable.has_proof_obligations()
                && !crate::verifier::resolved_expr::has_resolved_callsite_obligations(
                    callable,
                    program.callables(),
                    program.resolved_types(),
                ))
        {
            continue;
        }
        reasons
            .entry(owner.clone())
            .or_default()
            .push("the request contains an unresolved checker boundary");
    }
    reasons
        .into_iter()
        .map(|(owner, reasons)| {
            let name = program
                .functions()
                .get(&owner)
                .map(|function| function.qualified_name.clone())
                .or_else(|| {
                    program.transitions().values().find_map(|transition| {
                        (transition.node_id == owner)
                            .then(|| format!("{}::{}", transition.id.flow.0, transition.id.event))
                    })
                })
                .unwrap_or_else(|| owner.0.clone());
            let reason = reasons.join("; ");
            let message = format!(
                "{}: typed verifier request for '{}' remains deferred: {}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                name,
                reason
            );
            let span = program
                .node_meta()
                .get(&owner)
                .map(|meta| meta.origin.user_span())
                .or_else(|| program.entry_span())
                .unwrap_or(crate::span::Span::UNKNOWN);
            VerificationResult {
                func_name: name,
                status: VerifStatus::NotInTrustedSubset,
                message: message.clone(),
                diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    message,
                    span,
                )),
                duration_us: 0,
                constraint_count: 0,
                artifact: None,
                trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
            }
        })
        .collect()
}

/// Enforce the checked verifier's owner-scoped V2 coverage boundary. MIR
/// observations remain useful for staged diagnostics, but neither staged nor
/// incomplete coverage may leave a proof-shaped result for one of its source
/// owners. Results for unrelated callable owners stay intact alongside these
/// explicit boundaries.
pub(super) fn enforce_ordinary_map_owned_any_v2_coverage(
    program: &crate::core::CheckedProgram,
    mut results: Vec<VerificationResult>,
) -> Vec<VerificationResult> {
    let admission =
        crate::core::mir::classify_canonical_mir_route_admission(program).ordinary_map_owned_any_v2;
    let (coverage, readiness_boundary) = match admission {
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::StagedGetValues => {
            ("StagedGetValues", "V2 production readiness is disabled")
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage => {
            ("IncompleteCoverage", "V2 production readiness is disabled")
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage
            if !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY =>
        {
            (
                "CompleteCoverage (production readiness disabled)",
                "V2 production readiness is disabled",
            )
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CheckedGet => {
            // A generic V2 result batch has no authority over this separate
            // profile. Keep it diagnostic if it reaches the broad-profile
            // readiness adapter; the exact CheckedGet path bypasses this gate.
            (
                "CheckedGet",
                "the exact CheckedGet verifier receipt is required",
            )
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::SetGetAnyI32 => (
            "SetGetAnyI32",
            "the exact CheckedProgram-aware Set/GetAny-i32 verifier receipt is required",
        ),
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::SetGetAnyResultI32 => (
            "SetGetAnyResultI32",
            "the exact CheckedProgram-aware Result-valued Set/GetAny-i32 verifier receipt is required",
        ),
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::DirectPrint => (
            "DirectPrint",
            "the exact DirectPrint verifier receipt is required",
        ),
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::DirectPrintResult => {
            // The dedicated Checker-bound L2 route returns a neutral
            // observation before reaching this broad owner-v2 readiness gate.
            return results;
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControlResult => {
            // The exact Checker-bound route returns a neutral observation
            // before reaching broad owner-v2 readiness. It carries no Result
            // status/tag proof.
            return results;
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedKeyGet => (
            "ComputedKeyGet",
            "the exact computed-key Get verifier receipt is required",
        ),
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedStringKeyOwnerChain => (
            "ComputedStringKeyOwnerChain",
            "the exact CheckedProgram-aware ComputedStringKeyOwnerChain verifier receipt is required",
        ),
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapRootProducerCaller => (
            "MapRootProducerCaller",
            "the exact MapRootProducerCaller verifier receipt is required",
        ),
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesCrosscallStringCaller => (
            "ValuesCrosscallStringCaller",
            "the exact Values cross-call String verifier receipt is required",
        ),
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesCrosscallStringCallerIndex1 => (
            "ValuesCrosscallStringCallerIndex1",
            "the exact V2 Values cross-call String caller verifier receipt is required",
        ),
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesResultProducerCaller => (
            "ValuesResultProducerCaller",
            "NotInTrustedSubset: a CheckedProgram-aware Result producer/caller theorem route is not implemented",
        ),
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::DynamicKeysGet => {
            // The exact CheckedProgram-aware route verifier is the only
            // authority for argv projection and runtime-key String output.
            // This broad result adapter must never mint or downgrade that
            // dedicated route's proof rows.
            return results;
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapRootGetAnyCaller
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultI32
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultString => return results,
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapEmptyRootRootQueriesV1
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IsEmpty
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RecordUnit
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControl
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ZeroMissI32
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RemovedKeyCheckedMiss
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesFieldMove
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesLocalIndexI32
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesLocalIndexI32ResultBranch
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesHeterogeneousStringIndex1
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesSnapshotReplacement
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshot
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshotRemove
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesProducerCaller
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesResultCrosscallStringCaller
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesResultCrosscallStringCallerIndex1
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::KeysSnapshot => {
            return results;
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::BranchLocalPersistentSetDiamond => {
            // Public checked-verifier entry points dispatch this admission to
            // the dedicated Checker-aware profile verifier before reaching
            // this broad-V2 result adapter. That route has already validated
            // its exact route receipt and strict capability census, so keep
            // its proof results outside the generic readiness downgrade.
            return results;
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation => {
            // Checked entry points select CloneIsolation through its own
            // source-bound profile before reaching this broad-coverage
            // downgrade. Keep that result outside generic V2 readiness.
            return results;
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::PersistentRootAliasReplacement => {
            // Checked entry points select this source-bound profile through
            // its dedicated verifier before reaching the broad-V2 downgrade.
            return results;
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::OutsideProfile
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage => return results,
    };
    let owners = program
        .resource_analyses()
        .iter()
        .filter_map(|(owner, analysis)| {
            (!analysis.ordinary_map_owned_any_v2_actions.is_empty()
                || !analysis
                    .ordinary_map_owned_any_v2_operation_attempts
                    .is_empty())
            .then_some(owner.clone())
        })
        .collect::<std::collections::BTreeSet<_>>();
    for owner in owners {
        let name = program
            .functions()
            .get(&owner)
            .map(|function| function.qualified_name.clone())
            .unwrap_or_else(|| owner.0.clone());
        let message = format!(
            "{}: ordinary Map/Any owner-v2 callable '{}' remains NotInTrustedSubset because Checker admission is {coverage} and {readiness_boundary}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            name
        );
        let span = program
            .node_meta()
            .get(&owner)
            .map(|meta| meta.origin.user_span())
            .or_else(|| program.entry_span())
            .unwrap_or(crate::span::Span::UNKNOWN);
        let mut matched = false;
        for result in &mut results {
            // The owner NodeId is the identity boundary. Public result rows
            // use the checked callable's qualified source name, so match it
            // exactly (or its canonical `function:` spelling) rather than
            // relying on substring/name suffix heuristics.
            let result_owner = result
                .func_name
                .strip_prefix("function:")
                .unwrap_or(&result.func_name);
            if result_owner != name && result.func_name != owner.0 {
                continue;
            }
            matched = true;
            result.status = VerifStatus::NotInTrustedSubset;
            result.message = message.clone();
            result.diagnostic = Some(crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                message.clone(),
                span,
            ));
            result.duration_us = 0;
            result.constraint_count = 0;
            result.artifact = None;
            result.trusted_subset_domain = Some(TrustedSubsetDomain::Contract);
        }
        if !matched {
            results.push(VerificationResult {
                func_name: format!("{name} [ordinary-map-owned-any-v2 coverage]"),
                status: VerifStatus::NotInTrustedSubset,
                diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    message.clone(),
                    span,
                )),
                message,
                duration_us: 0,
                constraint_count: 0,
                artifact: None,
                trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
            });
        }
    }
    results
}

/// Make the public MIR proof API's result batch visibly diagnostic while
/// owner-v2 production readiness is closed. The verifier may still analyze
/// the MIR, but neither a proof artifact nor a `Proven` owner row can be fed
/// through the public execution-readiness predicate.
fn enforce_ordinary_map_owned_any_v2_mir_readiness(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    mut results: Vec<VerificationResult>,
) -> Vec<VerificationResult> {
    if crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY
        || receipt.profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND
    {
        return results;
    }
    for (owner, _function) in program.functions().iter().filter(|(_, function)| {
        function
            .ownership
            .has_checker_ordinary_map_owned_any_v2_actions()
    }) {
        let message = format!(
            "{}: ordinary Map/Any owner-v2 callable '{}' is diagnostic-only while V2 production readiness is disabled",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            owner.0
        );
        let mut matched = false;
        for result in &mut results {
            if result.func_name != owner.0 {
                continue;
            }
            matched = true;
            result.status = VerifStatus::NotInTrustedSubset;
            result.message = message.clone();
            result.diagnostic = Some(crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                message.clone(),
                crate::span::Span::UNKNOWN,
            ));
            result.duration_us = 0;
            result.constraint_count = 0;
            result.artifact = None;
            result.trusted_subset_domain = Some(TrustedSubsetDomain::Contract);
        }
        if !matched {
            results.push(VerificationResult {
                func_name: owner.0.clone(),
                status: VerifStatus::NotInTrustedSubset,
                message: message.clone(),
                diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    message,
                    crate::span::Span::UNKNOWN,
                )),
                duration_us: 0,
                constraint_count: 0,
                artifact: None,
                trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
            });
        }
    }
    results
}

fn actor_ffi_error_result(message: String) -> VerificationResult {
    let resolution_error = message.starts_with("TOOL-RESOLUTION-001:");
    VerificationResult {
        func_name: "extern call-site obligations".into(),
        status: if resolution_error {
            VerifStatus::InfrastructureError
        } else {
            VerifStatus::NotInTrustedSubset
        },
        message: format!("FFI call-site verification failed closed: {message}"),
        diagnostic: None,
        duration_us: 0,
        constraint_count: 0,
        artifact: None,
        trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
    }
}

pub(crate) fn actor_ffi_results_or_boundary(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Vec<VerificationResult> {
    match verify_ffi_checked_with_source_hash(program, source_hash) {
        Ok(results) => results,
        Err(message) => vec![actor_ffi_error_result(message)],
    }
}

pub(crate) fn actor_ffi_no_z3_results_or_boundary(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Vec<VerificationResult> {
    match verify_ffi_checked_with_source_hash_and_z3_override(program, source_hash, Some(false)) {
        Ok(results) => results,
        Err(message) => vec![actor_ffi_error_result(message)],
    }
}

/// Build the no-solver result set for the checked Actor compatibility route.
///
/// The ordinary mock verifier is still the result owner for accepted callable
/// proofs, so they stay explicit infrastructure failures with no artifact.
/// Preserve owner-specific checker boundaries alongside those rows: a
/// deferred owner is a coverage result, not a solver failure. FFI remains on
/// its dedicated checked call-site route, which distinguishes Requires from
/// foreign Ensures even when Z3 cannot be created.
fn actor_compatibility_no_z3_results(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Vec<VerificationResult> {
    use std::collections::{BTreeMap, BTreeSet};

    let boundaries = match mir_request::prepare(program) {
        mir_request::Preparation::Deferred(boundaries) => boundaries,
        mir_request::Preparation::Complete(_) => Vec::new(),
    };
    let mut owner_reasons = BTreeMap::<crate::core::NodeId, Vec<String>>::new();
    for boundary in boundaries {
        // The Actor/impl graph boundary applies to the overall compatibility
        // request. Keep it from downgrading every otherwise eligible scalar
        // proof owner; only a boundary attached to that owner becomes NITS.
        if boundary.owner.0 == "verification:request" || boundary.owner.0 == "verification:consumer"
        {
            continue;
        }
        let Some(callable) = program.callable(&boundary.owner) else {
            continue;
        };
        let has_callsite_obligations = resolved_expr::has_resolved_callsite_obligations(
            callable,
            program.callables(),
            program.resolved_types(),
        );
        if !callable.has_proof_obligations() && !has_callsite_obligations {
            continue;
        }
        owner_reasons
            .entry(boundary.owner)
            .or_default()
            .push(boundary.reason);
    }

    let mut deferred = owner_reasons
        .into_iter()
        .map(|(owner, reasons)| {
            let name = program
                .functions()
                .get(&owner)
                .map(|function| function.qualified_name.clone())
                .or_else(|| {
                    program.transitions().values().find_map(|transition| {
                        (transition.node_id == owner)
                            .then(|| format!("{}::{}", transition.id.flow.0, transition.id.event))
                    })
                })
                .unwrap_or_else(|| owner.0.clone());
            let message = format!(
                "{}: typed Actor proof owner '{}' remains outside the admitted proof request: {}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                name,
                reasons.join("; ")
            );
            let span = program
                .node_meta()
                .get(&owner)
                .map(|meta| meta.origin.user_span())
                .or_else(|| program.entry_span())
                .unwrap_or(crate::span::Span::UNKNOWN);
            VerificationResult {
                func_name: name,
                status: VerifStatus::NotInTrustedSubset,
                diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    message.clone(),
                    span,
                )),
                message,
                duration_us: 0,
                constraint_count: 0,
                artifact: None,
                trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
            }
        })
        .collect::<Vec<_>>();

    // Declaration expressions do not have callable entries in the mock
    // result table. Retain proof-bearing user initializers as span-bearing
    // owner boundaries rather than silently dropping them from an Actor
    // proof census. Extern roots are classified by the FFI call-site adapter.
    for root in program.declaration_expression_bodies().values() {
        if matches!(
            &root.kind,
            crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternRequires
                | crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternEnsures
        ) {
            continue;
        }
        let has_obligation = match &root.body {
            Ok(body) => body.has_proof_obligations(),
            Err(_) => true,
        };
        if !has_obligation {
            continue;
        }
        let message = format!(
            "{}: typed Actor declaration proof owner '{}' is outside the admitted proof request",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            root.display_name
        );
        let span = program
            .node_meta()
            .get(&root.node_id)
            .or_else(|| program.node_meta().get(&root.owner))
            .map(|meta| meta.origin.user_span())
            .or_else(|| {
                root.body
                    .as_ref()
                    .ok()
                    .map(|body| body.root.origin.user_span())
            })
            .or_else(|| program.entry_span())
            .unwrap_or(crate::span::Span::UNKNOWN);
        deferred.push(VerificationResult {
            func_name: root.display_name.clone(),
            status: VerifStatus::NotInTrustedSubset,
            diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                message.clone(),
                span,
            )),
            message,
            duration_us: 0,
            constraint_count: 0,
            artifact: None,
            trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
        });
    }

    let deferred_names = deferred
        .iter()
        .map(|result| result.func_name.clone())
        .collect::<BTreeSet<_>>();
    let mut results = ctx::mock_verify_checked(program);
    results.retain(|result| {
        !result.func_name.starts_with("extern ") && !deferred_names.contains(&result.func_name)
    });
    results.extend(deferred);
    // Keep the FFI checker’s identity, arity, and composition checks while
    // carrying the no-Z3 decision into that route. It must not probe a
    // host-installed solver again and emit a proof from this result path.
    results.extend(actor_ffi_no_z3_results_or_boundary(program, source_hash));
    results
}

/// Verify contracts in source text.
pub fn verify_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    let file = parse_memory_source(source, "contracts")?;
    let program = crate::core::check_program(&file).map_err(format_check_errors)?;
    // P1-24: compute source hash for tamper detection in ProofArtifact.
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    verify_checked(&program, source_hash)
}

/// Verify contracts using a caller-provided verifier (for timeout/config tests).
pub fn verify_source_with(
    source: &str,
    verifier: &mut Verifier,
) -> Result<Vec<VerificationResult>, String> {
    let file = parse_memory_source(source, "contracts")?;
    let program = crate::core::check_program(&file).map_err(format_check_errors)?;
    // P1-24: compute source hash for tamper detection in ProofArtifact.
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    if let Some(reason) = program.dynamic_protocol_method_boundary_reason() {
        return Err(reason);
    }
    // A caller-supplied verifier is the retained compatibility engine used by
    // timeout/configuration tests.  Once Checker admission recognizes an
    // ordinary Map owned-get candidate, keep this source entry on the same
    // canonical route as `verify_checked`; otherwise the configured engine
    // could silently consume the old Resolved body and bypass the owner
    // settlement gate.
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&program);
    if admission.map_root.is_candidate()
        || admission.map_any_owner.is_candidate()
        || admission.ordinary_map_owned_any_v2.has_profile_signal()
        || admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
        || admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal()
    {
        validate_public_verifier_mir_boundary(&program)?;
        return verify_checked(&program, source_hash);
    }
    verifier.set_source_hash(source_hash);
    Ok(verifier.verify_checked(&program))
}

/// Enforce the checker-owned boundaries that prevent a recognized but
/// incomplete Canonical MIR profile from falling through to a compatibility
/// verifier. Keep this shared by the single- and dual-engine public APIs so
/// callers cannot obtain different answers merely by selecting an engine
/// wrapper.
fn validate_public_verifier_mir_boundary(
    program: &crate::core::CheckedProgram,
) -> Result<(), String> {
    if let Some(reason) = program.dynamic_protocol_method_boundary_reason() {
        return Err(reason);
    }
    if crate::core::mir::has_ordinary_map_owned_any_v2_direct_tuple_get_unpack_l2_candidate(program)
    {
        return Err(format!(
            "{}: direct tuple Get L2 requires the dedicated CheckedProgram semantic-projection API; generic checked verification cannot emit a contract verdict for this receipt",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if let Some(reason) = crate::core::mir::scalar_ffi_boundary_reason(program) {
        return Err(format!(
            "{}: canonical scalar FFI declaration boundary: {reason}",
            crate::core::mir::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE
        ));
    }

    // Result projection candidates are checker-owned route admissions. Once
    // the front end recognizes an unsupported shape, verification must not
    // silently enter the retained AST/Flow compatibility engine. Complete
    // admission proceeds through the canonical MIR profile below; mixed or
    // unsupported admission is a stable hard error.
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesResultCrosscallStringCaller
    {
        let route =
            crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
                let code = error
                    .diagnostic_code()
                    .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                format!("{code}: Values Result caller V3 route materialization rejected: {error}")
            })?;
        let receipt = crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
            program,
            &route.program,
        )
        .map_err(|message| {
            format!(
                "{}: Values Result caller V3 CheckedProgram/MIR route rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
        if receipt.profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_PROFILE
        {
            return Err(format!(
                "{}: V3 admission produced a different Result caller profile '{}'",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE,
                receipt.profile
            ));
        }
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
            program,
            &route.program,
            &receipt,
        )
        .map_err(|errors| {
            format!(
                "MIR-CAPABILITY-001: Values Result caller V3 boundary rejected the graph: {errors:?}"
            )
        })?;
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesResultCrosscallStringCallerIndex1
    {
        let route =
            crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
                let code = error
                    .diagnostic_code()
                    .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                format!("{code}: Values Result caller V4 route materialization rejected: {error}")
            })?;
        let receipt = crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
            program,
            &route.program,
        )
        .map_err(|message| {
            format!(
                "{}: Values Result caller V4 CheckedProgram/MIR route rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
        if receipt.profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_INDEX1_PROFILE
        {
            return Err(format!(
                "{}: V4 admission produced a different Result caller profile '{}'",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE,
                receipt.profile
            ));
        }
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_route(
            program,
            &route.program,
            &receipt,
        )
        .map_err(|errors| {
            format!(
                "MIR-CAPABILITY-001: Values Result caller V4 boundary rejected the graph: {errors:?}"
            )
        })?;
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::SetGetAnyResultI32
    {
        let route = crate::core::mir::materialize_ordinary_map_set_get_any_result_i32_verifier_route(
            program,
        )
        .map_err(|reason| {
            format!(
                "{}: ordinary Map Set/GetAny-Result-i32 L2 route materialization rejected: {reason}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
        let receipt = route.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_L2_VERIFIER_PROFILE,
        );
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_set_get_any_result_i32_l2_verifier_route(
            program,
            &route,
            &receipt,
        )
        .map_err(|errors| {
            format!(
                "MIR-CAPABILITY-001: ordinary Map Set/GetAny-Result-i32 L2 verifier capability rejected the graph: {errors:?}"
            )
        })?;
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::SetGetAnyI32
    {
        return Err(format!(
            "{}: ordinary Map Set/GetAny-i32 requires its dedicated CheckedProgram-aware L2 verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if let Some(reason) =
        crate::core::mir::unsupported_nested_callable_boundary_reason(program, None)
    {
        // Actor proof compatibility deliberately retains the complete checker
        // owner census. A dormant nested owner can be outside the scalar
        // execution slice while its proof result still belongs in the
        // compatibility verifier; that verifier keeps the shape explicitly
        // non-Proven. Every other route keeps this checker-owned boundary
        // hard.
        if !is_actor_proof_compatibility(program) {
            return Err(format!(
                "{}: {reason}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
    }
    if admission.map_root.is_candidate() && !admission.map_root_complete() {
        return Err(format!(
            "{}: Checker recognized a MapRoot candidate but withheld its complete local lifecycle receipt",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission.map_any_owner.is_candidate() && !admission.map_any_owner_complete() {
        return Err(format!(
            "{}: Checker recognized a MapAnyOwner v1 candidate but withheld its complete independent lifecycle receipt",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map/Any owner-v2 candidate but withheld complete operation-attempt and result-receipt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IsEmpty
    {
        let route =
            crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
                let code = error
                    .diagnostic_code()
                    .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                format!("{code}: IsEmpty canonical route materialization rejected: {error}")
            })?;
        crate::core::mir::validate_ordinary_map_owned_any_v2_is_empty_route(
            program,
            &route.program,
        )
        .map_err(|message| {
            format!(
                "{}: Checker IsEmpty source/MIR route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RecordUnit
    {
        let route =
            crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
                let code = error
                    .diagnostic_code()
                    .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                format!("{code}: RecordUnit canonical route materialization rejected: {error}")
            })?;
        if !crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RecordUnit
            .is_materialized(&route)
            || route
                .ordinary_map_owned_any_v2_record_unit_route_receipt
                .as_ref()
                != Some(
                    &route.program.route_receipt(
                        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE,
                    ),
                )
        {
            return Err(format!(
                "{}: RecordUnit admission, materialization, and route receipt are not bidirectionally bound",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::validate_ordinary_map_owned_any_v2_record_unit_route(
            program,
            &route.program,
        )
        .map_err(|message| {
            format!(
                "{}: Checker RecordUnit source/MIR route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControl
    {
        let route =
            crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
                let code = error
                    .diagnostic_code()
                    .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                format!(
                    "{code}: MapSetDropControl canonical route materialization rejected: {error}"
                )
            })?;
        crate::core::mir::validate_ordinary_map_owned_any_v2_map_set_drop_control_route(
            program,
            &route.program,
        )
        .map_err(|message| {
            format!(
                "{}: Checker MapSetDropControl source/MIR route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControlResult
    {
        let route =
            crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
                let code = error
                    .diagnostic_code()
                    .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                format!(
                    "{code}: MapSetDropControlResult canonical route materialization rejected: {error}"
                )
            })?;
        let profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_RESULT_BRANCH_PROFILE;
        let canonical_receipt = route.program.route_receipt(profile);
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route_receipt
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "{}: MapSetDropControlResult admission did not produce its exact materialized route receipt",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
        if route.admission.ordinary_map_owned_any_v2
            != crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControlResult
            || !crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControlResult
                .is_materialized(&route)
            || !route
                .materialized_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_candidate
            || materialized_receipt != &canonical_receipt
        {
            return Err(format!(
                "{}: MapSetDropControlResult admission, materialization, and route receipt are not bidirectionally bound",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        let checked_receipt = crate::core::mir::validate_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
            program,
            &route.program,
        )
        .map_err(|message| {
            format!(
                "{}: Checker MapSetDropControlResult source/MIR route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
        if checked_receipt != *materialized_receipt {
            return Err(format!(
                "{}: MapSetDropControlResult materialized receipt differs from its replayed Checker route",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            ));
        }
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
            program,
            &route.program,
            materialized_receipt,
        )
        .map_err(|errors| {
            format!(
                "MIR-CAPABILITY-001: MapSetDropControlResult verifier boundary rejected the graph: {errors:?}"
            )
        })?;
    }
    if admission.ordinary_map_owned_any_v2_branch_local_set_diamond() {
        let route =
            crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
                let code = error
                    .diagnostic_code()
                    .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                format!(
                    "{code}: BranchLocalSetDiamond canonical route materialization rejected: {error}"
                )
            })?;
        if !crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond
            .is_materialized(&route)
            || route
                .ordinary_map_owned_any_v2_branch_local_set_diamond_route_receipt
                .as_ref()
                != Some(&route.program.route_receipt(
                    crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE,
                ))
        {
            return Err(format!(
                "{}: BranchLocalSetDiamond admission, materialization, and route receipt are not bidirectionally bound",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
            program,
            &route.program,
        )
        .map_err(|message| {
            format!(
                "{}: Checker BranchLocalSetDiamond source/MIR route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ZeroMissI32
    {
        let route =
            crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
                let code = error
                    .diagnostic_code()
                    .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                format!("{code}: ZeroMissI32 canonical route materialization rejected: {error}")
            })?;
        crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_route(
            program,
            &route.program,
        )
        .map_err(|message| {
            format!(
                "{}: Checker ZeroMissI32 source/MIR route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RemovedKeyCheckedMiss
    {
        let route =
            crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
                let code = error
                    .diagnostic_code()
                    .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                format!(
                    "{code}: RemovedKeyCheckedMiss canonical route materialization rejected: {error}"
                )
            })?;
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_removed_key_checked_miss_route_receipt
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "{}: RemovedKeyCheckedMiss admission did not produce its exact materialized route receipt",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
        let canonical_receipt = route.program.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE,
        );
        if route.admission.ordinary_map_owned_any_v2
            != crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RemovedKeyCheckedMiss
            || !crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RemovedKeyCheckedMiss
                .is_materialized(&route)
            || !route
                .materialized_ordinary_map_owned_any_v2_removed_key_checked_miss_candidate
            || materialized_receipt != &canonical_receipt
        {
            return Err(format!(
                "{}: RemovedKeyCheckedMiss admission, materialization, and route receipt are not bidirectionally bound",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::validate_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
            program,
            &route.program,
        )
        .map_err(|message| {
            format!(
                "{}: Checker RemovedKeyCheckedMiss source/MIR route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    }
    if admission
        .ordinary_map_owner_lifecycle_observer_v1
        .has_profile_signal()
        && !admission.ordinary_map_owner_lifecycle_observer_v1_complete()
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map lifecycle observer-v1 attempt without complete independent observer receipt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission
        .ordinary_map_owned_get_i32_string_v1
        .has_profile_signal()
        && !admission.ordinary_map_owned_get_i32_string_v1_complete()
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map owned-get attempt without complete consumer coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if let Some(reason) =
        crate::core::mir::ordinary_map_owned_get_result_settlement_reason_checked(program)
    {
        return Err(format!(
            "{}: {reason}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    // Ordinary Map attempts outside the first owner lifecycle slice remain
    // on the legacy verifier by design. Direct MIR construction/materializing
    // APIs still reject this partial graph, while a separate complete V1
    // receipt may cover every ordinary attempt through its explicit bridge.
    if !admission.scalar_ffi
        && matches!(
            admission.flow,
            crate::core::mir::S8FlowAdmission::MixedCoverage
        )
    {
        return Err(
            "MIR-COVERAGE-001: S8 Flow transition candidate is not complete coverage".into(),
        );
    }
    if matches!(
        admission.generic_variant,
        crate::core::mir::GenericVariantPredicateAdmission::MixedCoverage
    ) || (!admission.generic_variant_complete()
        && crate::core::mir::has_generic_variant_predicate_operation_candidate(program))
    {
        return Err(
            "MIR-COVERAGE-001: generic variant predicate candidate is outside complete coverage"
                .into(),
        );
    }
    if matches!(
        admission.record,
        crate::core::mir::FlatCopyRecordAdmission::MixedCoverage
    ) && crate::core::mir::has_unsupported_generic_record_projection_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: generic record projection candidate did not materialize a supported scalar MIR shape"
                .into(),
        );
    }
    if matches!(
        admission.record,
        crate::core::mir::FlatCopyRecordAdmission::MixedCoverage
    ) && crate::core::mir::has_unsupported_generic_record_update_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: generic record update candidate did not materialize a supported scalar MIR shape"
                .into(),
        );
    }
    if !matches!(
        admission.collection,
        crate::core::mir::ScalarCollectionAdmission::OutsideProfile
    ) && crate::core::mir::has_unsupported_list_reverse_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: canonical List.reverse candidate did not materialize a supported MIR shape"
                .into(),
        );
    }
    if !matches!(
        admission.collection,
        crate::core::mir::ScalarCollectionAdmission::OutsideProfile
    ) && crate::core::mir::has_unsupported_list_concat_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: canonical List.concat candidate did not materialize a supported MIR shape"
                .into(),
        );
    }
    if !matches!(
        admission.collection,
        crate::core::mir::ScalarCollectionAdmission::OutsideProfile
    ) && crate::core::mir::has_unsupported_generic_list_facade_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: canonical generic List facade candidate did not materialize a supported scalar MIR shape"
                .into(),
        );
    }
    if admission.collection_complete()
        && crate::core::mir::has_unsupported_generic_set_facade_candidate(program)
    {
        return Err("MIR-COVERAGE-001: canonical generic Set facade construction failed".into());
    }
    if matches!(
        admission.generic_option_projection,
        crate::core::mir::GenericOptionProjectionAdmission::MixedCoverage
    ) || crate::core::mir::has_unsupported_generic_option_projection_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: generic Option projection candidate is outside complete coverage"
                .into(),
        );
    }
    if matches!(
        admission.generic_option_projection_fallback,
        crate::core::mir::GenericOptionProjectionFallbackAdmission::MixedCoverage
    ) || crate::core::mir::has_unsupported_generic_option_projection_fallback_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: generic Option fallback projection candidate is outside complete coverage"
                .into(),
        );
    }
    if matches!(
        admission.generic_result_projection,
        crate::core::mir::GenericResultProjectionAdmission::MixedCoverage
    ) || crate::core::mir::has_unsupported_generic_result_projection_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: generic Result projection candidate is outside complete coverage"
                .into(),
        );
    }
    if matches!(
        admission.generic_result_projection_fallback,
        crate::core::mir::GenericResultProjectionFallbackAdmission::MixedCoverage
    ) || crate::core::mir::has_unsupported_generic_result_projection_fallback_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: generic Result fallback projection candidate is outside complete coverage"
                .into(),
        );
    }
    if matches!(
        admission.managed_result_call,
        crate::core::mir::ManagedResultCallAdmission::MixedCoverage
    ) || (crate::core::mir::has_managed_result_call_candidate(program)
        && !matches!(
            admission.managed_result_call,
            crate::core::mir::ManagedResultCallAdmission::CompleteCoverage
        ))
    {
        return Err(
            "MIR-COVERAGE-001: managed Result direct-call candidate is outside complete coverage"
                .into(),
        );
    }
    for (mixed, profile) in [
        (
            matches!(
                admission.copy_option_i32,
                crate::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
            ),
            "Copy Option<i32>",
        ),
        (
            matches!(
                admission.copy_option_bool,
                crate::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
            ),
            "Copy Option<bool>",
        ),
        (
            matches!(
                admission.copy_option_i64,
                crate::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
            ),
            "Copy Option<i64>",
        ),
        (
            matches!(
                admission.copy_option_f64,
                crate::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
            ),
            "Copy Option<f64>",
        ),
    ] {
        if mixed {
            return Err(format!(
                "MIR-COVERAGE-001: {profile} projection candidate is outside complete coverage"
            ));
        }
    }
    if matches!(
        admission.copy_result_i32,
        crate::core::mir::CopyResultI32VariantAdmission::MixedCoverage
    ) {
        return Err(
            "MIR-COVERAGE-001: Copy Result<i32, i32> projection candidate is outside complete coverage"
                .into(),
        );
    }

    Ok(())
}

/// Verify contracts in a type-checked program (supports pre-merged imports).
///
/// `source_hash` is the BLAKE3 hash of the source text (for ProofArtifact
/// tamper detection). Pass an empty string if source text is unavailable.
///
/// Closed scalar FFI, scalar collection, flat Copy-record, S8 Flow, recoverable
/// Flow, and exact non-Copy `Option<string>` programs are verified from one
/// canonical MIR graph. A typed request outside those profiles returns an
/// explicit checker-owned coverage result; it never reopens a retained
/// surface-body compatibility owner. When Z3 is unavailable, the existing
/// CheckedProgram infrastructure result remains the only fallback.
///
/// The Resolved IR contract path (`verify_checked_contracts`) remains available
/// through callers that hold a configured `Verifier`; this public checked entry
/// uses the canonical MIR request or reports a typed coverage boundary.
fn verify_checked_ordinary_map_owned_any_v2_values_result_crosscall_string_caller(
    program: &crate::core::CheckedProgram,
    source_hash: String,
    index1: bool,
) -> Result<Vec<VerificationResult>, String> {
    if !is_z3_available() {
        return Err(format!(
            "{}: Values Result caller V{} requires the canonical MIR verifier, but Z3 is unavailable",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            if index1 { 4 } else { 3 }
        ));
    }
    let route =
        crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
            let code = error
                .diagnostic_code()
                .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
            format!("{code}: Values Result caller route materialization rejected: {error}")
        })?;
    let receipt = crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
        program,
        &route.program,
    )
    .map_err(|message| {
        format!(
            "{}: Values Result caller CheckedProgram/MIR route rejected: {message}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;
    let expected_profile = if index1 {
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_INDEX1_PROFILE
    } else {
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_PROFILE
    };
    if receipt.profile != expected_profile {
        return Err(format!(
            "{}: Values Result caller route selected profile '{}', expected '{expected_profile}'",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE,
            receipt.profile
        ));
    }
    if index1 {
        verify_mir_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_route(
            program,
            &route.program,
            &receipt,
            source_hash,
        )
    } else {
        verify_mir_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
            program,
            &route.program,
            &receipt,
            source_hash,
        )
    }
}

pub fn verify_checked(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    program
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    // Run the checker-owned route boundary before MIR request preparation.
    // `mir_request::prepare` can complete and return directly when Z3 is
    // available; placing this check below that early return would let an
    // incomplete ordinary Map/GetAny or Values owner reach a proof engine.
    validate_public_verifier_mir_boundary(program)?;
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControlResult
    {
        return verify_checked_ordinary_map_owned_any_v2_map_set_drop_control_result(
            program,
            source_hash,
        );
    }
    let z3_available = is_z3_available();
    let actor_compatibility = is_actor_proof_compatibility(program);
    if admission.ordinary_map_owned_any_v2.is_direct_print_result() {
        return verify_checked_ordinary_map_owned_any_v2_direct_print_result(program, source_hash);
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesResultCrosscallStringCaller
    {
        return verify_checked_ordinary_map_owned_any_v2_values_result_crosscall_string_caller(
            program,
            source_hash,
            false,
        );
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesResultCrosscallStringCallerIndex1
    {
        return verify_checked_ordinary_map_owned_any_v2_values_result_crosscall_string_caller(
            program,
            source_hash,
            true,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::SetGetAnyResultI32
    {
        let route =
            crate::core::mir::materialize_ordinary_map_set_get_any_result_i32_verifier_route(
                program,
            )
            .map_err(|reason| {
                format!(
                    "{}: ordinary Map Set/GetAny-Result-i32 L2 verifier route rejected: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
        let receipt = route.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_L2_VERIFIER_PROFILE,
        );
        return verify_mir_with_ordinary_map_owned_any_v2_set_get_any_result_i32_l2_verifier_route(
            program,
            &route,
            &receipt,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapEmptyRootRootQueriesV1
    {
        return verify_checked_map_empty_root_root_queries_v1(program, source_hash);
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapRootGetAnyCaller
    {
        return verify_checked_ordinary_map_owned_any_v2_map_root_get_any_caller(
            program,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultI32
    {
        return verify_checked_ordinary_map_owned_any_v2_get_or_default_i32(program, source_hash);
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultString
    {
        return verify_checked_ordinary_map_owned_any_v2_get_or_default_string(
            program,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IsEmpty
    {
        return verify_checked_ordinary_map_owned_any_v2_is_empty(program, source_hash);
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RecordUnit
    {
        return verify_checked_ordinary_map_owned_any_v2_record_unit(program, source_hash);
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControl
    {
        return verify_checked_ordinary_map_owned_any_v2_map_set_drop_control(program, source_hash);
    }
    if z3_available && admission.ordinary_map_owned_any_v2_branch_local_set_diamond() {
        return verify_checked_ordinary_map_owned_any_v2_branch_local_set_diamond(
            program,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ZeroMissI32
    {
        return verify_checked_ordinary_map_owned_any_v2_zero_miss_i32(program, source_hash);
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RemovedKeyCheckedMiss
    {
        return verify_checked_ordinary_map_owned_any_v2_removed_key_checked_miss(
            program,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation
    {
        return verify_checked_ordinary_map_owned_any_v2_clone_isolation(program, source_hash);
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::PersistentRootAliasReplacement
    {
        return verify_checked_ordinary_map_owned_any_v2_persistent_root_alias_replacement(
            program,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CheckedGet
    {
        return verify_checked_ordinary_map_owned_any_v2_get(program, source_hash);
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedKeyGet
    {
        return verify_checked_ordinary_map_owned_any_v2_computed_key_get(program, source_hash);
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedStringKeyOwnerChain
    {
        return verify_checked_ordinary_map_owned_any_v2_computed_string_key_owner_chain(
            program,
            source_hash,
        );
    }
    if z3_available && admission.ordinary_map_owned_any_v2_dynamic_keys_get() {
        return verify_checked_ordinary_map_owned_any_v2_dynamic_keys_get(program, source_hash);
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::DirectPrint
    {
        return verify_checked_ordinary_map_owned_any_v2_direct_print(program, source_hash);
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesFieldMove
    {
        return verify_checked_ordinary_map_owned_any_v2_values_field_move(program, source_hash);
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesLocalIndexI32
    {
        return verify_checked_ordinary_map_owned_any_v2_values_local_index_i32(
            program,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesLocalIndexI32ResultBranch
    {
        return verify_checked_ordinary_map_owned_any_v2_values_local_index_i32_result_branch(
            program,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesHeterogeneousStringIndex1
    {
        return verify_checked_ordinary_map_owned_any_v2_values_heterogeneous_string_index1(
            program,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesCrosscallStringCallerIndex1
    {
        let route = crate::core::mir::materialize_canonical_mir_route(program, None).map_err(
            |error| {
                let code = error
                    .diagnostic_code()
                    .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                format!(
                    "{code}: Values cross-call String caller index-one verifier route failed: {error}"
                )
            },
        )?;
        let receipt = route.program.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE,
        );
        return verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
            program,
            &route.program,
            &receipt,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesSnapshotReplacement
    {
        return verify_checked_ordinary_map_owned_any_v2_values_snapshot_replacement(
            program,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshot
    {
        return verify_checked_ordinary_map_owned_any_v2_values_mixed_snapshot(
            program,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshotRemove
    {
        return verify_checked_ordinary_map_owned_any_v2_values_mixed_snapshot_remove(
            program,
            source_hash,
        );
    }
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::StagedGetValues
    {
        return verify_staged_ordinary_map_owned_any_v2(program, source_hash);
    }
    // A complete owner-v2 profile remains diagnostic-only while the shared
    // production gate is closed. Keep this ahead of the generic typed request
    // fast path so a future request model cannot produce a proof artifact for
    // an owner whose backend route is not production-ready.
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage
        && !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY
    {
        return verify_closed_mir_program(program, source_hash)?.ok_or_else(|| {
            format!(
                "{}: complete ordinary Map/Any owner-v2 diagnostic verifier did not materialize a route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        });
    }
    if z3_available && admission.ordinary_map_owner_lifecycle_observer_v1_complete() {
        return verify_closed_mir_program(program, source_hash)?.ok_or_else(|| {
            format!(
                "{}: complete ordinary Map lifecycle observer-v1 verifier did not materialize its exact route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        });
    }
    let deferred_boundaries = if z3_available {
        match mir_request::prepare(program) {
            mir_request::Preparation::Complete(request) => {
                return mir_request::verify(&request, program, source_hash);
            }
            mir_request::Preparation::Deferred(boundaries) => Some(boundaries),
        }
    } else {
        None
    };
    if !z3_available && actor_compatibility {
        return Ok(actor_compatibility_no_z3_results(program, source_hash));
    }
    if !z3_available {
        // A closed MIR profile cannot produce a proof without its solver.
        // Keep the checked no-solver result algebra instead of materializing
        // that profile and failing during SolverSession initialization.
        return Ok(ctx::mock_verify_checked(program));
    }
    match verify_closed_mir_program(program, source_hash.clone()) {
        Ok(Some(results))
            if deferred_boundaries
                .as_deref()
                .is_some_and(|boundaries| !boundaries.is_empty())
                && results.iter().any(|result| {
                    result
                        .message
                        .contains("proof worklist coverage gap: DeferredBoundary")
                }) =>
        {
            return Ok(deferred_verification_results(
                program,
                deferred_boundaries.as_deref().unwrap_or_default(),
            ));
        }
        Ok(Some(results)) => return Ok(results),
        Ok(None) => {}
        Err(error)
            if deferred_boundaries
                .as_deref()
                .is_some_and(|boundaries| !boundaries.is_empty())
                && error.contains("proof worklist coverage gap: DeferredBoundary") =>
        {
            // The checker has already sealed this owner as a deferred proof
            // boundary.  A later scalar island probe may discover that the
            // same body cannot be materialized, but that is still an
            // owner-scoped coverage result, not a verifier infrastructure
            // failure.  Preserve the typed boundary and never reopen the
            // retained AST/Flow owner.
            return Ok(deferred_verification_results(
                program,
                deferred_boundaries.as_deref().unwrap_or_default(),
            ));
        }
        Err(error) => return Err(error),
    }
    if actor_compatibility {
        if let Some(batch) = flow_mir::verify_actor_flow_projection_v1(program, &source_hash)? {
            let mut verifier = Verifier::new()?;
            verifier.set_source_hash(source_hash);
            let mut results =
                verifier.verify_checked_excluding_owners(program, &batch.projection.owner_scope);
            results.extend(batch.results);
            return Ok(results);
        }
        let mut verifier = Verifier::new()?;
        verifier.set_source_hash(source_hash);
        return Ok(verifier.verify_checked(program));
    }
    if let Some(boundaries) = deferred_boundaries.as_deref() {
        // A typed request that did not reach a closed MIR profile is an
        // explicit coverage boundary. Do not reconstruct a surface file: the checker
        // has already identified the exact owner and unsupported proof shape.
        return Ok(deferred_verification_results(program, boundaries));
    }
    // No callable proof roots were requested. The old Flow verifier used to
    // return an empty batch here after reading the source file; an empty typed
    // result is equivalent and keeps the AST owner unreachable.
    Ok(Vec::new())
}

/// Parse source and verify extern call sites using Z3.
pub fn verify_ffi_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    let file = parse_memory_source(source, "ffi-call-sites")?;
    let program = crate::core::check_program(&file).map_err(format_check_errors)?;
    verify_ffi_checked_with_source_hash(
        &program,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
}

/// Verify extern call sites from a checked program.
///
/// Complete scalar C ABIs consume the shared canonical MIR route and
/// call-site receipts, including scalar `requires`/`ensures` predicates.
/// Declarations outside that profile stay checker-owned as explicit
/// `MIR-COVERAGE-001` observations at their declaration/call-site spans;
/// they never reopen a retained source body. Compatibility never claims an
/// `ensures` proof. Declarations and calls with no FFI obligations return an
/// empty result, while malformed identity/arity and mixed canonical owners
/// fail closed before either verifier route.
pub fn verify_ffi_checked(
    program: &crate::core::CheckedProgram,
) -> Result<Vec<VerificationResult>, String> {
    verify_ffi_checked_with_source_hash(program, String::new())
}

/// Verify FFI obligations in an already-materialized canonical MIR program.
///
/// Build and other route owners use this adapter when they already hold the
/// checker-approved scalar FFI `MirProgram`. Keeping the verifier on that
/// exact object prevents a second frontend/materialization pass from drifting
/// the route receipt between proof and code generation. The input is still
/// validated at the verifier consumer boundary and never opens a legacy AST
/// compatibility owner.
pub fn verify_ffi_mir(
    program: &crate::core::mir::reference::MirProgram,
) -> Result<Vec<VerificationResult>, String> {
    verify_ffi_mir_with_source_hash(program, String::new())
}

/// Verify an already-materialized canonical FFI MIR program while preserving
/// the caller's source provenance in each proof artifact.
///
/// `source_hash` should be the BLAKE3 hash of the source text that produced
/// the checked program. The MIR identity remains independent of this value;
/// callers can therefore compare one canonical route across entry points
/// while still detecting which source snapshot requested the proof.
pub fn verify_ffi_mir_with_source_hash(
    program: &crate::core::mir::reference::MirProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let receipt = program.route_receipt("verifier-mir-v1");
    let results =
        verify_ffi_mir_results_with_route_receipt(program, source_hash.clone(), &receipt)?;
    validate_mir_result_provenance(&results, &receipt, &source_hash, "verify_ffi_mir")?;
    Ok(results)
}

fn verify_ffi_mir_results_with_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    source_hash: String,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<Vec<VerificationResult>, String> {
    crate::verifier::mir_capability::validate_ffi_verifier_mir_capabilities(program).map_err(
        |errors| format!("MIR-FFI-CAPABILITY-001: canonical verifier rejected FFI MIR: {errors:?}"),
    )?;
    mir::verify_ffi_program_with_route_receipt(program, source_hash, receipt)
}

/// Verify canonical FFI MIR after checking the caller-supplied route receipt.
/// The verifier therefore proves the same immutable graph that the execution
/// consumers admitted, including its ABI and FFI sub-digests.
pub fn verify_ffi_mir_with_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_ffi_mir_with_route_receipt_profile(program, receipt, source_hash, true)
}

/// Verify only FFI Requires from the supplied MIR graph. This preserves the
/// foreign Ensures boundary as a separate call-site observation for
/// compatibility owners that do not own the foreign implementation body.
#[cfg(test)]
pub(crate) fn verify_ffi_mir_requires_with_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_ffi_mir_with_route_receipt_profile(program, receipt, source_hash, false)
}

/// Verify the complete FFI route for Requires and only those Ensures call
/// instructions selected from an exact Checker/source-census join. This is
/// used by the verifier-only owner slice, where Ready Ensures can be proved
/// without treating a foreign Requires predicate as an assumption.
pub(crate) fn verify_ffi_mir_with_selected_ensures_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
    ensures_instructions: &std::collections::BTreeSet<crate::core::mir::MirInstructionId>,
) -> Result<Vec<VerificationResult>, String> {
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: canonical route receipt rejected: {message}",
                crate::core::mir::MIR_FFI_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    crate::verifier::mir_capability::validate_ffi_verifier_mir_capabilities(program).map_err(
        |errors| format!("MIR-FFI-CAPABILITY-001: canonical verifier rejected FFI MIR: {errors:?}"),
    )?;
    let results = mir::verify_ffi_program_with_selected_ensures_route_receipt(
        program,
        source_hash.clone(),
        receipt,
        ensures_instructions,
    )?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_ffi_mir_with_selected_ensures_route_receipt",
    )?;
    Ok(results)
}

fn verify_ffi_mir_with_route_receipt_profile(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
    include_ensures: bool,
) -> Result<Vec<VerificationResult>, String> {
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: canonical route receipt rejected: {message}",
                crate::core::mir::MIR_FFI_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    crate::verifier::mir_capability::validate_ffi_verifier_mir_capabilities(program).map_err(
        |errors| format!("MIR-FFI-CAPABILITY-001: canonical verifier rejected FFI MIR: {errors:?}"),
    )?;
    let results = if include_ensures {
        mir::verify_ffi_program_with_route_receipt(program, source_hash.clone(), receipt)?
    } else {
        mir::verify_ffi_requires_program_with_route_receipt(program, source_hash.clone(), receipt)?
    };
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        if include_ensures {
            "verify_ffi_mir_with_route_receipt"
        } else {
            "verify_ffi_mir_requires_with_route_receipt"
        },
    )?;
    Ok(results)
}

/// Replay the FFI-only canonical verifier from a CLI route receipt manifest.
pub fn verify_ffi_mir_with_route_manifest(
    program: &crate::core::mir::reference::MirProgram,
    manifest: &str,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let receipt = crate::core::mir::CanonicalMirRouteReceipt::from_manifest_round_trip(manifest)
        .map_err(|message| {
            format!(
                "{}: canonical route manifest rejected: {message}",
                crate::core::mir::MIR_FFI_ROUTE_MANIFEST_ERROR_CODE
            )
        })?;
    verify_ffi_mir_with_route_receipt(program, &receipt, source_hash)
}

mod ffi_compat;

/// Verify FFI call-site obligations from a checked program and preserve the
/// source snapshot identity in any canonical proof artifacts.
fn scalar_ffi_map_v2_or_observer_composition_failure(
    admission: crate::core::mir::CanonicalMirRouteAdmission,
) -> Option<String> {
    if !admission.scalar_ffi
        || (!admission.ordinary_map_owned_any_v2.has_profile_signal()
            && !admission.map_any_owner.is_candidate()
            && !admission
                .ordinary_map_owner_lifecycle_observer_v1
                .has_profile_signal())
    {
        return None;
    }

    // Reuse the shared Map/FFI composition diagnostic for complete owner
    // profiles, including MapAnyOwner v1. The fallback below closes an
    // incomplete observer-v1 signal, which has no complete profile identity
    // for that shared selector yet.
    crate::core::mir::scalar_ffi_map_owner_composition_failure(admission)
        .map(|error| error.to_diagnostic().message)
        .or_else(|| {
            Some(format!(
                "{}: ordinary Map owner-v2 or observer-v1 profile signal cannot be combined with scalar FFI",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ))
        })
}

pub fn verify_ffi_checked_with_source_hash(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_ffi_checked_with_source_hash_and_z3_override(program, source_hash, None)
}

fn verify_ffi_checked_with_source_hash_and_z3_override(
    program: &crate::core::CheckedProgram,
    source_hash: String,
    z3_available_override: Option<bool>,
) -> Result<Vec<VerificationResult>, String> {
    let inactive_default_call_nodes = checked_ffi_inactive_declaration_default_calls(program)?;
    let declaration_root_results =
        verify_declaration_ffi_source_roots(program, &inactive_default_call_nodes)?;
    let mut results = verify_ffi_checked_body_with_source_hash_and_z3_override(
        program,
        source_hash,
        z3_available_override,
        &inactive_default_call_nodes,
    )?;
    if let Some(mut root_results) = declaration_root_results {
        results.append(&mut root_results);
    }
    Ok(results)
}

fn verify_ffi_checked_body_with_source_hash_and_z3_override(
    program: &crate::core::CheckedProgram,
    source_hash: String,
    z3_available_override: Option<bool>,
    inactive_default_call_nodes: &std::collections::BTreeSet<crate::core::NodeId>,
) -> Result<Vec<VerificationResult>, String> {
    // CheckedProgram owns declaration identity and arity. Validate them before
    // deciding whether this API has an obligation, so malformed call-site
    // metadata cannot be hidden by an otherwise empty result.
    let mut has_called_extern = false;
    let mut called_contract_names = std::collections::BTreeSet::new();
    let mut called_requires_names = std::collections::BTreeSet::new();
    let mut unmodeled_ensures_call_sites = Vec::new();
    for site in program.call_sites_sorted() {
        if inactive_default_call_nodes.contains(&site.node_id)
            || ffi_site_is_declaration_root_call(program, site)
        {
            continue;
        }
        if site.kind != crate::core::ResolvedCallKind::Extern {
            continue;
        }
        has_called_extern = true;
        let same_identity_count = program
            .call_sites()
            .values()
            .filter(|candidate| candidate.node_id == site.node_id)
            .count();
        if !program.call_sites().contains_key(&site.node_id) || same_identity_count != 1 {
            return Err(format!(
                "TOOL-RESOLUTION-001: malformed extern call-site identity '{}'",
                site.node_id.0
            ));
        }
        if !site.arity_matches() {
            return Err(format!(
                "TOOL-RESOLUTION-001: extern call '{}' has inconsistent call-site arity metadata: expected {}, got {}",
                site.callee,
                site.expected_argc.unwrap_or(site.argc),
                site.argc
            ));
        }
        // The CheckedProgram's surface declaration directory remains the
        // authority for symbol identity. The typed MIR descriptor map is
        // built once during checking; a merged or otherwise malformed
        // declaration directory must not be hidden by that earlier snapshot
        // or by a later empty/no-obligation result.
        program
            .extern_func_signature_for_call(&site.callee, site.argc)
            .map_err(|error| format!("TOOL-RESOLUTION-001: {error}"))?;
        let declarations = program
            .ffi_declarations()
            .values()
            .filter(|declaration| declaration.symbol == site.callee)
            .collect::<Vec<_>>();
        if declarations.len() != 1 {
            return Err(format!(
                "TOOL-RESOLUTION-001: extern call '{}' resolves to {} typed declaration descriptors",
                site.callee,
                declarations.len()
            ));
        }
        let declaration = declarations[0];
        declaration
            .validate(program)
            .map_err(|errors| format!("TOOL-RESOLUTION-001: {errors:?}"))?;
        if site.argc != declaration.fixed_parameter_ids.len() {
            return Err(format!(
                "TOOL-RESOLUTION-001: extern call '{}' expects {} arguments, got {}",
                site.callee,
                declaration.fixed_parameter_ids.len(),
                site.argc
            ));
        }
        if declaration.has_declared_contract() {
            called_contract_names.insert(declaration.symbol.clone());
        }
        if declaration.requires.is_declared() {
            called_requires_names.insert(declaration.symbol.clone());
        }
        if declaration.ensures.is_declared() {
            unmodeled_ensures_call_sites.push(site.clone());
        }
    }

    if !has_called_extern {
        // An uncalled extern declaration, or a contract on an unrelated Mimi
        // callable, must not enter an FFI route or manufacture a proof status.
        return Ok(Vec::new());
    }
    let profile = crate::core::mir::CanonicalMirRouteProfile::ScalarFfi;
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if let Some(error) = scalar_ffi_map_v2_or_observer_composition_failure(admission) {
        return Err(error);
    }
    let actor_compatibility = is_actor_proof_compatibility(program);
    // Map-owner profiles have independent canonical ownership receipts. A
    // program that combines one with called externs cannot choose the AST
    // verifier for the FFI portion and a canonical owner for the rest.
    // Incomplete Map attempts also remain visible here so they cannot turn a
    // mixed program into a compatibility route.
    let map_owner_candidate = admission.map_root.is_candidate()
        || admission.map_any_owner.is_candidate()
        || admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
        || admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal();
    if map_owner_candidate {
        if actor_compatibility {
            if called_contract_names.is_empty() {
                return Err(format!(
                    "{}: called externs cannot borrow an independent canonical Map owner receipt from the Actor compatibility route",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ));
            }
            // The Actor compatibility route cannot borrow a separate Map
            // receipt. Preserve each called FFI obligation as explicitly
            // deferred rather than allowing the Actor selector to bypass the
            // same cross-owner guard enforced by canonical Map consumers.
            let mut results = program
                .call_sites_sorted()
                .into_iter()
                .filter(|site| {
                    site.kind == crate::core::ResolvedCallKind::Extern
                        && called_requires_names.contains(&site.callee)
                        && !ffi_site_is_declaration_root_call(program, site)
                })
                .map(|site| ffi_mixed_map_requires_result(program, site))
                .collect::<Vec<_>>();
            results.extend(
                unmodeled_ensures_call_sites
                    .iter()
                    .map(|site| ffi_unmodeled_ensures_result(program, site)),
            );
            return Ok(results);
        }
        return Err(format!(
            "{}: called externs conflict with a separate canonical Map owner profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }

    // A checker-owned declaration whose complete call/root census is literal
    // has a dedicated canonical MIR VerifierOnly profile. It retains every
    // Checker owner, callsite, and span in its source receipt; it runs ahead
    // of ABI-specific routes only after mixed Map/Any owners are rejected and
    // ScalarFfi keeps its own execution-capable MIR receipt.
    if !called_contract_names.is_empty() {
        if let Some(results) = ffi_compat::verify_literal_contracts(program, &source_hash) {
            return results;
        }
    }

    if called_contract_names.is_empty() {
        // A real but obligation-free extern still has to cross the checked
        // source/ABI and route boundary. Keep the no-obligation result only
        // for a pure compatibility input with no canonical candidate; every
        // incomplete or mixed candidate must remain a hard route error.
        let route = match crate::core::mir::materialize_canonical_mir_route(program, None) {
            Ok(route) => route,
            Err(error) => {
                if let crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility {
                    admission: compatibility_admission,
                    ..
                } = &error
                {
                    if !compatibility_admission.has_candidate() {
                        return Ok(Vec::new());
                    }
                }
                return Err(match error.diagnostic_code() {
                    Some(code) => format!("{code}: {error}"),
                    None => format!(
                        "{}: called extern has no complete canonical ScalarFfi route: {error}",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    ),
                });
            }
        };
        if !profile.is_admitted(admission) || !profile.is_materialized(&route) {
            return Err(format!(
                "{}: called extern did not receive a complete canonical ScalarFfi admission and materialization",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        return Ok(Vec::new());
    }

    let scalar_route = if profile.is_admitted(admission) && !actor_compatibility {
        // A closed ScalarFfi execution admission keeps its existing route
        // selector. The verifier-only owner-slice exception below is limited
        // to one exact construction boundary and never changes native or
        // bytecode admission.
        match crate::core::mir::materialize_canonical_mir_route(program, None) {
            Ok(route) => {
                if !profile.is_materialized(&route) {
                    return Err(format!(
                        "{}: complete ScalarFfi admission did not materialize a canonical verifier graph",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    ));
                }
                Some(route.program)
            }
            Err(error)
                if scalar_ffi_singleton_for_slice_is_the_only_construction_boundary(
                    program,
                    &called_contract_names,
                    &error,
                ) =>
            {
                // This narrow verifier adapter is explicitly non-executable.
                // The standard ScalarFfi materializer, default dispatch, and
                // both execution backends keep their existing hard failure.
                None
            }
            Err(error) => {
                return Err(match error.diagnostic_code() {
                    Some(code) => format!("{code}: {error}"),
                    None => format!(
                        "{}: complete ScalarFfi admission entered compatibility materialization: {error}",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    ),
                });
            }
        }
    } else {
        None
    };

    let z3_available = z3_available_override.unwrap_or_else(is_z3_available);
    if !z3_available {
        return Ok(ffi_no_z3_contract_results(program, scalar_route.is_some()));
    }

    if actor_compatibility {
        return verify_actor_compatibility_ffi_callers(
            program,
            &called_contract_names,
            &unmodeled_ensures_call_sites,
            source_hash,
        );
    }

    if scalar_route.is_none() {
        // The verifier-only compatibility route is owner scoped. A sibling
        // with an unsupported MIR shape must leave its own exact NITS result,
        // not erase a supported caller's FFI proof from the batch.
        return verify_isolated_ffi_callers(
            program,
            &called_contract_names,
            &unmodeled_ensures_call_sites,
            source_hash,
            true,
        );
    }

    let canonical = match scalar_route {
        Some(canonical) => canonical,
        None => match crate::core::mir::reference::MirProgram::from_checked_program(program) {
            Ok(canonical) => canonical,
            Err(error) => {
                return Ok(ffi_mir_materialization_boundary_results(
                    program,
                    &called_contract_names,
                    &format!("MIR proof graph could not be materialized: {error:?}"),
                ));
            }
        },
    };
    ffi_compat::verify_scalar_route(program, &canonical, source_hash)
}

/// Permit the verifier-only FFI owner slice after a complete ScalarFfi route
/// failed construction only when its typed failure set consists exactly of
/// the ordinary Phase 0 rejections for the corresponding source-level root
/// singleton-For owners, and every such owner independently materializes via
/// the Checker-backed verifier-only constructor. This does not retry generic
/// MIR failures, receipt failures, or any execution route.
fn scalar_ffi_singleton_for_slice_is_the_only_construction_boundary(
    program: &crate::core::CheckedProgram,
    called_contract_names: &std::collections::BTreeSet<String>,
    route_error: &crate::core::mir::CanonicalMirRouteMaterializationError,
) -> bool {
    if !matches!(
        route_error,
        crate::core::mir::CanonicalMirRouteMaterializationError::Complete {
            profile: crate::core::mir::CanonicalMirRouteProfile::ScalarFfi,
            stage: crate::core::mir::CanonicalMirRouteFailureStage::Construction,
            ..
        }
    ) {
        return false;
    }

    let owners = program
        .call_sites_sorted()
        .into_iter()
        .filter(|site| {
            site.kind == crate::core::ResolvedCallKind::Extern
                && called_contract_names.contains(&site.callee)
                && !ffi_site_is_declaration_root_call(program, site)
        })
        .map(|site| crate::core::NodeId(site.owner.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    if owners.is_empty() {
        return false;
    }

    let mut expected_for_nodes = std::collections::BTreeSet::new();
    for owner in &owners {
        let Some(callable) = program.callable(owner) else {
            return false;
        };
        let root_for_nodes = callable
            .body
            .root
            .statements
            .iter()
            .filter(|statement| {
                matches!(&statement.kind, crate::core::ResolvedStmtKind::For { .. })
            })
            .map(|statement| statement.node_id.clone())
            .collect::<Vec<_>>();
        let [for_node] = root_for_nodes.as_slice() else {
            return false;
        };
        if !expected_for_nodes.insert(for_node.clone()) {
            return false;
        }
    }
    if !ffi_compat::all_top_level_callers_project(program, &owners, called_contract_names) {
        return false;
    }

    // Mirror the ScalarFfi route's known prelude exclusion and retain the
    // typed build diagnostics instead of deciding from a rendered error
    // string. Any additional unsupported shape keeps the hard route failure.
    let excluded_sources = program
        .source_registry()
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<std::collections::HashSet<_>>();
    let ordinary_build = if excluded_sources.is_empty() {
        crate::core::mir::reference::MirProgram::from_checked_program(program)
    } else {
        crate::core::mir::reference::MirProgram::from_checked_program_excluding_sources(
            program,
            &excluded_sources,
        )
    };
    let Err(crate::core::mir::reference::MirProgramBuildError::Lowering(errors)) = ordinary_build
    else {
        return false;
    };
    !errors.is_empty()
        && errors.len() == expected_for_nodes.len()
        && errors.iter().all(|error| {
            error.message == "structured control flow is not lowered by MIR Phase 0"
                && expected_for_nodes.contains(&error.node_id)
        })
}

/// Return declaration-default FFI call nodes that are declarations only. A
/// callable default becomes an execution root only when a Checker-issued
/// TypedDefaultProofUse binds an omitted argument at an exact caller site;
/// trait defaults remain declaration-only until the trait-default expansion
/// has an equivalent activation receipt.
fn checked_ffi_inactive_declaration_default_calls(
    program: &crate::core::CheckedProgram,
) -> Result<std::collections::BTreeSet<crate::core::NodeId>, String> {
    use crate::core::ir::lower::{
        ResolvedDeclarationExpressionKind as RootKind,
        ResolvedDeclarationRootIdentity as RootIdentity,
    };

    let roots = program.checked_ffi_source_roots();
    let activated_defaults =
        crate::core::mir::checked_ffi_callable_default_activation_closure(program)
            .map_err(|message| format!("TOOL-RESOLUTION-001: {message}"))?;

    let mut inactive = std::collections::BTreeSet::new();
    let mut root_call_nodes = std::collections::BTreeSet::new();
    for root in roots.iter().filter(|root| {
        matches!(
            root.kind(),
            RootKind::CallableParameterDefault | RootKind::TraitParameterDefault
        )
    }) {
        if program.checked_ffi_source_root_body(root).is_err() {
            return Err(format!(
                "TOOL-RESOLUTION-001: FFI declaration-default root '{}' no longer resolves through its Checker handle",
                root.root_node().0
            ));
        }
        let declaration_only = match (root.kind(), root.identity()) {
            (
                RootKind::TraitParameterDefault,
                Some(RootIdentity::TraitParameterDefault { method, .. }),
            ) => method == root.owner(),
            (
                RootKind::CallableParameterDefault,
                Some(RootIdentity::CallableParameterDefault {
                    callable,
                    parameter,
                }),
            ) => {
                callable == root.owner()
                    && !activated_defaults.contains(&(callable.clone(), parameter.clone()))
            }
            _ => false,
        };
        for call in root.calls() {
            if !root_call_nodes.insert(call.call_node().clone()) {
                return Err(format!(
                    "TOOL-RESOLUTION-001: duplicate Checker FFI default call identity '{}'",
                    call.call_node().0
                ));
            }
            let context = program
                .checked_ffi_source_call_context(root, call)
                .map_err(|_| {
                    format!(
                        "TOOL-RESOLUTION-001: FFI default call '{}' no longer resolves through its Checker root/call handles",
                        call.call_node().0
                    )
                })?;
            let Some(site) = program.call_sites().get(call.call_node()) else {
                return Err(format!(
                    "TOOL-RESOLUTION-001: FFI default call '{}' is absent from the checked call-site directory",
                    call.call_node().0
                ));
            };
            let matching_sites = program
                .call_sites()
                .values()
                .filter(|candidate| candidate.node_id == *call.call_node())
                .count();
            let crate::core::ir::ResolvedExprKind::Call(resolved_call) = &context.call.kind else {
                return Err(format!(
                    "TOOL-RESOLUTION-001: FFI default call '{}' no longer resolves to a typed Call expression",
                    call.call_node().0
                ));
            };
            if matching_sites != 1
                || site.node_id != *call.call_node()
                || site.owner != root.owner().0
                || site.kind != crate::core::ResolvedCallKind::Extern
                || site.callee != context.declaration.symbol
                // Some declaration-root calls are collected before the
                // surface call-site table has a resolved-callee entry. The
                // Checker-issued root/call handles above already resolve to
                // the exact typed Extern expression and declaration. Keep an
                // optional call-site identity as a consistency check when it
                // exists, without making that optional cache a second owner.
                || site.resolved_callee.as_ref()
                    != Some(&crate::core::ir::ResolvedCallee::Extern(
                        call.external_declaration().clone(),
                    ))
                || site.origin != context.call.origin
                || !site.checked_ffi_source_arity_matches(
                    resolved_call.arguments.len(),
                    context.declaration,
                )
            {
                return Err(format!(
                    "TOOL-RESOLUTION-001: FFI default root/call '{}' disagrees with the exact checked call-site identity",
                    call.call_node().0
                ));
            }
            if declaration_only {
                inactive.insert(call.call_node().clone());
            }
        }
    }
    Ok(inactive)
}

fn ffi_site_is_declaration_root_call(
    program: &crate::core::CheckedProgram,
    site: &crate::core::ResolvedCallSite,
) -> bool {
    program.checked_ffi_source_roots().iter().any(|root| {
        root.calls()
            .iter()
            .any(|call| call.call_node() == &site.node_id)
    })
}

/// Consume Checker-issued FFI declaration-root handles through the canonical
/// MIR verifier census. Eager constants, Actor construction roots, and
/// activated callable defaults receive exact artifact-free boundaries; an
/// unactivated callable or trait default remains declaration-only and never
/// manufactures a call result.
fn verify_declaration_ffi_source_roots(
    program: &crate::core::CheckedProgram,
    inactive_default_call_nodes: &std::collections::BTreeSet<crate::core::NodeId>,
) -> Result<Option<Vec<VerificationResult>>, String> {
    use crate::core::mir::MirProofSourceFfiRootExecution as Execution;

    let checker_roots = program
        .checked_ffi_source_roots()
        .iter()
        .filter(|root| !root.calls().is_empty())
        .collect::<Vec<_>>();
    if checker_roots.is_empty() {
        return Ok(None);
    }

    let mut checker_calls =
        std::collections::BTreeMap::<crate::core::NodeId, crate::core::NodeId>::new();
    for root in &checker_roots {
        program.checked_ffi_source_root_body(root).map_err(|_| {
            format!(
                "TOOL-RESOLUTION-001: FFI declaration root '{}' no longer resolves through its Checker source-root handle",
                root.root_node().0
            )
        })?;
        for call in root.calls() {
            let context = program
                .checked_ffi_source_call_context(root, call)
                .map_err(|_| {
                    format!(
                        "TOOL-RESOLUTION-001: FFI declaration root '{}' call '{}' no longer resolves through its exact Checker root/call handles",
                        root.root_node().0,
                        call.call_node().0
                    )
                })?;
            if let Some(previous_root) =
                checker_calls.insert(call.call_node().clone(), root.root_node().clone())
            {
                return Err(format!(
                    "TOOL-RESOLUTION-001: FFI source call '{}' is duplicated or cross-bound between roots '{}' and '{}'",
                    call.call_node().0,
                    previous_root.0,
                    root.root_node().0
                ));
            }
            let site = program.call_sites().get(call.call_node()).ok_or_else(|| {
                format!(
                    "TOOL-RESOLUTION-001: FFI declaration root '{}' call '{}' has no exact Checker call-site entry",
                    root.root_node().0,
                    call.call_node().0
                )
            })?;
            let same_identity_count = program
                .call_sites()
                .values()
                .filter(|candidate| candidate.node_id == *call.call_node())
                .count();
            let crate::core::ir::ResolvedExprKind::Call(resolved_call) = &context.call.kind else {
                return Err(format!(
                    "TOOL-RESOLUTION-001: FFI declaration root '{}' source call '{}' no longer resolves to a typed Call",
                    root.root_node().0,
                    call.call_node().0
                ));
            };
            if same_identity_count != 1
                || site.owner != root.owner().0
                || site.node_id != *call.call_node()
                || site.kind != crate::core::ResolvedCallKind::Extern
                || site.callee != context.declaration.symbol
                || site.target_function.is_some()
                || context.declaration.declaration != *call.external_declaration()
                || resolved_call.callee
                    != crate::core::ir::ResolvedCallee::Extern(call.external_declaration().clone())
                || site.resolved_callee.as_ref()
                    != Some(&crate::core::ir::ResolvedCallee::Extern(
                        call.external_declaration().clone(),
                    ))
                || site.origin != context.call.origin
                || !site.checked_ffi_source_arity_matches(
                    resolved_call.arguments.len(),
                    context.declaration,
                )
            {
                return Err(format!(
                    "TOOL-RESOLUTION-001: FFI declaration root '{}' call '{}' disagrees with the exact Checker declaration/call/span identity",
                    root.root_node().0,
                    call.call_node().0
                ));
            }
            let declaration = program
                .ffi_declarations()
                .get(call.external_declaration())
                .ok_or_else(|| {
                    format!(
                        "TOOL-RESOLUTION-001: FFI declaration-root call '{}' lost declaration '{}'",
                        call.call_node().0,
                        call.external_declaration().0
                    )
                })?;
            declaration
                .validate(program)
                .map_err(|errors| format!("TOOL-RESOLUTION-001: {errors:?}"))?;
        }
    }

    let census = crate::core::mir::MirProofSourceCensus::checked_ffi_declaration_roots(program);
    census.validate_ffi_source_roots().map_err(|errors| {
        format!(
            "{}: source-only verifier census rejected the Checker FFI declaration roots: {errors:?}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })?;

    let census_roots = census.ffi_source_roots();
    let checker_root_nodes = checker_roots
        .iter()
        .map(|root| root.root_node().clone())
        .collect::<std::collections::BTreeSet<_>>();
    let census_root_nodes = census_roots
        .iter()
        .filter(|root| !root.calls().is_empty())
        .map(|root| root.root_node().clone())
        .collect::<std::collections::BTreeSet<_>>();
    if checker_root_nodes != census_root_nodes {
        return Err(format!(
            "{}: canonical MIR FFI source-root census omitted or added a Checker declaration root",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }

    let mut results = Vec::new();
    let mut census_calls =
        std::collections::BTreeMap::<crate::core::NodeId, crate::core::NodeId>::new();
    for root in census_roots.iter().filter(|root| !root.calls().is_empty()) {
        if !root.call_census_valid() {
            return Err(format!(
                "{}: FFI declaration root '{}' has an incomplete direct-call census",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                root.root_node().0
            ));
        }
        let root_origin = root.origin().ok_or_else(|| {
            format!(
                "{}: FFI declaration root '{}' has calls but no source origin",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                root.root_node().0
            )
        })?;
        let should_report = matches!(
            root.execution(),
            Execution::ConstantInitializer
                | Execution::ActorConstructionPending
                | Execution::CallableDefaultActivatedUnsupported
        );
        match root.execution() {
            Execution::TraitDefaultDeclarationOnly | Execution::CallableDefaultDeclarationOnly => {
                if root
                    .calls()
                    .iter()
                    .any(|call| !inactive_default_call_nodes.contains(call.call_node()))
                {
                    return Err(format!(
                        "{}: declaration-only FFI default root '{}' has an executable call without an activation receipt",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                        root.root_node().0
                    ));
                }
            }
            Execution::CallableDefaultActivatedUnsupported => {
                if root.activations().is_empty()
                    || root
                        .calls()
                        .iter()
                        .any(|call| inactive_default_call_nodes.contains(call.call_node()))
                {
                    return Err(format!(
                        "{}: activated FFI default root '{}' lacks a valid exact activation receipt",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                        root.root_node().0
                    ));
                }
            }
            Execution::ConstantInitializer | Execution::ActorConstructionPending => {
                if !root.activations().is_empty() {
                    return Err(format!(
                        "{}: non-default FFI root '{}' has an unexpected default activation",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                        root.root_node().0
                    ));
                }
            }
        }

        let activation_summary = root
            .activations()
            .iter()
            .map(|activation| {
                format!(
                    "caller='{}' invocation='{}' parameter='{}' argument='{}' index={} order={} scope={:?} guards={:?} disposition={:?} span={}",
                    activation.caller_owner().0,
                    activation.call_node().0,
                    activation.parameter().0.0,
                    activation.argument_node().0,
                    activation.parameter_index(),
                    activation.order(),
                    activation.scope(),
                    activation.guards(),
                    activation.disposition(),
                    activation.origin().user_span(),
                )
            })
            .collect::<Vec<_>>()
            .join("; ");

        for call in root.calls() {
            if !checker_calls
                .get(call.call_node())
                .is_some_and(|checker_root| checker_root == root.root_node())
                || census_calls
                    .insert(call.call_node().clone(), root.root_node().clone())
                    .is_some()
            {
                return Err(format!(
                    "{}: FFI declaration-root call '{}' is omitted, duplicated, or cross-bound in the canonical MIR census",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    call.call_node().0
                ));
            }
            if !should_report {
                continue;
            }

            let declaration = program
                .ffi_declarations()
                .get(call.declaration())
                .ok_or_else(|| {
                    format!(
                        "TOOL-RESOLUTION-001: FFI declaration-root call '{}' lost declaration '{}'",
                        call.call_node().0,
                        call.declaration().0
                    )
                })?;
            if !declaration.has_declared_contract() {
                continue;
            }
            let site = program
                .call_sites()
                .get(call.call_node())
                .expect("Checker root/call/site identity was validated before MIR materialization");
            let caller = checked_caller_name(program, &crate::core::NodeId(site.owner.clone()));
            let boundary = match root.execution() {
                Execution::ConstantInitializer => {
                    "constant initializer root evaluation has no executable MIR proof owner"
                }
                Execution::ActorConstructionPending => {
                    "Actor construction/reachability has no executable MIR proof owner"
                }
                Execution::CallableDefaultActivatedUnsupported => {
                    "activated default expansion has no executable MIR proof owner"
                }
                Execution::TraitDefaultDeclarationOnly
                | Execution::CallableDefaultDeclarationOnly => {
                    unreachable!("declaration-only defaults do not produce call results")
                }
            };
            for (declared, phase) in [
                (declaration.requires.is_declared(), "Requires"),
                (declaration.ensures.is_declared(), "Ensures"),
            ] {
                if !declared {
                    continue;
                }
                let message = format!(
                    "{}: checker-owned FFI declaration '{}' ({}) {} at {:?} root '{}' (owner '{}', root span {}) call '{}' (call span {}) has no proof route: {}; canonical MIR verifier census consumed this exact root/call identity; activations=[{}]",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    declaration.symbol,
                    declaration.declaration.0,
                    phase,
                    root.kind(),
                    root.root_node().0,
                    root.owner().0,
                    root_origin.user_span(),
                    call.call_node().0,
                    call.call_origin().user_span(),
                    boundary,
                    activation_summary,
                );
                let func_name = if phase == "Requires" {
                    format!("{caller} calls {}", declaration.symbol)
                } else {
                    format!("{caller} -> extern {} Ensures", declaration.symbol)
                };
                results.push(VerificationResult {
                    func_name,
                    status: VerifStatus::NotInTrustedSubset,
                    diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                        message.clone(),
                        call.call_origin().user_span(),
                    )),
                    message,
                    duration_us: 0,
                    constraint_count: 0,
                    artifact: None,
                    trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
                });
            }
        }
    }

    let expected_calls = checker_calls
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if census_calls
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        != expected_calls
        || census_calls != checker_calls
    {
        return Err(format!(
            "{}: canonical MIR source-root census omitted or remapped a Checker FFI call",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    Ok(Some(results))
}

/// Actor compatibility keeps the scalar Actor NoContracts execution gate
/// intact.  FFI Requires are proved through independent, verifier-only MIR
/// slices for isolated top-level callers; an unsupported caller is reported
/// at each exact FFI call site instead of being lost when Actor execution MIR
/// omits ordinary functions. Foreign Ensures always remain artifact-free NITS
/// on this route, even when their checked roots are otherwise encodable.
fn verify_actor_compatibility_ffi_callers(
    program: &crate::core::CheckedProgram,
    called_contract_names: &std::collections::BTreeSet<String>,
    unmodeled_ensures_call_sites: &[crate::core::ResolvedCallSite],
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_isolated_ffi_callers(
        program,
        called_contract_names,
        unmodeled_ensures_call_sites,
        source_hash,
        false,
    )
}

/// Allow the exact direct `len(StringParameter)` zero comparison that already
/// belongs to the isolated caller slice, plus the one closed guarded
/// `len(s) > 0 && char_at(s, 0) == scalar` caller Requires shape and one
/// aggregate-field shape. Both extensions bind to checker-owned direct
/// parameters and exact ResolvedCallSites; all other projections, contexts,
/// and calls remain outside this owner.
fn isolated_ffi_direct_string_length_requires_site(
    program: &crate::core::CheckedProgram,
    callable: &crate::core::ResolvedCallable,
    site: &crate::core::ResolvedCallSite,
    type_catalog: Option<&crate::core::mir::types::MirTypeCatalog>,
) -> bool {
    use crate::core::ir::{
        ContractKind, ResolvedBinaryOp, ResolvedCallee, ResolvedExpr, ResolvedExprKind,
        ResolvedLiteral, ResolvedProjection,
    };

    if site.kind != crate::core::ResolvedCallKind::Builtin || site.callee != "len" || site.argc != 1
    {
        return false;
    }

    fn len_call_in_zero_comparison<'a>(
        expression: &'a ResolvedExpr,
        bool_type: &crate::core::ResolvedTypeId,
        i32_type: &crate::core::ResolvedTypeId,
    ) -> Option<&'a ResolvedExpr> {
        let ResolvedExprKind::Binary { op, left, right } = &expression.kind else {
            return None;
        };
        if &expression.ty != bool_type {
            return None;
        }
        let is_zero = |candidate: &ResolvedExpr| {
            &candidate.ty == i32_type
                && matches!(
                    &candidate.kind,
                    ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
                )
        };
        let call_expression = match op {
            ResolvedBinaryOp::Greater if is_zero(right) => left.as_ref(),
            ResolvedBinaryOp::Less if is_zero(left) => right.as_ref(),
            ResolvedBinaryOp::Equal if is_zero(right) => left.as_ref(),
            ResolvedBinaryOp::Equal if is_zero(left) => right.as_ref(),
            _ => return None,
        };
        (&call_expression.ty == i32_type).then_some(call_expression)
    }

    fn exact_len_call_at_site<'a>(
        expression: &'a ResolvedExpr,
        site: &crate::core::ResolvedCallSite,
        bool_type: &crate::core::ResolvedTypeId,
        i32_type: &crate::core::ResolvedTypeId,
        string_type: &crate::core::ResolvedTypeId,
    ) -> Option<(&'a crate::core::ir::ResolvedCall, &'a ResolvedExpr)> {
        let call_expression = len_call_in_zero_comparison(expression, bool_type, i32_type)?;
        if call_expression.node_id != site.node_id {
            return None;
        }
        let ResolvedExprKind::Call(call) = &call_expression.kind else {
            return None;
        };
        if !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "len")
            || &call.result != i32_type
            || !call.type_arguments.is_empty()
            || call.permission.is_some()
            || !call.effects.is_empty()
            || !call.session.is_empty()
        {
            return None;
        }
        let [argument] = call.arguments.as_slice() else {
            return None;
        };
        if argument.parameter.0 != crate::core::NodeId("builtin:len/parameter:0".into())
            || argument.conversion.kind != crate::core::ir::CheckedConversionKind::Identity
            || &argument.conversion.from != string_type
            || &argument.conversion.to != string_type
            || &argument.value.ty != string_type
        {
            return None;
        }
        Some((call, &argument.value))
    }

    fn direct_string_parameter(
        callable: &crate::core::ResolvedCallable,
        argument: &ResolvedExpr,
        string_type: &crate::core::ResolvedTypeId,
    ) -> bool {
        direct_string_parameter_base(callable, argument, string_type).is_some()
    }

    fn direct_string_parameter_base<'a>(
        callable: &crate::core::ResolvedCallable,
        argument: &'a ResolvedExpr,
        string_type: &crate::core::ResolvedTypeId,
    ) -> Option<&'a crate::core::ResolvedLocalId> {
        let ResolvedExprKind::Load(place) = &argument.kind else {
            return None;
        };
        if !place.projections.is_empty()
            || !callable
                .signature
                .parameters
                .iter()
                .zip(&callable.body.parameters)
                .any(|(parameter, local)| local == &place.base && &parameter.ty == string_type)
        {
            return None;
        }
        Some(&place.base)
    }

    fn exact_char_at0_equality<'a>(
        expression: &'a ResolvedExpr,
        callable: &crate::core::ResolvedCallable,
        string_type: &crate::core::ResolvedTypeId,
        bool_type: &crate::core::ResolvedTypeId,
    ) -> Option<&'a crate::core::ResolvedLocalId> {
        let ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::Equal,
            left,
            right,
        } = &expression.kind
        else {
            return None;
        };
        if &expression.ty != bool_type
            || !expression.effects.is_empty()
            || !expression.backend_requirements.is_empty()
        {
            return None;
        }
        let (char_expression, literal_expression) = match (&left.kind, &right.kind) {
            (ResolvedExprKind::Call(_), ResolvedExprKind::Literal(ResolvedLiteral::String(_))) => {
                (left.as_ref(), right.as_ref())
            }
            (ResolvedExprKind::Literal(ResolvedLiteral::String(_)), ResolvedExprKind::Call(_)) => {
                (right.as_ref(), left.as_ref())
            }
            _ => return None,
        };
        if &literal_expression.ty != string_type
            || !literal_expression.effects.is_empty()
            || !literal_expression.backend_requirements.is_empty()
        {
            return None;
        }
        let ResolvedExprKind::Literal(ResolvedLiteral::String(literal)) = &literal_expression.kind
        else {
            return None;
        };
        let mut literal_scalars = literal.chars();
        let Some(scalar) = literal_scalars.next() else {
            return None;
        };
        if scalar == '\0' || scalar as u32 > 0x2ffff || literal_scalars.next().is_some() {
            return None;
        }

        let ResolvedExprKind::Call(call) = &char_expression.kind else {
            return None;
        };
        let [string_argument, index_argument] = call.arguments.as_slice() else {
            return None;
        };
        let integer_index_type = [
            crate::core::PrimitiveType::I8,
            crate::core::PrimitiveType::I16,
            crate::core::PrimitiveType::I32,
            crate::core::PrimitiveType::I64,
            crate::core::PrimitiveType::I128,
            crate::core::PrimitiveType::U8,
            crate::core::PrimitiveType::U16,
            crate::core::PrimitiveType::U32,
            crate::core::PrimitiveType::U64,
            crate::core::PrimitiveType::U128,
            crate::core::PrimitiveType::Isize,
            crate::core::PrimitiveType::Usize,
        ]
        .into_iter()
        .any(|primitive| index_argument.value.ty == crate::core::ir::primitive_type_id(primitive));
        if !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "char_at")
            || string_argument.parameter.0
                != crate::core::NodeId("builtin:char_at/parameter:0".into())
            || index_argument.parameter.0
                != crate::core::NodeId("builtin:char_at/parameter:1".into())
            || call.type_arguments.len() != 0
            || call.permission.is_some()
            || !call.effects.is_empty()
            || !call.session.is_empty()
            || !char_expression.effects.is_empty()
            || !char_expression.backend_requirements.is_empty()
            || string_argument.conversion.kind != crate::core::ir::CheckedConversionKind::Identity
            || string_argument.conversion.from != string_argument.value.ty
            || string_argument.conversion.to != string_argument.value.ty
            || string_argument.value.ty != *string_type
            || !string_argument.value.effects.is_empty()
            || !string_argument.value.backend_requirements.is_empty()
            || index_argument.conversion.kind != crate::core::ir::CheckedConversionKind::Identity
            || index_argument.conversion.from != index_argument.value.ty
            || index_argument.conversion.to != index_argument.value.ty
            || !integer_index_type
            || !matches!(
                &index_argument.value.kind,
                ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
            )
            || !index_argument.value.effects.is_empty()
            || !index_argument.value.backend_requirements.is_empty()
            || call.result != *string_type
            || char_expression.ty != *string_type
        {
            return None;
        }
        direct_string_parameter_base(callable, &string_argument.value, string_type)
    }

    fn exact_guarded_char_at0_len_site(
        condition: &ResolvedExpr,
        callable: &crate::core::ResolvedCallable,
        site: &crate::core::ResolvedCallSite,
        bool_type: &crate::core::ResolvedTypeId,
        i32_type: &crate::core::ResolvedTypeId,
        string_type: &crate::core::ResolvedTypeId,
    ) -> bool {
        let ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::LogicalAnd,
            left,
            right,
        } = &condition.kind
        else {
            return false;
        };
        if &condition.ty != bool_type
            || !condition.effects.is_empty()
            || !condition.backend_requirements.is_empty()
        {
            return false;
        }
        let ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::Greater,
            left: length,
            right: zero,
        } = &left.kind
        else {
            return false;
        };
        if &left.ty != bool_type
            || !left.effects.is_empty()
            || !left.backend_requirements.is_empty()
            || &zero.ty != i32_type
            || !matches!(
                &zero.kind,
                ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
            )
            || !zero.effects.is_empty()
            || !zero.backend_requirements.is_empty()
            || &length.ty != i32_type
            || !length.effects.is_empty()
            || !length.backend_requirements.is_empty()
        {
            return false;
        }
        let Some((_, length_argument)) =
            exact_len_call_at_site(left, site, bool_type, i32_type, string_type)
        else {
            return false;
        };
        if !length_argument.effects.is_empty() || !length_argument.backend_requirements.is_empty() {
            return false;
        }
        let Some(length_parameter) =
            direct_string_parameter_base(callable, length_argument, string_type)
        else {
            return false;
        };
        exact_char_at0_equality(right, callable, string_type, bool_type)
            .is_some_and(|character_parameter| character_parameter == length_parameter)
    }

    fn checked_string_record_field(
        program: &crate::core::CheckedProgram,
        catalog: &crate::core::mir::types::MirTypeCatalog,
        record_type: &crate::core::ResolvedTypeId,
        field_id: &crate::core::NodeId,
        field_name: &str,
        field_type: &crate::core::ResolvedTypeId,
        string_type: &crate::core::ResolvedTypeId,
    ) -> bool {
        use crate::core::ir::{NativeTypeDeclarationKind, ResolvedType};
        use crate::core::mir::types::MirLayout;

        if field_type != string_type
            || !catalog.is_canonical_ffi_verifier_string_record(record_type)
        {
            return false;
        }
        let Some(ResolvedType::Nominal {
            item, arguments, ..
        }) = program.resolved_types().get(record_type)
        else {
            return false;
        };
        if !arguments.is_empty() {
            return false;
        }
        let owner = crate::core::NodeId(item.as_str().to_string());
        let Some(type_def) = program.type_defs().get(&owner) else {
            return false;
        };
        if type_def.node_id != owner
            || type_def.kind != crate::core::ResolvedTypeKind::Record
            || !type_def.generic_parameters.is_empty()
            || type_def.field_ids.get(field_name) != Some(field_id)
            || program.resolved_field_type(field_id) != Some(field_type)
        {
            return false;
        }
        let Some(declaration) = program.native_declarations().types.get(&type_def.node_id) else {
            return false;
        };
        if declaration.owner != type_def.node_id
            || !declaration.representation.repr_c
            || !declaration.generics.is_empty()
        {
            return false;
        }
        let NativeTypeDeclarationKind::Record {
            fields: declared_fields,
        } = &declaration.kind
        else {
            return false;
        };
        if !declared_fields.iter().any(|field| {
            field.field == *field_id && field.name == field_name && field.ty == *field_type
        }) {
            return false;
        }
        let Some(descriptor) = catalog.get(record_type) else {
            return false;
        };
        let MirLayout::Record { fields, .. } = &descriptor.layout else {
            return false;
        };
        fields.iter().any(|field| {
            field.id == *field_id && field.name == field_name && field.ty == *field_type
        })
    }

    fn aggregate_string_parameter_field(
        program: &crate::core::CheckedProgram,
        catalog: &crate::core::mir::types::MirTypeCatalog,
        callable: &crate::core::ResolvedCallable,
        argument: &ResolvedExpr,
        string_type: &crate::core::ResolvedTypeId,
    ) -> bool {
        let ResolvedExprKind::Load(place) = &argument.kind else {
            return false;
        };
        let [ResolvedProjection::Field {
            field,
            name,
            ty: field_type,
        }] = place.projections.as_slice()
        else {
            return false;
        };
        if field_type != string_type || &argument.ty != string_type {
            return false;
        }
        let Some(record_type) = callable
            .signature
            .parameters
            .iter()
            .zip(&callable.body.parameters)
            .find(|(_, local)| *local == &place.base)
            .map(|(parameter, _)| &parameter.ty)
        else {
            return false;
        };
        checked_string_record_field(
            program,
            catalog,
            record_type,
            field,
            name,
            field_type,
            string_type,
        )
    }

    fn aggregate_call_in_and_tree(
        program: &crate::core::CheckedProgram,
        catalog: &crate::core::mir::types::MirTypeCatalog,
        callable: &crate::core::ResolvedCallable,
        condition: &ResolvedExpr,
        site: &crate::core::ResolvedCallSite,
        bool_type: &crate::core::ResolvedTypeId,
        i32_type: &crate::core::ResolvedTypeId,
        string_type: &crate::core::ResolvedTypeId,
    ) -> bool {
        let ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::LogicalAnd,
            left,
            right,
        } = &condition.kind
        else {
            return false;
        };
        [&**left, &**right].into_iter().any(|child| {
            exact_len_call_at_site(child, site, bool_type, i32_type, string_type).is_some_and(
                |(_, argument)| {
                    aggregate_string_parameter_field(
                        program,
                        catalog,
                        callable,
                        argument,
                        string_type,
                    )
                },
            ) || aggregate_call_in_and_tree(
                program,
                catalog,
                callable,
                child,
                site,
                bool_type,
                i32_type,
                string_type,
            )
        })
    }

    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let bool_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
    let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    callable.contracts.iter().any(|contract| {
        if contract.kind != ContractKind::Requires {
            return false;
        }
        if exact_len_call_at_site(
            &contract.condition,
            site,
            &bool_type,
            &i32_type,
            &string_type,
        )
        .is_some_and(|(_, argument)| direct_string_parameter(callable, argument, &string_type))
        {
            return true;
        }
        if exact_guarded_char_at0_len_site(
            &contract.condition,
            callable,
            site,
            &bool_type,
            &i32_type,
            &string_type,
        ) {
            return true;
        }
        type_catalog.is_some_and(|catalog| {
            aggregate_call_in_and_tree(
                program,
                catalog,
                callable,
                &contract.condition,
                site,
                &bool_type,
                &i32_type,
                &string_type,
            )
        })
    })
}

/// Recognize the one Flow transition body admitted by the verifier-only FFI
/// compatibility route: a single direct extern call with one fixed String
/// literal, followed by a target-state record made only from literals. The
/// FFI precondition must be the Checker-owned Ready predicate `s != ""`.
/// This is deliberately a typed-body check; the resulting MIR still binds the
/// transition owner, call node, declaration, argument, and predicate through
/// the independent Checker source census.
fn isolated_ffi_flow_literal_requires_site(
    program: &crate::core::CheckedProgram,
    owner: &crate::core::NodeId,
    site: &crate::core::ResolvedCallSite,
) -> Option<crate::core::NodeId> {
    use crate::core::ir::{
        CheckedConversionKind, ResolvedBinaryOp, ResolvedCallee, ResolvedExprKind,
        ResolvedFfiContractRoot, ResolvedFfiParameterMode, ResolvedLiteral, ResolvedPatternKind,
        ResolvedStmtKind,
    };

    if site.owner != owner.0
        || site.kind != crate::core::ResolvedCallKind::Extern
        || site.argc != 1
        || !site.arity_matches()
        || !owner.0.starts_with("transition:")
    {
        return None;
    }
    let owner_sites = program
        .call_sites_sorted()
        .into_iter()
        .filter(|candidate| candidate.owner == owner.0)
        .collect::<Vec<_>>();
    let [only_site] = owner_sites.as_slice() else {
        return None;
    };
    if only_site.node_id != site.node_id
        || only_site.kind != site.kind
        || only_site.callee != site.callee
    {
        return None;
    }

    let mut matching_transitions = program
        .transitions()
        .values()
        .filter(|transition| &transition.node_id == owner);
    let transition = matching_transitions.next()?;
    if matching_transitions.next().is_some()
        || !matches!(&transition.origin, crate::core::Origin::User(_))
        || transition.fails.is_some()
        || transition.is_fallback
        || transition.is_ffi_pinned
        || transition.silent_transition
        || !transition.params.is_empty()
        || transition.targets.len() != 1
    {
        return None;
    }
    let flow = program.flows().get(&transition.id.flow)?;
    let source_state = flow.states.get(&transition.id.source.name)?;
    let [target_id] = transition.targets.as_slice() else {
        return None;
    };
    let target_state = flow.states.get(&target_id.name)?;
    let expected_owner = crate::core::NodeId(format!(
        "transition:{}::{}::{}",
        transition.id.flow.0, transition.id.event, transition.id.source.name
    ));
    if !matches!(&flow.origin, crate::core::Origin::User(_))
        || !flow.generic_parameters.is_empty()
        || !flow.transitions.contains(&transition.id)
        || source_state.id != transition.id.source
        || &target_state.id != target_id
        || transition.id.flow != flow.id
        || expected_owner != *owner
        || program
            .transitions()
            .values()
            .filter(|candidate| {
                candidate.id.flow == transition.id.flow
                    && matches!(&candidate.origin, crate::core::Origin::User(_))
                    && program.resolved_body(&candidate.node_id).is_some()
            })
            .count()
            != 1
    {
        return None;
    }

    let callable = program.callable(owner)?;
    let body = program.resolved_body(owner)?;
    if callable.owner != *owner
        || callable.signature.owner != *owner
        || !callable.signature.generic_parameters.is_empty()
        || callable.signature.parameters.len() != 1
        || !callable.signature.effects.is_empty()
        || !callable.contracts.is_empty()
        || !body.captures.is_empty()
        || !body.default_values.is_empty()
        || !body.place_inputs.is_empty()
        || body.owner != *owner
        || body.root.result.is_some()
        || body.resolved_calls.len() != 1
        || body.resolved_calls.get(&site.node_id) != site.resolved_callee.as_ref()
    {
        return None;
    }
    let [bind, returned] = body.root.statements.as_slice() else {
        return None;
    };
    let (binding, initializer) = match &bind.kind {
        ResolvedStmtKind::Bind {
            pattern,
            initializer: Some(initializer),
        } => (pattern, initializer),
        _ => return None,
    };
    let ResolvedPatternKind::Binding {
        local: result_local,
        by_reference: None,
    } = &binding.kind
    else {
        return None;
    };
    let local = body.locals.get(result_local)?;
    if local.mutable || local.ty != initializer.ty || binding.ty != initializer.ty {
        return None;
    }
    let ResolvedExprKind::Call(call) = &initializer.kind else {
        return None;
    };
    let ResolvedCallee::Extern(declaration_id) = &call.callee else {
        return None;
    };
    if initializer.node_id != site.node_id
        || call.type_arguments.len() != 0
        || call.effects.len() != 0
        || call.session.len() != 0
        || call.arguments.len() != 1
        || site.resolved_callee != Some(ResolvedCallee::Extern(declaration_id.clone()))
    {
        return None;
    }
    let argument = &call.arguments[0];
    let ResolvedExprKind::Literal(ResolvedLiteral::String(_)) = &argument.value.kind else {
        return None;
    };
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    if argument.value.ty != string_type
        || argument.conversion.kind != CheckedConversionKind::Identity
        || argument.conversion.from != string_type
        || argument.conversion.to != string_type
    {
        return None;
    }

    let declaration = program.ffi_declarations().get(declaration_id)?;
    if declaration.symbol != site.callee
        || declaration.abi != "C"
        || declaration.invocation.variadic
        || declaration.invocation.unsafe_block
        || declaration.invocation.block_returns_errno
        || declaration.invocation.declaration_returns_errno
        || declaration.fixed_parameter_ids.len() != 1
        || declaration.fixed_parameter_modes.len() != 1
        || declaration.fixed_parameter_modes.first() != Some(&ResolvedFfiParameterMode::Value)
        || !matches!(&declaration.ensures, ResolvedFfiContractRoot::Absent)
        || declaration.validate(program).is_err()
    {
        return None;
    }
    let crate::core::ir::ResolvedFfiContractRoot::Ready {
        node_id: requires_node,
    } = &declaration.requires
    else {
        return None;
    };
    let requires_root = program.declaration_expression_bodies().get(requires_node)?;
    if requires_root.node_id != *requires_node
        || requires_root.owner != declaration.declaration
        || requires_root.kind
            != crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternRequires
    {
        return None;
    }
    let requires_body = requires_root.body.as_ref().ok()?;
    if requires_body.owner != declaration.declaration
        || requires_body.parameters.len() != 1
        || requires_body.locals.len() != 1
        || !requires_body.captures.is_empty()
        || !requires_body.default_values.is_empty()
        || !requires_body.place_inputs.is_empty()
        || !requires_body.resolved_calls.is_empty()
        || !requires_body.root.statements.is_empty()
    {
        return None;
    }
    let parameter_local_id = requires_body.parameters.first()?;
    let parameter_local = requires_body.locals.get(parameter_local_id)?;
    let requires = requires_body.root.result.as_deref()?;
    let ResolvedExprKind::Binary {
        op: ResolvedBinaryOp::NotEqual,
        left,
        right,
    } = &requires.kind
    else {
        return None;
    };
    let ResolvedExprKind::Load(parameter_place) = &left.kind else {
        return None;
    };
    let ResolvedExprKind::Literal(ResolvedLiteral::String(empty)) = &right.kind else {
        return None;
    };
    let bool_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
    if requires.node_id != *requires_node
        || requires.ty != bool_type
        || parameter_local.display_name != "s"
        || parameter_local.ty != string_type
        || parameter_place.base != *parameter_local_id
        || !parameter_place.projections.is_empty()
        || left.ty != string_type
        || right.ty != string_type
        || !empty.is_empty()
    {
        return None;
    }

    let ResolvedStmtKind::Return {
        value: Some(return_value),
        conversion: Some(conversion),
    } = &returned.kind
    else {
        return None;
    };
    let ResolvedExprKind::Record {
        nominal,
        fields,
        rest: None,
    } = &return_value.kind
    else {
        return None;
    };
    let expected_nominal = crate::core::NominalTypeId::new(format!(
        "state:{}::{}",
        transition.id.flow.0, target_id.name
    ))
    .ok()?;
    let expected_fields = target_state
        .field_ids
        .values()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut actual_fields = std::collections::BTreeSet::new();
    for field in fields {
        if !actual_fields.insert(field.field.clone())
            || !matches!(&field.value.kind, ResolvedExprKind::Literal(_))
            || field.conversion.kind != CheckedConversionKind::Identity
        {
            return None;
        }
    }
    if nominal != &expected_nominal
        || !matches!(
            program.resolved_types().get(&return_value.ty),
            Some(crate::core::ir::ResolvedType::Nominal { item, .. }) if item == &expected_nominal
        )
        || actual_fields != expected_fields
        || conversion.kind != CheckedConversionKind::Identity
    {
        return None;
    }
    Some(site.node_id.clone())
}

/// Compatibility verification proves checker-owned FFI contract roots through
/// independent, verifier-only MIR slices for isolated top-level callers and
/// one exact user-implemented Flow transition shape. When enabled, Ready
/// Ensures are selected only after the exact checked source census and
/// call-site identity join. Actor compatibility disables that selection and
/// keeps every foreign Ensures as an artifact-free NITS; unsupported callers
/// stay visible without opening an execution route.
fn verify_isolated_ffi_callers(
    program: &crate::core::CheckedProgram,
    called_contract_names: &std::collections::BTreeSet<String>,
    unmodeled_ensures_call_sites: &[crate::core::ResolvedCallSite],
    source_hash: String,
    allow_ready_ensures: bool,
) -> Result<Vec<VerificationResult>, String> {
    use std::collections::BTreeMap;

    let actor_method_owners = program
        .actor_descriptors()
        .values()
        .flat_map(|descriptor| descriptor.methods.iter().map(|method| method.owner.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    let mut sites_by_owner =
        BTreeMap::<crate::core::NodeId, Vec<crate::core::ResolvedCallSite>>::new();
    for site in program.call_sites_sorted().into_iter().filter(|site| {
        site.kind == crate::core::ResolvedCallKind::Extern
            && called_contract_names.contains(&site.callee)
            && !ffi_site_is_declaration_root_call(program, site)
    }) {
        sites_by_owner
            .entry(crate::core::NodeId(site.owner.clone()))
            .or_default()
            .push(site.clone());
    }

    let checker_type_catalog =
        crate::core::mir::types::MirTypeCatalog::from_checked_program(program).ok();
    let mut results = Vec::new();
    let mut modeled_ensures_sites = Vec::<FfiCallSiteIdentity>::new();
    for (owner, sites) in sites_by_owner {
        let callable = program.callable(&owner);
        let helper_binding = callable
            .and_then(|callable| isolated_ffi_requires_helper_binding(program, &owner, callable));
        let flow_transition_call_node = if sites.len() == 1 {
            isolated_ffi_flow_literal_requires_site(program, &owner, &sites[0])
        } else {
            None
        };
        let isolated_top_level_owner = owner.0.starts_with("function:")
            && !owner.0["function:".len()..].contains("::")
            && !actor_method_owners.contains(&owner)
            && callable.is_some_and(|callable| {
                callable.signature.generic_parameters.is_empty()
                    && !program.native_declarations().exports.contains_key(&owner)
            })
            && program
                .call_sites_sorted()
                .into_iter()
                .filter(|site| site.owner == owner.0)
                // `char_at` is the only non-extern callable surface admitted
                // by the checked String Requires shape. It is pure and has a
                // dedicated MIR StringCharAt0 representation; user functions
                // and methods still keep the owner outside this isolated slice.
                .all(|site| {
                    site.kind == crate::core::ResolvedCallKind::Extern
                        || (site.kind == crate::core::ResolvedCallKind::Builtin
                            && site.callee == "char_at"
                            && site.argc == 2)
                        || callable.is_some_and(|callable| {
                            isolated_ffi_direct_string_length_requires_site(
                                program,
                                callable,
                                site,
                                checker_type_catalog.as_ref(),
                            )
                        })
                        || helper_binding.as_ref().is_some_and(|(helper, call_node)| {
                            site.kind == crate::core::ResolvedCallKind::Function
                                && site.node_id == *call_node
                                && site.resolved_callee
                                    == Some(crate::core::ir::ResolvedCallee::Function(
                                        helper.clone(),
                                    ))
                        })
                });
        let isolated_owner = isolated_top_level_owner || flow_transition_call_node.is_some();
        if !isolated_owner {
            results.extend(ffi_mir_requires_materialization_boundary_results(
                program,
                &sites,
                &format!(
                    "caller owner '{}' is an Actor method or is outside the isolated FFI caller and Flow-transition slices",
                    owner.0
                ),
            ));
            continue;
        }

        let projection_shape = if let Some(call_node) = &flow_transition_call_node {
            ffi_compat::FfiCallerProjectionShape::FlowTransition { call: call_node }
        } else if let Some((helper, call_node)) = &helper_binding {
            ffi_compat::FfiCallerProjectionShape::TopLevelWithHelper {
                helper_owner: helper,
                helper_call: call_node,
            }
        } else {
            ffi_compat::FfiCallerProjectionShape::TopLevel
        };
        let projection = match ffi_compat::FfiVerifierOnlyProjection::from_checked_caller(
            program,
            &owner,
            &sites,
            projection_shape,
        ) {
            Ok(projection) => projection,
            Err(error) => {
                results.extend(ffi_mir_requires_materialization_boundary_results(
                    program,
                    &sites,
                    &format!(
                        "verifier-only FFI MIR caller slice could not be materialized: {error}"
                    ),
                ));
                continue;
            }
        };
        let canonical = projection.mir();

        let mut expected_contract_sites = Vec::new();
        for (site_index, site) in sites.iter().enumerate() {
            let Some(declaration) = program
                .ffi_declarations()
                .values()
                .find(|declaration| declaration.symbol == site.callee)
            else {
                return Err(format!(
                    "TOOL-RESOLUTION-001: checker-owned FFI declaration for '{}' disappeared during owner-site reconciliation",
                    site.callee
                ));
            };
            let identity = FfiCallSiteIdentity {
                owner: crate::core::NodeId(site.owner.clone()),
                call_node: site.node_id.clone(),
                symbol: site.callee.clone(),
            };
            if declaration.requires.is_declared() {
                expected_contract_sites.push((
                    FfiContractSiteIdentity {
                        call: identity.clone(),
                        phase: FfiContractPhase::Requires,
                    },
                    site_index,
                ));
            }
            if declaration.ensures.is_declared() {
                expected_contract_sites.push((
                    FfiContractSiteIdentity {
                        call: identity,
                        phase: FfiContractPhase::Ensures,
                    },
                    site_index,
                ));
            }
        }

        let mut selected_ensures = std::collections::BTreeSet::new();
        let mut selected_ensures_sites = Vec::<FfiCallSiteIdentity>::new();
        let mut mir_contract_sites = Vec::new();
        for (instruction, contract) in canonical.ffi_call_entries_in_source_order() {
            if contract.caller != owner {
                continue;
            }
            let identity = FfiCallSiteIdentity {
                owner: contract.caller.clone(),
                call_node: contract.call_node.clone(),
                symbol: contract.symbol.clone(),
            };
            let source_site = sites.iter().find(|site| {
                site.owner == identity.owner.0
                    && site.node_id == identity.call_node
                    && site.callee == identity.symbol
            });
            if contract.requires.is_some() || contract.requires_deferred {
                mir_contract_sites.push(FfiContractSiteIdentity {
                    call: identity.clone(),
                    phase: FfiContractPhase::Requires,
                });
            }
            if contract.ensures.is_some() || contract.ensures_deferred {
                mir_contract_sites.push(FfiContractSiteIdentity {
                    call: identity.clone(),
                    phase: FfiContractPhase::Ensures,
                });
            }

            let Some(source_site) = source_site else {
                continue;
            };
            let Some(declaration) = program
                .ffi_declarations()
                .values()
                .find(|declaration| declaration.symbol == source_site.callee)
            else {
                continue;
            };
            let ready_root = match &declaration.ensures {
                crate::core::ir::ResolvedFfiContractRoot::Ready { node_id } => Some(node_id),
                crate::core::ir::ResolvedFfiContractRoot::Absent
                | crate::core::ir::ResolvedFfiContractRoot::Deferred { .. } => None,
            };
            if allow_ready_ensures
                && ready_root.is_some_and(|node_id| {
                    contract.ensures_root.as_ref() == Some(node_id)
                        && contract.ensures.is_some()
                        && !contract.ensures_deferred
                        && !contract.ensures_types.is_empty()
                })
            {
                selected_ensures.insert(instruction.clone());
                selected_ensures_sites.push(identity);
            }
        }

        match projection.verify(program, source_hash.clone(), &selected_ensures) {
            Ok(mut owner_results) => {
                // Reconcile both contract phases by stable Checker call-site
                // identity. Display labels are assigned only after every
                // MIR root has an exact source identity; the per-call result
                // count then covers Requires plus only selected Ready Ensures.
                let expected_identities = expected_contract_sites
                    .iter()
                    .map(|(identity, _)| identity.clone())
                    .collect::<Vec<_>>();
                let site_order = order_ffi_contract_sites_by_identity(
                    &expected_identities,
                    &mir_contract_sites,
                )?;
                for missing_index in site_order.missing_checker_site_indexes {
                    let (identity, site_index) = &expected_contract_sites[missing_index];
                    if identity.phase == FfiContractPhase::Requires {
                        results.extend(ffi_mir_requires_materialization_boundary_results(
                            program,
                            std::slice::from_ref(&sites[*site_index]),
                            "checker-owned FFI Requires call site has no exact MIR receipt",
                        ));
                    }
                }
                let mut result_sites = Vec::<(FfiCallSiteIdentity, usize)>::new();
                for checker_index in &site_order.mir_ordered_checker_site_indexes {
                    let (identity, site_index) = &expected_contract_sites[*checker_index];
                    let emits_result = identity.phase == FfiContractPhase::Requires
                        || (identity.phase == FfiContractPhase::Ensures
                            && selected_ensures_sites.contains(&identity.call));
                    if emits_result
                        && !result_sites
                            .iter()
                            .any(|(existing, _)| existing == &identity.call)
                    {
                        result_sites.push((identity.call.clone(), *site_index));
                    }
                }
                let labels = result_sites
                    .iter()
                    .map(|(_, site_index)| {
                        let site = &sites[*site_index];
                        let owner = crate::core::NodeId(site.owner.clone());
                        (checked_caller_name(program, &owner), site.callee.clone())
                    })
                    .collect::<Vec<_>>();
                relabel_ffi_owner_results_by_call_site(&owner, &labels, &mut owner_results)
                    .map_err(|actual| {
                        format!(
                            "{}: canonical MIR FFI owner result count for '{}' does not match its Requires/Ensures call-site receipt census ({} sites, {} results)",
                            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                            owner.0,
                            labels.len(),
                            actual,
                        )
                    })?;
                modeled_ensures_sites.extend(selected_ensures_sites);
                results.extend(owner_results);
            }
            Err(error) => {
                if error.contains("canonical MIR FFI verifier encountered reachable trap") {
                    // A verified reachable trap is a concrete semantic
                    // failure, not an unsupported caller-slice boundary.
                    // Do not downgrade it to NITS while reconciling sites.
                    return Err(error);
                }
                results.extend(ffi_mir_requires_materialization_boundary_results(
                    program,
                    &sites,
                    &format!("verifier-only FFI MIR caller route was rejected: {error}"),
                ));
            }
        }
    }

    // A foreign postcondition is an observation about the external callee,
    // never an implementation proof or a caller-entry assumption. Retain one
    // explicit, artifact-free NITS row for each called declaration/site.
    results.extend(unmodeled_ensures_call_sites.iter().filter_map(|site| {
        let identity = FfiCallSiteIdentity {
            owner: crate::core::NodeId(site.owner.clone()),
            call_node: site.node_id.clone(),
            symbol: site.callee.clone(),
        };
        (!modeled_ensures_sites.contains(&identity))
            .then(|| ffi_unmodeled_ensures_result(program, site))
    }));
    Ok(results)
}

fn isolated_ffi_requires_helper_binding(
    program: &crate::core::CheckedProgram,
    owner: &crate::core::NodeId,
    callable: &crate::core::ResolvedCallable,
) -> Option<(crate::core::NodeId, crate::core::NodeId)> {
    use crate::core::ir::{ContractKind, ResolvedCallee, ResolvedExprKind};
    use crate::core::ResolvedCallKind;

    let root_contracts = callable
        .body
        .root
        .statements
        .iter()
        .filter(|statement| {
            matches!(
                &statement.kind,
                crate::core::ir::ResolvedStmtKind::Contract { .. }
            )
        })
        .map(|statement| statement.node_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let candidates = callable
        .contracts
        .iter()
        .filter(|contract| {
            contract.kind == ContractKind::Requires
                && root_contracts.contains(&contract.node_id)
                && matches!(&contract.condition.kind, ResolvedExprKind::Call(_))
        })
        .filter_map(|contract| {
            let ResolvedExprKind::Call(call) = &contract.condition.kind else {
                return None;
            };
            let ResolvedCallee::Function(helper) = &call.callee else {
                return None;
            };
            Some((contract, call, helper.clone()))
        })
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return None;
    }
    let (contract, call, helper_owner) = &candidates[0];
    if !owner.0.starts_with("function:")
        || owner.0["function:".len()..].contains("::")
        || call.effects.len() != 0
        || !call.type_arguments.is_empty()
        || call.permission.is_some()
        || !call.session.is_empty()
    {
        return None;
    }
    let helper = program.callable(helper_owner)?;
    let source_helper = program
        .functions()
        .values()
        .find(|function| function.node_id == *helper_owner)?;
    let bool_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
    if helper.signature.owner != *helper_owner
        || !helper_owner.0.starts_with("function:")
        || helper_owner.0["function:".len()..].contains("::")
        || !helper.signature.generic_parameters.is_empty()
        || !helper.signature.effects.is_empty()
        || helper.signature.result != bool_type
        || helper
            .body
            .root
            .result
            .as_ref()
            .is_none_or(|result| result.ty != bool_type)
        || helper.body.root.ty != bool_type
        || helper.body.parameters.len() != helper.signature.parameters.len()
        || !helper.body.captures.is_empty()
        || !helper.body.root.statements.is_empty()
        || !helper.body.resolved_calls.is_empty()
        || !helper.contracts.is_empty()
        || helper
            .signature
            .parameters
            .iter()
            .any(|parameter| parameter.has_default)
        || !helper.body.default_values.is_empty()
        || source_helper.pub_
        || source_helper.is_async
        || source_helper.is_comptime
        || program
            .native_declarations()
            .exports
            .contains_key(helper_owner)
        || program
            .call_sites_sorted()
            .into_iter()
            .any(|site| site.owner == helper_owner.0)
    {
        return None;
    }
    let helper_sites = program
        .call_sites_sorted()
        .into_iter()
        .filter(|site| site.owner == owner.0 && site.kind == ResolvedCallKind::Function)
        .collect::<Vec<_>>();
    if helper_sites.len() != 1
        || helper_sites[0].node_id != contract.condition.node_id
        || helper_sites[0].resolved_callee != Some(ResolvedCallee::Function(helper_owner.clone()))
        || helper_sites[0].argc != helper.signature.parameters.len()
    {
        return None;
    }
    Some((helper_owner.clone(), contract.condition.node_id.clone()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FfiCallSiteIdentity {
    owner: crate::core::NodeId,
    call_node: crate::core::NodeId,
    symbol: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FfiContractPhase {
    Requires,
    Ensures,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FfiContractSiteIdentity {
    call: FfiCallSiteIdentity,
    phase: FfiContractPhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FfiContractSiteOrder {
    /// Checker contract-root indices in canonical MIR receipt order.
    mir_ordered_checker_site_indexes: Vec<usize>,
    /// Checker phase roots for which the owner slice carries no MIR receipt.
    missing_checker_site_indexes: Vec<usize>,
}

/// Join Checker Requires/Ensures roots to the MIR per-callsite receipt census.
/// The call NodeId, owner and symbol identify the source invocation; phase
/// distinguishes its independent foreign precondition and postcondition.
fn order_ffi_contract_sites_by_identity(
    checker_sites: &[FfiContractSiteIdentity],
    mir_sites: &[FfiContractSiteIdentity],
) -> Result<FfiContractSiteOrder, String> {
    let mut matched = vec![false; checker_sites.len()];
    let mut mir_ordered_checker_site_indexes = Vec::with_capacity(mir_sites.len());

    for mir_site in mir_sites {
        let mut matches = checker_sites
            .iter()
            .enumerate()
            .filter(|(index, checker_site)| !matched[*index] && *checker_site == mir_site)
            .map(|(index, _)| index);
        let Some(index) = matches.next() else {
            return Err(format!(
                "{}: canonical MIR FFI {:?} receipt has no unique checker call-site identity (owner '{}', call '{}', symbol '{}')",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                mir_site.phase,
                mir_site.call.owner.0,
                mir_site.call.call_node.0,
                mir_site.call.symbol,
            ));
        };
        if matches.next().is_some() {
            return Err(format!(
                "{}: canonical MIR FFI {:?} receipt ambiguously matches checker call-site identities (owner '{}', call '{}', symbol '{}')",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                mir_site.phase,
                mir_site.call.owner.0,
                mir_site.call.call_node.0,
                mir_site.call.symbol,
            ));
        }
        matched[index] = true;
        mir_ordered_checker_site_indexes.push(index);
    }

    let missing_checker_site_indexes = matched
        .iter()
        .enumerate()
        .filter_map(|(index, matched)| (!matched).then_some(index))
        .collect();
    Ok(FfiContractSiteOrder {
        mir_ordered_checker_site_indexes,
        missing_checker_site_indexes,
    })
}

/// Attach stable presentation labels only after exact call-site identities
/// have been joined and the MIR owner's one-result-per-site count is checked.
/// The result records themselves (including artifact and Requires verdict)
/// remain intact.
fn relabel_ffi_owner_results_by_call_site(
    owner: &crate::core::NodeId,
    labels_in_mir_order: &[(String, String)],
    results: &mut [VerificationResult],
) -> Result<(), usize> {
    let result_indexes = results
        .iter()
        .enumerate()
        .filter(|(_, result)| result.func_name == owner.0)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if result_indexes.len() != labels_in_mir_order.len() {
        return Err(result_indexes.len());
    }

    for ((caller, callee), index) in labels_in_mir_order.iter().zip(result_indexes) {
        results[index].func_name = format!("{caller} calls {callee}");
    }
    Ok(())
}

#[cfg(test)]
mod ffi_owner_slice_result_identity_tests {
    use super::*;

    fn identity(call_node: &str, symbol: &str, phase: FfiContractPhase) -> FfiContractSiteIdentity {
        FfiContractSiteIdentity {
            call: FfiCallSiteIdentity {
                owner: crate::core::NodeId("function:ordinary".into()),
                call_node: crate::core::NodeId(call_node.into()),
                symbol: symbol.into(),
            },
            phase,
        }
    }

    fn owner_proof_result(
        owner: &crate::core::NodeId,
        status: VerifStatus,
        mir_hash: &str,
    ) -> VerificationResult {
        VerificationResult {
            func_name: owner.0.clone(),
            status,
            message: "canonical MIR FFI contract proof observation".into(),
            diagnostic: None,
            duration_us: 1,
            constraint_count: 1,
            artifact: Some(ProofArtifact {
                semantics_version: ProofArtifact::SEMANTICS_VERSION,
                proof_profile: ProofArtifact::PROFILE_MIR_FFI_EXPERIMENTAL_V1.into(),
                obligation_scope: ProofArtifact::SCOPE_FFI_REQUIRES_ENSURES_V1.into(),
                integer_model: "checked".into(),
                float_model: crate::core::mir::types::MIR_FFI_VERIFIER_FLOAT_MODEL.into(),
                solver_version: "z3 test".into(),
                source_hash: "source".into(),
                resolved_ir_hash: String::new(),
                mir_hash: mir_hash.into(),
                mir_route_receipt: None,
                vir_hash: String::new(),
                engine: ProofArtifact::ENGINE_MIR.into(),
            }),
            trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
        }
    }

    #[test]
    fn existing_owner_proof_is_reconciled_without_an_extra_boundary_row() {
        let owner = crate::core::NodeId("function:ordinary".into());
        let checker_sites = vec![identity(
            "expr.call:first",
            "nonempty",
            FfiContractPhase::Requires,
        )];
        let mir_sites = checker_sites.clone();
        let order = order_ffi_contract_sites_by_identity(&checker_sites, &mir_sites).unwrap();
        assert_eq!(order.mir_ordered_checker_site_indexes, vec![0]);
        assert!(order.missing_checker_site_indexes.is_empty());

        let labels = order
            .mir_ordered_checker_site_indexes
            .iter()
            .map(|index| {
                (
                    "ordinary".to_string(),
                    checker_sites[*index].call.symbol.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut results = vec![owner_proof_result(&owner, VerifStatus::Proven, "mir-one")];
        relabel_ffi_owner_results_by_call_site(&owner, &labels, &mut results).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].func_name, "ordinary calls nonempty");
        assert_eq!(results[0].status, VerifStatus::Proven);
        assert_eq!(
            results[0]
                .artifact
                .as_ref()
                .map(|artifact| artifact.mir_hash.as_str()),
            Some("mir-one")
        );
    }

    #[test]
    fn multiple_owner_results_follow_exact_mir_callsite_order() {
        let owner = crate::core::NodeId("function:ordinary".into());
        // The checker census order differs from the canonical receipt order.
        // Both sites share an owner and symbol. Distinct exact call NodeIds,
        // not owner or display labels, determine the site-to-result order.
        let checker_sites = vec![
            identity("expr.call:second", "nonempty", FfiContractPhase::Requires),
            identity("expr.call:first", "nonempty", FfiContractPhase::Requires),
        ];
        let mir_sites = vec![
            identity("expr.call:first", "nonempty", FfiContractPhase::Requires),
            identity("expr.call:second", "nonempty", FfiContractPhase::Requires),
        ];
        let order = order_ffi_contract_sites_by_identity(&checker_sites, &mir_sites).unwrap();
        assert_eq!(order.mir_ordered_checker_site_indexes, vec![1, 0]);
        assert!(order.missing_checker_site_indexes.is_empty());
        assert_eq!(
            order
                .mir_ordered_checker_site_indexes
                .iter()
                .map(|index| checker_sites[*index].call.call_node.0.as_str())
                .collect::<Vec<_>>(),
            vec!["expr.call:first", "expr.call:second"]
        );

        let labels = order
            .mir_ordered_checker_site_indexes
            .iter()
            .map(|index| {
                (
                    "ordinary".to_string(),
                    checker_sites[*index].call.symbol.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut results = vec![
            owner_proof_result(&owner, VerifStatus::Proven, "mir-first"),
            owner_proof_result(&owner, VerifStatus::Disproven, "mir-second"),
        ];
        relabel_ffi_owner_results_by_call_site(&owner, &labels, &mut results).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].func_name, "ordinary calls nonempty");
        assert_eq!(results[1].func_name, "ordinary calls nonempty");
        assert_eq!(results[0].status, VerifStatus::Proven);
        assert_eq!(results[1].status, VerifStatus::Disproven);
        assert_eq!(
            results[0]
                .artifact
                .as_ref()
                .map(|artifact| artifact.mir_hash.as_str()),
            Some("mir-first")
        );
        assert_eq!(
            results[1]
                .artifact
                .as_ref()
                .map(|artifact| artifact.mir_hash.as_str()),
            Some("mir-second")
        );
    }

    #[test]
    fn requires_and_ensures_share_call_identity_but_keep_distinct_phase_evidence() {
        let call = identity(
            "expr.call:shared",
            "checked_host",
            FfiContractPhase::Requires,
        )
        .call;
        let checker_sites = vec![
            FfiContractSiteIdentity {
                call: call.clone(),
                phase: FfiContractPhase::Requires,
            },
            FfiContractSiteIdentity {
                call: call.clone(),
                phase: FfiContractPhase::Ensures,
            },
        ];
        let mir_sites = vec![
            FfiContractSiteIdentity {
                call: call.clone(),
                phase: FfiContractPhase::Requires,
            },
            FfiContractSiteIdentity {
                call,
                phase: FfiContractPhase::Ensures,
            },
        ];
        let order = order_ffi_contract_sites_by_identity(&checker_sites, &mir_sites).unwrap();
        assert_eq!(order.mir_ordered_checker_site_indexes, vec![0, 1]);
        assert!(order.missing_checker_site_indexes.is_empty());

        let wrong_phase = vec![identity(
            "expr.call:shared",
            "checked_host",
            FfiContractPhase::Requires,
        )];
        let error = order_ffi_contract_sites_by_identity(&wrong_phase, &mir_sites)
            .expect_err("an Ensures receipt cannot alias a Requires identity");
        assert!(error.contains("canonical MIR FFI Ensures receipt"));
        assert!(error.contains("expr.call:shared"));
    }
}

fn ffi_mir_requires_materialization_boundary_results(
    program: &crate::core::CheckedProgram,
    sites: &[crate::core::ResolvedCallSite],
    reason: &str,
) -> Vec<VerificationResult> {
    sites
        .iter()
        .filter_map(|site| {
            let declaration = program
                .ffi_declarations()
                .values()
                .find(|declaration| declaration.symbol == site.callee)?;
            if !declaration.requires.is_declared() {
                return None;
            }
            let owner = crate::core::NodeId(site.owner.clone());
            let caller = checked_caller_name(program, &owner);
            let message = format!(
                "{}: checker-owned FFI declaration '{}' Requires at exact call site '{}' (owner '{}') has no verifier MIR proof result: {reason}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                site.callee,
                site.node_id.0,
                site.owner,
            );
            Some(VerificationResult {
                func_name: format!("{caller} calls {}", site.callee),
                status: VerifStatus::NotInTrustedSubset,
                diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    message.clone(),
                    site.origin.user_span(),
                )),
                message,
                duration_us: 0,
                constraint_count: 0,
                artifact: None,
                trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
            })
        })
        .collect()
}

/// Find the one exact top-level FFI call shape supported by the verifier-only
/// String-record owner slice. This is a structural preflight only: it does
/// not materialize or authorize an execution route.
fn isolated_ffi_string_record_owner_slice_callsite(
    program: &crate::core::CheckedProgram,
) -> Option<crate::core::ResolvedCallSite> {
    use crate::core::ir::ResolvedFfiParameterMode;

    // Keep this exception narrower than the general verifier-only FFI API:
    // one top-level caller, one direct extern call, and one non-generic
    // repr(C) String-bearing record passed and returned by value. The FFI API
    // remains the owner of call-site/source-census reconciliation and emits
    // the `verifier-ffi-owner-slice-v1` receipt; this selector only decides
    // whether that existing owner slice may answer the default dual request.
    let extern_sites = program
        .call_sites_sorted()
        .into_iter()
        .filter(|site| {
            site.kind == crate::core::ResolvedCallKind::Extern
                && !ffi_site_is_declaration_root_call(program, site)
        })
        .collect::<Vec<_>>();
    let [site] = extern_sites.as_slice() else {
        return None;
    };
    if !site.arity_matches() {
        return None;
    }

    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if scalar_ffi_map_v2_or_observer_composition_failure(admission).is_some()
        || crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_admitted(admission)
        || admission.map_root.is_candidate()
        || admission.map_any_owner.is_candidate()
        || admission.ordinary_map_owned_any_v2.has_profile_signal()
        || admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
        || admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal()
        || !program.actors().is_empty()
        || !program.sessions().is_empty()
        || !program.impls().is_empty()
    {
        return None;
    }
    let implicit_main_flow_nodes = program
        .flows()
        .values()
        .filter(|flow| {
            flow.id.0 == "Main"
                && matches!(
                    &flow.origin,
                    crate::core::Origin::RuntimeSystem { rule, .. }
                        if rule == "progressive.main"
                )
        })
        .map(|flow| flow.node_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let flows_are_only_implicit_main = !implicit_main_flow_nodes.is_empty()
        && program.flows().len() == implicit_main_flow_nodes.len()
        && program.transitions().values().all(|transition| {
            matches!(
                &transition.origin,
                crate::core::Origin::RuntimeSystem { parent, .. }
                    if implicit_main_flow_nodes.contains(parent)
            )
        });
    if !program.flows().is_empty() && !flows_are_only_implicit_main {
        return None;
    }

    let owner = crate::core::NodeId(site.owner.clone());
    if !owner.0.starts_with("function:")
        || owner.0["function:".len()..].contains("::")
        || program.native_declarations().exports.contains_key(&owner)
    {
        return None;
    }
    let Some(callable) = program.callable(&owner) else {
        return None;
    };
    let caller_contract_roots = callable.body.contract_proof_roots();
    if !callable.signature.generic_parameters.is_empty()
        || callable.signature.parameters.len() != 1
        || callable.contracts.len() != 1
        || callable.contracts[0].kind != crate::core::ir::ContractKind::Requires
        || caller_contract_roots.len() != 1
        || caller_contract_roots[0].kind != crate::core::ir::ContractKind::Requires
        || !callable.body.math_proof_roots().is_empty()
        || program
            .callables()
            .iter()
            .any(|(candidate_owner, candidate)| {
                candidate_owner != &owner
                    && (!candidate.body.contract_proof_roots().is_empty()
                        || !candidate.body.math_proof_roots().is_empty())
            })
    {
        return None;
    }

    let contract_declarations = program
        .ffi_declarations()
        .values()
        .filter(|declaration| declaration.has_declared_contract())
        .collect::<Vec<_>>();
    if contract_declarations.len() != 1 || contract_declarations[0].symbol != site.callee {
        return None;
    }
    let declarations = program
        .ffi_declarations()
        .values()
        .filter(|declaration| declaration.symbol == site.callee)
        .collect::<Vec<_>>();
    let [declaration] = declarations.as_slice() else {
        return None;
    };
    if declaration.abi != "C"
        || declaration.invocation.variadic
        || !matches!(
            &declaration.requires,
            crate::core::ir::ResolvedFfiContractRoot::Ready { .. }
        )
        || !matches!(
            &declaration.ensures,
            crate::core::ir::ResolvedFfiContractRoot::Ready { .. }
        )
    {
        return None;
    }
    let signature = declaration.signature.as_ref().ok()?;
    let [parameter] = signature.parameters.as_slice() else {
        return None;
    };
    if parameter.mode != ResolvedFfiParameterMode::Value
        || parameter.ty != signature.result
        || callable.signature.parameters[0].ty != parameter.ty
        || callable.signature.result != signature.result
    {
        return None;
    }
    let type_catalog =
        crate::core::mir::types::MirTypeCatalog::from_checked_program(program).ok()?;
    if !type_catalog.is_canonical_ffi_verifier_string_record(&signature.result) {
        return None;
    }

    Some((**site).clone())
}

/// Whether the checked program has the exact verifier-only `repr(C)`
/// String-record FFI shape admitted by the default dual verifier. The caller
/// must still dispatch to `verify_checked_dual`, which repeats this shared
/// predicate and validates the existing Checker-bound owner-slice receipt.
/// This predicate never grants ScalarFfi execution admission.
pub fn has_default_dual_ffi_string_record_owner_slice(
    program: &crate::core::CheckedProgram,
) -> bool {
    isolated_ffi_string_record_owner_slice_callsite(program).is_some()
}

/// Verify the exact isolated FFI String-record owner slice after its shared
/// structural preflight. The existing FFI verifier owns callsite/source
/// census reconciliation and emits the non-executable
/// `verifier-ffi-owner-slice-v1` receipt.
fn verify_dual_isolated_ffi_string_record_owner_slice(
    program: &crate::core::CheckedProgram,
    source_hash: &str,
) -> Option<Result<Vec<VerificationResult>, String>> {
    let site = isolated_ffi_string_record_owner_slice_callsite(program)?;
    let owner = crate::core::NodeId(site.owner.clone());

    let results = match verify_ffi_checked_with_source_hash(program, source_hash.to_owned()) {
        Ok(results) => results,
        Err(error) => return Some(Err(error)),
    };
    let expected_name = format!(
        "{} calls {}",
        owner.0.trim_start_matches("function:"),
        site.callee
    );
    if results.len() != 1 || results[0].func_name != expected_name {
        return Some(Err(format!(
            "{}: isolated repr(C) String-record FFI owner slice produced an unexpected result set for Checker callsite '{}'",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            site.node_id.0
        )));
    }
    let result = &results[0];
    if let Some(artifact) = result.artifact.as_ref() {
        let receipt_profile = artifact
            .mir_route_receipt
            .as_ref()
            .map(|receipt| receipt.profile.as_str());
        if artifact.engine != ProofArtifact::ENGINE_MIR
            || artifact.source_hash != source_hash
            || artifact.obligation_scope != ProofArtifact::SCOPE_FFI_REQUIRES_ENSURES_V1
            || receipt_profile != Some("verifier-ffi-owner-slice-v1")
        {
            return Some(Err(format!(
                "{}: isolated repr(C) String-record FFI result lost its Checker-bound verifier owner-slice provenance",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )));
        }
    }
    if result.status != VerifStatus::Proven
        && result
            .diagnostic
            .as_ref()
            .is_none_or(|diagnostic| diagnostic.span != site.origin.user_span())
    {
        return Some(Err(format!(
            "{}: isolated repr(C) String-record FFI result lost Checker callsite '{}' diagnostics",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            site.node_id.0
        )));
    }
    Some(Ok(results))
}

fn verify_dual_isolated_ffi_mir_route(
    program: &crate::core::CheckedProgram,
    source_hash: &str,
) -> Option<Result<Vec<VerificationResult>, String>> {
    let has_called_ffi_contract = program.call_sites_sorted().iter().any(|site| {
        site.kind == crate::core::ResolvedCallKind::Extern
            && !ffi_site_is_declaration_root_call(program, site)
            && program.ffi_declarations().values().any(|declaration| {
                declaration.symbol == site.callee && declaration.has_declared_contract()
            })
    });
    if !has_called_ffi_contract {
        return None;
    }
    // Scalar FFI already has an independent canonical MIR/dual route.  The
    // declaration-root slice only covers ABI shapes that still lack that
    // route; it must not replace a scalar MIR proof or steal a mixed Map
    // owner request.
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if let Some(error) = scalar_ffi_map_v2_or_observer_composition_failure(admission) {
        return Some(Err(error));
    }
    if crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_admitted(admission)
        || admission.map_root.is_candidate()
        || admission.map_any_owner.is_candidate()
        || admission.ordinary_map_owned_any_v2.has_profile_signal()
        || admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
        || admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal()
    {
        return None;
    }
    // `check_program` injects the RuntimeSystem `Main` flow for ordinary
    // scripts. It carries no user contract and is safe to ignore here, but
    // an explicit/user Flow (or a second generated flow) keeps the profile
    // deferred to the normal dual owner.
    let implicit_main_flow_nodes = program
        .flows()
        .values()
        .filter(|flow| {
            flow.id.0 == "Main"
                && matches!(
                    &flow.origin,
                    crate::core::Origin::RuntimeSystem { rule, .. }
                        if rule == "progressive.main"
                )
        })
        .map(|flow| flow.node_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let flows_are_only_implicit_main = !implicit_main_flow_nodes.is_empty()
        && program.flows().len() == implicit_main_flow_nodes.len()
        && program.transitions().values().all(|transition| {
            matches!(
                &transition.origin,
                crate::core::Origin::RuntimeSystem { parent, .. }
                    if implicit_main_flow_nodes.contains(parent)
            )
        });
    if (!program.flows().is_empty() && !flows_are_only_implicit_main)
        || !program.actors().is_empty()
        || !program.sessions().is_empty()
        || !program.impls().is_empty()
    {
        return None;
    }
    if program
        .callables()
        .values()
        .any(has_non_default_callable_proof_roots)
    {
        return None;
    }
    let Some(results) = ffi_compat::verify_literal_contracts(program, source_hash) else {
        return Some(Err(format!(
            "{}: called non-scalar extern contract is outside the typed literal declaration-root profile",
            crate::core::mir::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE
        )));
    };
    if matches!(&results, Ok(batch) if batch.is_empty()) {
        // An empty successful FFI batch is not proof ownership. Let the
        // ordinary dual path materialize and validate the canonical request.
        return None;
    }
    Some(results)
}

fn has_non_default_callable_proof_roots(callable: &crate::core::ResolvedCallable) -> bool {
    let worklist = callable.proof_point_worklist();
    if !worklist.validate_against(callable) || !worklist.boundaries.is_empty() {
        return true;
    }
    let supported_default_math = worklist
        .default_roots
        .iter()
        .filter(|root| root.supports_local_only_profile(callable))
        .flat_map(|root| root.points.iter().map(|point| point.id.statement.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    !callable.body.contract_proof_roots().is_empty()
        || callable
            .body
            .math_proof_roots()
            .iter()
            .any(|(root, _)| !supported_default_math.contains(root))
        || worklist.default_roots.iter().any(|root| {
            (!root.points.is_empty() || !root.calls.is_empty() || !root.boundaries.is_empty())
                && !root.supports_local_only_profile(callable)
        })
}

fn ffi_unmodeled_ensures_result(
    program: &crate::core::CheckedProgram,
    site: &crate::core::ResolvedCallSite,
) -> VerificationResult {
    let caller_owner = crate::core::NodeId(site.owner.clone());
    let caller = checked_caller_name(program, &caller_owner);
    let declaration_owner = program
        .ffi_declarations()
        .values()
        .find(|declaration| declaration.symbol == site.callee)
        .map(|declaration| declaration.declaration.0.as_str())
        .unwrap_or("<missing>");
    let message = format!(
        "{}: checker-owned FFI declaration '{}' (owner '{}') Ensures at call site '{}' (owner '{}', span {}) is outside canonical proof coverage; any Requires result is independent and does not prove the foreign postcondition",
        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
        site.callee,
        declaration_owner,
        site.node_id.0,
        caller_owner.0,
        site.origin.user_span()
    );
    VerificationResult {
        func_name: format!("{} -> extern {} Ensures", caller, site.callee),
        status: VerifStatus::NotInTrustedSubset,
        diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            message.clone(),
            site.origin.user_span(),
        )),
        message,
        duration_us: 0,
        constraint_count: 0,
        artifact: None,
        trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
    }
}

fn ffi_mir_materialization_boundary_results(
    program: &crate::core::CheckedProgram,
    called_contract_names: &std::collections::BTreeSet<String>,
    reason: &str,
) -> Vec<VerificationResult> {
    let mut results = Vec::new();
    for site in program.call_sites_sorted().into_iter().filter(|site| {
        site.kind == crate::core::ResolvedCallKind::Extern
            && called_contract_names.contains(&site.callee)
            && !ffi_site_is_declaration_root_call(program, site)
    }) {
        let Some(declaration) = program
            .ffi_declarations()
            .values()
            .find(|declaration| declaration.symbol == site.callee)
        else {
            continue;
        };
        for (declared, phase) in [
            (declaration.requires.is_declared(), "Requires"),
            (declaration.ensures.is_declared(), "Ensures"),
        ] {
            if !declared {
                continue;
            }
            let caller_owner = crate::core::NodeId(site.owner.clone());
            let caller = checked_caller_name(program, &caller_owner);
            let message = format!(
                "{}: checker-owned FFI declaration '{}' at call site '{}' has no verifier MIR proof graph: {reason}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                site.callee,
                site.node_id.0
            );
            let func_name = if phase == "Requires" {
                format!("{caller} calls {}", site.callee)
            } else {
                format!("{caller} -> extern {} Ensures", site.callee)
            };
            results.push(VerificationResult {
                func_name,
                status: VerifStatus::NotInTrustedSubset,
                diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    message.clone(),
                    site.origin.user_span(),
                )),
                message,
                duration_us: 0,
                constraint_count: 0,
                artifact: None,
                trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
            });
        }
    }
    results
}

fn ffi_no_z3_contract_results(
    program: &crate::core::CheckedProgram,
    scalar_mir_route: bool,
) -> Vec<VerificationResult> {
    let mut results = Vec::new();
    for site in program.call_sites_sorted().into_iter().filter(|site| {
        site.kind == crate::core::ResolvedCallKind::Extern
            && !ffi_site_is_declaration_root_call(program, site)
    }) {
        let Some(declaration) = program
            .ffi_declarations()
            .values()
            .find(|declaration| declaration.symbol == site.callee)
        else {
            continue;
        };
        let caller = checked_caller_name(program, &crate::core::NodeId(site.owner.clone()));
        if declaration.requires.is_declared() {
            results.push(ffi_infrastructure_error(format!(
                "{caller} calls {}",
                site.callee
            )));
        }
        if declaration.ensures.is_declared() {
            if scalar_mir_route {
                results.push(ffi_infrastructure_error(format!(
                    "{caller} -> extern {} Ensures",
                    site.callee
                )));
            } else {
                results.push(ffi_unmodeled_ensures_result(program, site));
            }
        }
    }
    results
}

fn ffi_mixed_map_requires_result(
    program: &crate::core::CheckedProgram,
    site: &crate::core::ResolvedCallSite,
) -> VerificationResult {
    let caller = checked_caller_name(program, &crate::core::NodeId(site.owner.clone()));
    let message = format!(
        "{}: called extern '{}' Requires at call site '{}' is Deferred because the Actor compatibility request also contains an independently-owned Map profile without a combined verifier receipt",
        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
        site.callee,
        site.node_id.0
    );
    VerificationResult {
        func_name: format!("{caller} calls {}", site.callee),
        status: VerifStatus::NotInTrustedSubset,
        diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            message.clone(),
            site.origin.user_span(),
        )),
        message,
        duration_us: 0,
        constraint_count: 0,
        artifact: None,
        trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
    }
}

fn checked_caller_name(
    program: &crate::core::CheckedProgram,
    owner: &crate::core::NodeId,
) -> String {
    if let Some(function) = program.functions().get(owner) {
        return function.qualified_name.clone();
    }
    if let Some(transition) = program
        .transitions()
        .values()
        .find(|transition| &transition.node_id == owner)
    {
        return format!(
            "{}::{} from {}",
            transition.id.flow.0, transition.id.event, transition.id.source.name
        );
    }
    owner.0.clone()
}

fn ffi_infrastructure_error(func_name: String) -> VerificationResult {
    VerificationResult {
        func_name,
        status: VerifStatus::InfrastructureError,
        message: "Z3 solver not available".into(),
        diagnostic: None,
        duration_us: 0,
        constraint_count: 0,
        artifact: None,
        trusted_subset_domain: None,
    }
}

/// Check whether the Z3 solver is available at runtime.
pub fn is_z3_available() -> bool {
    // The integration CLI needs a deterministic way to exercise the same
    // no-Z3 branch on hosts where libz3 is installed. Keep this hook confined
    // to debug builds; release users always receive the real runtime check.
    #[cfg(debug_assertions)]
    if std::env::var_os("MIMI_TEST_FORCE_NO_Z3").is_some() {
        return false;
    }
    Verifier::new().is_ok()
}

/// 0.34.44 (ADR-008 §3): dual-engine verification with fail-closed divergence.
///
/// Runs BOTH engines on the same checked program and merges their verdicts:
/// - primary = the Resolved engine (ADR-008 §1 main judgment);
/// - secondary = the canonical MIR engine on materialized typed requests;
///   deferred requests are either admitted by a closed canonical profile or
///   returned as an explicit coverage result;
/// - per-function verdict classes that disagree produce a fail-closed result
///   carrying the new E0439 divergence diagnostic — neither engine is trusted
///   alone when they disagree.
///
/// Used by `mimi verify` (the CLI main path). Library callers that need a
/// single engine keep using `verify_checked` / `Verifier::verify_checked`.
pub fn verify_checked_dual(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    program
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    if let Some(reason) = program.dynamic_protocol_method_boundary_reason() {
        return Err(reason);
    }
    let z3_available = is_z3_available();
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if let Some(error) = crate::core::mir::scalar_export_map_owner_composition_failure(admission) {
        let reason = error.to_diagnostic().message;
        if !z3_available {
            return Err(reason);
        }
        let mut primary = Verifier::new()?;
        primary.set_source_hash(source_hash.clone());
        let primary_results = primary.verify_checked_without_actor_ffi(program);
        let owner = program
            .native_declarations()
            .exports
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| crate::core::NodeId("program".into()));
        let boundary = crate::core::mir::verification::MirVerificationBoundary {
            owner,
            point: None,
            reason,
        };
        return Ok(dual_verifier_coverage_results(
            program,
            primary_results,
            &[boundary],
            &source_hash,
        ));
    }
    // A program with only isolated FFI obligations may use the verifier-only
    // MIR constructor even when its ABI is outside ScalarFfi execution
    // admission. The resulting route receipt is never passed to native or
    // bytecode consumers.
    if let Some(results) = verify_dual_isolated_ffi_string_record_owner_slice(program, &source_hash)
    {
        return results;
    }
    if let Some(results) = verify_dual_isolated_ffi_mir_route(program, &source_hash) {
        return results;
    }
    // Keep the dual entry point on the same admission boundary before its
    // typed request fast path.  Otherwise a complete request envelope could
    // bypass the ordinary Map result-settlement check used by the single
    // verifier and canonical CLI route.
    validate_public_verifier_mir_boundary(program)?;
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControlResult
    {
        return verify_checked_ordinary_map_owned_any_v2_map_set_drop_control_result(
            program,
            source_hash,
        );
    }
    if admission.ordinary_map_owned_any_v2.is_direct_print_result() {
        return verify_checked_ordinary_map_owned_any_v2_direct_print_result(program, source_hash);
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesResultCrosscallStringCaller
    {
        return verify_checked_ordinary_map_owned_any_v2_values_result_crosscall_string_caller(
            program,
            source_hash,
            false,
        );
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesResultCrosscallStringCallerIndex1
    {
        return verify_checked_ordinary_map_owned_any_v2_values_result_crosscall_string_caller(
            program,
            source_hash,
            true,
        );
    }
    let actor_compatibility = is_actor_proof_compatibility(program);
    if !z3_available {
        if actor_compatibility {
            return Ok(actor_compatibility_no_z3_results(program, source_hash));
        }
        // The dual wrapper preserves the same no-solver result algebra as the
        // single checked entry. Never enter a closed MIR profile that would
        // attempt to initialize SolverSession without Z3.
        return Ok(ctx::mock_verify_checked(program));
    }

    // Owner-v2 is a checker-owned canonical profile, but CompleteCoverage is
    // still diagnostic-only while the shared production gate is closed. Keep
    // it out of the generic typed-request merge so a raw MIR proof cannot be
    // counted as production readiness; the internal profile verifier adds
    // the owner-scoped NITS result after collecting diagnostic observations.
    match admission.ordinary_map_owned_any_v2 {
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapEmptyRootRootQueriesV1 => {
            return verify_checked_map_empty_root_root_queries_v1(program, source_hash);
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IsEmpty => {
            return verify_checked_ordinary_map_owned_any_v2_is_empty(program, source_hash);
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RecordUnit => {
            return verify_checked_ordinary_map_owned_any_v2_record_unit(program, source_hash);
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControl => {
            return verify_checked_ordinary_map_owned_any_v2_map_set_drop_control(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::BranchLocalPersistentSetDiamond => {
            return verify_checked_ordinary_map_owned_any_v2_branch_local_set_diamond(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ZeroMissI32 => {
            return verify_checked_ordinary_map_owned_any_v2_zero_miss_i32(program, source_hash);
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RemovedKeyCheckedMiss => {
            return verify_checked_ordinary_map_owned_any_v2_removed_key_checked_miss(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation => {
            return verify_checked_ordinary_map_owned_any_v2_clone_isolation(program, source_hash);
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::PersistentRootAliasReplacement => {
            return verify_checked_ordinary_map_owned_any_v2_persistent_root_alias_replacement(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CheckedGet => {
            return verify_checked_ordinary_map_owned_any_v2_get(program, source_hash);
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedKeyGet => {
            return verify_checked_ordinary_map_owned_any_v2_computed_key_get(program, source_hash);
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedStringKeyOwnerChain => {
            return verify_checked_ordinary_map_owned_any_v2_computed_string_key_owner_chain(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::DynamicKeysGet => {
            return verify_checked_ordinary_map_owned_any_v2_dynamic_keys_get(program, source_hash);
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::DirectPrint => {
            return verify_checked_ordinary_map_owned_any_v2_direct_print(program, source_hash);
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::DirectPrintResult => {
            return verify_checked_ordinary_map_owned_any_v2_direct_print_result(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControlResult => {
            return verify_checked_ordinary_map_owned_any_v2_map_set_drop_control_result(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesFieldMove => {
            return verify_checked_ordinary_map_owned_any_v2_values_field_move(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesLocalIndexI32 => {
            return verify_checked_ordinary_map_owned_any_v2_values_local_index_i32(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesLocalIndexI32ResultBranch => {
            return verify_checked_ordinary_map_owned_any_v2_values_local_index_i32_result_branch(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesHeterogeneousStringIndex1 => {
            return verify_checked_ordinary_map_owned_any_v2_values_heterogeneous_string_index1(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::KeysSnapshot => {
            return verify_checked_ordinary_map_owned_any_v2_keys_snapshot(program, source_hash);
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesSnapshotReplacement => {
            return verify_checked_ordinary_map_owned_any_v2_values_snapshot_replacement(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshot => {
            return verify_checked_ordinary_map_owned_any_v2_values_mixed_snapshot(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshotRemove => {
            return verify_checked_ordinary_map_owned_any_v2_values_mixed_snapshot_remove(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesProducerCaller => {
            let route = crate::core::mir::materialize_canonical_mir_route(program, None).map_err(
                |error| {
                    let code = error
                        .diagnostic_code()
                        .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                    format!("{code}: Values producer-caller verifier route failed: {error}")
                },
            )?;
            let receipt = route
                .ordinary_map_owned_any_v2_values_producer_caller_route_receipt
                .ok_or_else(|| {
                    format!(
                        "{}: Values producer-caller admission did not materialize its exact verifier receipt",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    )
                })?;
            return verify_mir_with_ordinary_map_owned_any_v2_values_producer_caller_route(
                program,
                &route.program,
                &receipt,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesCrosscallStringCaller => {
            return Err(format!(
                "{}: Values cross-call String caller requires its exact Checker-aware verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesCrosscallStringCallerIndex1 => {
            let route = crate::core::mir::materialize_canonical_mir_route(program, None).map_err(
                |error| {
                    let code = error
                        .diagnostic_code()
                        .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                    format!(
                        "{code}: Values cross-call String caller index-one verifier route failed: {error}"
                    )
                },
            )?;
            let receipt = route.program.route_receipt(
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE,
            );
            return verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
                program,
                &route.program,
                &receipt,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesResultCrosscallStringCaller => {
            return verify_checked_ordinary_map_owned_any_v2_values_result_crosscall_string_caller(
                program,
                source_hash,
                false,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesResultCrosscallStringCallerIndex1 => {
            return verify_checked_ordinary_map_owned_any_v2_values_result_crosscall_string_caller(
                program,
                source_hash,
                true,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesResultProducerCaller => {
            return Err(format!(
                "{}: NotInTrustedSubset: Result Values producer/caller has Checker and MIR receipts, but no implemented Z3 theorem consumer",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapRootProducerCaller => {
            let route = crate::core::mir::materialize_canonical_mir_route(program, None).map_err(
                |error| {
                    let code = error
                        .diagnostic_code()
                        .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
                    format!("{code}: Map-root producer/caller verifier route failed: {error}")
                },
            )?;
            let receipt = route
                .ordinary_map_owned_any_v2_map_root_producer_caller_route_receipt
                .ok_or_else(|| {
                    format!(
                        "{}: Map-root producer/caller admission did not materialize its exact verifier receipt",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    )
                })?;
            return verify_mir_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
                program,
                &route.program,
                &receipt,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapRootGetAnyCaller => {
            return verify_checked_ordinary_map_owned_any_v2_map_root_get_any_caller(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::SetGetAnyI32 => {
            return Err(format!(
                "{}: ordinary Map Set/GetAny-i32 requires its dedicated CheckedProgram-aware L2 verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::SetGetAnyResultI32 => {
            let route =
                crate::core::mir::materialize_ordinary_map_set_get_any_result_i32_verifier_route(
                    program,
                )
                .map_err(|reason| {
                    format!(
                        "{}: ordinary Map Set/GetAny-Result-i32 L2 verifier route rejected: {reason}",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    )
                })?;
            let receipt = route.route_receipt(
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_L2_VERIFIER_PROFILE,
            );
            return verify_mir_with_ordinary_map_owned_any_v2_set_get_any_result_i32_l2_verifier_route(
                program,
                &route,
                &receipt,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultI32 => {
            return verify_checked_ordinary_map_owned_any_v2_get_or_default_i32(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultString => {
            return verify_checked_ordinary_map_owned_any_v2_get_or_default_string(
                program,
                source_hash,
            );
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::StagedGetValues => {
            return verify_staged_ordinary_map_owned_any_v2(program, source_hash);
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage
            if !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY =>
        {
            return verify_closed_mir_program(program, source_hash)?.ok_or_else(|| {
                format!(
                    "{}: complete ordinary Map/Any owner-v2 diagnostic verifier did not materialize a route",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            });
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage => {
            return Err(format!(
                "{}: Checker recognized an ordinary Map/Any owner-v2 candidate but withheld complete operation-attempt and result-receipt coverage",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::OutsideProfile
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage => {}
    }
    if admission.ordinary_map_owner_lifecycle_observer_v1_complete() {
        return verify_closed_mir_program(program, source_hash)?.ok_or_else(|| {
            format!(
                "{}: complete ordinary Map lifecycle observer-v1 verifier did not materialize its exact route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        });
    }
    let map_profile_complete = admission.map_root_complete()
        || admission.map_any_owner_complete()
        || admission.ordinary_map_owned_any_v2_complete()
        || admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
        || admission.ordinary_map_owner_lifecycle_observer_v1_complete()
        || admission.ordinary_map_owned_get_i32_string_v1_complete();
    if map_profile_complete {
        // Prefer the typed request envelope when it is available so the
        // public dual API preserves the same canonical-contract receipt as
        // `verify_checked`.  Some ownership-only profiles are deferred from
        // that envelope; those still use their exact profile receipt below.
        match mir_request::prepare(program) {
            mir_request::Preparation::Complete(request) => {
                return mir_request::verify(&request, program, source_hash.clone());
            }
            mir_request::Preparation::Deferred(_) => {
                if let Some(results) = verify_closed_mir_program(program, source_hash.clone())? {
                    return Ok(results);
                }
            }
        }
    }
    // A contract-bearing Actor without a Map/Any owner still has an explicit
    // compatibility verifier.  Keep that named boundary for proof-only
    // requests, while mixed Actor+Map candidates remain fail-closed below and
    // can never hide the ownership route behind this adapter.
    let map_owner_candidate = admission.map_root.is_candidate()
        || admission.map_any_owner.is_candidate()
        || admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
        || admission
            .ordinary_map_owner_lifecycle_observer_v1
            .has_profile_signal()
        || admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal();
    if actor_compatibility && !map_owner_candidate {
        if let Some(batch) = flow_mir::verify_actor_flow_projection_v1(program, &source_hash)? {
            let mut primary = Verifier::new()?;
            primary.set_source_hash(source_hash);
            let resolved_results = primary.verify_checked(program);
            return merge_actor_flow_dual_results(resolved_results, batch);
        }
        let mut verifier = Verifier::new()?;
        verifier.set_source_hash(source_hash);
        return Ok(verifier.verify_checked(program));
    }
    let mut deferred_boundaries = Vec::new();
    if z3_available {
        match mir_request::prepare(program) {
            mir_request::Preparation::Complete(request) => {
                // Independent primary is still the Resolved expression/body
                // encoder. The secondary symbolically executes canonical MIR.
                // They share checker identities, never a solver traversal.
                let mut primary = Verifier::new()?;
                primary.set_source_hash(source_hash.clone());
                let mut resolved_results = primary.verify_checked(program);
                mir_request::normalize_primary_names(&mut resolved_results, &request);
                let mut mir_results = mir_request::verify(&request, program, source_hash.clone())?;
                mir_request::normalize_primary_names(&mut mir_results, &request);
                let peer_gaps =
                    mir_request::peer_coverage_boundaries(&resolved_results, &mir_results);
                if !peer_gaps.is_empty() {
                    return Ok(dual_verifier_coverage_results(
                        program,
                        resolved_results,
                        &peer_gaps,
                        &source_hash,
                    ));
                }
                return Ok(merge_engine_verdicts_with_secondary_label(
                    resolved_results,
                    mir_results,
                    "canonical_mir",
                ));
            }
            mir_request::Preparation::Deferred(boundaries) => {
                deferred_boundaries = boundaries;
                // An isolated FFI MIR batch is already the canonical proof
                // result for this request. Any other body proof root remains
                // with the general dual owner below.
                if let Some(results) = verify_dual_isolated_ffi_mir_route(program, &source_hash) {
                    return results;
                }
            }
        }
    }
    validate_public_verifier_mir_boundary(program)?;
    match verify_closed_mir_program(program, source_hash.clone()) {
        Ok(Some(results))
            if !deferred_boundaries.is_empty()
                && results.iter().any(|result| {
                    result
                        .message
                        .contains("proof worklist coverage gap: DeferredBoundary")
                }) =>
        {
            let mut primary = Verifier::new()?;
            primary.set_source_hash(source_hash.clone());
            let resolved_results = primary.verify_checked_without_actor_ffi(program);
            return Ok(dual_verifier_coverage_results(
                program,
                resolved_results,
                &deferred_boundaries,
                &source_hash,
            ));
        }
        Ok(Some(results)) => return Ok(results),
        Ok(None) => {}
        Err(error)
            if !deferred_boundaries.is_empty()
                && error.contains("proof worklist coverage gap: DeferredBoundary") =>
        {
            let mut primary = Verifier::new()?;
            primary.set_source_hash(source_hash.clone());
            let resolved_results = primary.verify_checked_without_actor_ffi(program);
            return Ok(dual_verifier_coverage_results(
                program,
                resolved_results,
                &deferred_boundaries,
                &source_hash,
            ));
        }
        Err(error) => return Err(error),
    }
    // Primary engine: resolved (verifies from Resolved IR).
    let mut primary = Verifier::new()?;
    primary.set_source_hash(source_hash.clone());
    let resolved_results = primary.verify_checked_without_actor_ffi(program);
    // Secondary engine: canonical MIR only. If the checker could not
    // materialize an admitted profile, retain the primary observations but
    // downgrade every proof-bearing result to an explicit coverage boundary;
    // reopening the retained AST here would make the dual API silently depend
    // on the retired Flow/VIR owner.
    if let Some(results) =
        verify_deferred_dual_mir(program, source_hash.clone(), resolved_results.clone())?
    {
        return Ok(results);
    }
    Ok(dual_verifier_coverage_results(
        program,
        resolved_results,
        &deferred_boundaries,
        &source_hash,
    ))
}

/// Consume a deferred dual request with the same checker-owned canonical MIR
/// route used by direct consumers. Closed profile adapters run before this
/// helper; this extra attempt covers a typed graph whose proof request is
/// deferred for a consumer-specific reason but whose complete MIR graph and
/// verifier capability gate are still available. Compatibility materializing
/// errors are an admission miss and are handled by
/// [`dual_verifier_coverage_results`]; complete-route errors are hard errors.
fn verify_deferred_dual_mir(
    program: &crate::core::CheckedProgram,
    source_hash: String,
    mut resolved_results: Vec<VerificationResult>,
) -> Result<Option<Vec<VerificationResult>>, String> {
    let route = match crate::core::mir::materialize_canonical_mir_route(program, None) {
        Ok(route) => route,
        Err(crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility { .. }) => {
            return Ok(None);
        }
        Err(error) => {
            let code = error
                .diagnostic_code()
                .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
            return Err(format!("{code}: {error}"));
        }
    };
    let canonical = route.program;
    crate::verifier::validate_mir_capabilities(&canonical).map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: deferred dual canonical verifier consumer rejected the graph: {errors:?}"
        )
    })?;
    let receipt = canonical.route_receipt("dual-canonical-mir-v1");
    receipt.validate_against_program(&canonical)?;
    let mut mir_results =
        crate::verifier::verify_mir_with_route_receipt(&canonical, &receipt, source_hash)?;
    // The typed MIR request path normalizes checker owners before merge. The
    // route materializer does not carry that table, so use the same source
    // callable labels before comparing the independent engine observations.
    for result in &mut resolved_results {
        if let Some(name) = result.func_name.strip_prefix("function:") {
            result.func_name = name.to_string();
        }
    }
    for result in &mut mir_results {
        if let Some(name) = result.func_name.strip_prefix("function:") {
            result.func_name = name.to_string();
        }
    }
    let peer_gaps = mir_request::peer_coverage_boundaries(&resolved_results, &mir_results);
    if !peer_gaps.is_empty() {
        // A route that materializes only a subgraph cannot claim a dual
        // proof for the whole checked program. Return to the outer coverage
        // adapter so every primary owner is downgraded to one structured
        // NotInTrustedSubset result carrying the exact missing peer owners
        // and source/Resolved provenance; never merge a partial batch.
        return Ok(None);
    }
    Ok(Some(merge_engine_verdicts_with_secondary_label(
        resolved_results,
        mir_results,
        "dual-canonical-mir",
    )))
}

/// Make a deferred dual request visibly fail closed without consuming the
/// retained source body. The primary engine may have produced a useful
/// observation, but the public dual contract cannot claim it without an
/// independent canonical secondary proof.
fn dual_verifier_coverage_results(
    program: &crate::core::CheckedProgram,
    primary: Vec<VerificationResult>,
    boundaries: &[crate::core::mir::verification::MirVerificationBoundary],
    source_hash: &str,
) -> Vec<VerificationResult> {
    let resolved_ir_hash = ctx::compute_resolved_ir_hash(program);
    let boundary_text = if boundaries.is_empty() {
        "canonical MIR secondary was not admitted".to_string()
    } else {
        boundaries
            .iter()
            .map(|boundary| {
                let point = boundary
                    .point
                    .as_ref()
                    .map(|point| format!("@{}", point.0))
                    .unwrap_or_default();
                format!("{}{}: {}", boundary.owner.0, point, boundary.reason)
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    let provenance = if source_hash.is_empty() {
        format!("resolved_ir_hash={resolved_ir_hash}")
    } else {
        format!("source_hash={source_hash}, resolved_ir_hash={resolved_ir_hash}")
    };
    let message = format!(
        "{}: dual verifier has no checker-owned canonical MIR secondary ({boundary_text}; {provenance})",
        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
    );
    let mut represented_owners = primary
        .iter()
        .map(|result| result.func_name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut coverage_results = primary
        .into_iter()
        .map(|mut result| {
            if matches!(
                result.status,
                VerifStatus::NoObligations | VerifStatus::Disproven
            ) {
                return result;
            }
            let span = result
                .diagnostic
                .as_ref()
                .map(|diagnostic| diagnostic.span)
                .unwrap_or_else(|| program.entry_span().unwrap_or(crate::span::Span::UNKNOWN));
            result.status = VerifStatus::NotInTrustedSubset;
            result.message = message.clone();
            result.diagnostic = Some(crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                message.clone(),
                span,
            ));
            result.constraint_count = 0;
            result.artifact = None;
            result.trusted_subset_domain = Some(TrustedSubsetDomain::Contract);
            result
        })
        .collect::<Vec<_>>();

    // A symmetric peer check can find proof work that exists only in the MIR
    // batch. Keep that owner visible in the public result set instead of
    // mentioning it only in another owner's message (or dropping it when the
    // Resolved batch is empty).
    for boundary in boundaries {
        if !represented_owners.insert(boundary.owner.0.clone()) {
            continue;
        }
        let span = boundary
            .point
            .as_ref()
            .and_then(|point| program.node_meta().get(point))
            .map(|meta| meta.origin.user_span())
            .unwrap_or_else(|| program.entry_span().unwrap_or(crate::span::Span::UNKNOWN));
        coverage_results.push(VerificationResult {
            func_name: boundary.owner.0.clone(),
            status: VerifStatus::NotInTrustedSubset,
            message: message.clone(),
            diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                message.clone(),
                span,
            )),
            duration_us: 0,
            constraint_count: 0,
            artifact: None,
            trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
        });
    }

    coverage_results
}

/// Try the already-closed verifier profile matrix from canonical MIR.
///
/// The public checked verifier is a program-level API. Once checker-owned
/// eligibility has established one of the exact closed profiles, its old
/// AST/Flow engine is no longer a valid consumer for this request. The profile
/// matrix below is owned by the shared MIR route module; construction,
/// materialized operation coverage, MIR capability, and the MIR verifier are
/// one hard-fail-closed chain. Programs outside every exact profile return
/// `None` so the caller can issue a checker-owned coverage result for the
/// unmigrated feature; they never re-enter a retained surface-body verifier.
fn verify_closed_mir_program(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Option<Vec<VerificationResult>>, String> {
    // Match default route precedence: a defined exported C body has no
    // canonical export-wrapper ABI receipt, so the entire request belongs to
    // the compatibility verifier even if another subgraph resembles a closed
    // MIR profile. Do not let a later verifier-side island probe turn that
    // explicit compatibility route into a construction failure.
    if crate::core::mir::exported_abi_body_boundary_reason(program).is_some()
        || crate::core::mir::scalar_actor_contract_boundary_reason(program).is_some()
    {
        return Ok(None);
    }
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if admission
        .ordinary_map_owner_lifecycle_observer_v1
        .has_profile_signal()
        && !admission.ordinary_map_owner_lifecycle_observer_v1_complete()
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map lifecycle observer-v1 attempt without complete independent observer receipt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map/Any owner-v2 candidate but withheld complete operation-attempt and result-receipt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission
        .ordinary_map_owned_get_i32_string_v1
        .has_profile_signal()
        && !admission.ordinary_map_owned_get_i32_string_v1_complete()
    {
        // This is a new typed API with no AST compatibility verifier. Keep
        // pre-existing untyped Map compatibility separate and fail closed for
        // a typed-get candidate whose independent receipt is incomplete.
        return Err(format!(
            "{}: Checker recognized an ordinary Map owned-get attempt without complete consumer coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    let ordinary_map_v1_exception = admission.map_any_owner_complete()
        && program
            .resource_analyses()
            .values()
            .filter(|analysis| analysis.has_ordinary_map_owner_lifecycle_v1_attempts())
            .all(|analysis| analysis.ordinary_map_attempts_fully_covered_by_map_any_owner_v1());
    if admission
        .ordinary_map_owner_lifecycle_i32_string_v1
        .has_profile_signal()
        && !admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
        && !ordinary_map_v1_exception
    {
        // Default verify follows the same explicit compatibility disposition
        // as run/build. Partial ordinary Map attempts do not become a
        // canonical profile until Get/Values/escape shapes have receipts.
        return Ok(None);
    }

    const PROFILES: [crate::core::mir::CanonicalMirRouteProfile; 47] = [
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CloneIsolation,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2PersistentRootAliasReplacement,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrintResult,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrint,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ZeroMissI32,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RemovedKeyCheckedMiss,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RecordUnit,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControl,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControlResult,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedKeyGet,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChain,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DynamicKeysGet,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CheckedGet,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesFieldMove,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesSnapshotReplacement,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshot,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshotRemove,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleObserverV1,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2,
        crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1,
        crate::core::mir::CanonicalMirRouteProfile::MapRoot,
        crate::core::mir::CanonicalMirRouteProfile::ScalarActor,
        crate::core::mir::CanonicalMirRouteProfile::ScalarFfi,
        crate::core::mir::CanonicalMirRouteProfile::ScalarNested,
        crate::core::mir::CanonicalMirRouteProfile::ScalarGenericIdentityI32,
        crate::core::mir::CanonicalMirRouteProfile::ScalarGenericIdentityI64,
        crate::core::mir::CanonicalMirRouteProfile::ScalarCollection,
        crate::core::mir::CanonicalMirRouteProfile::FlatCopyRecord,
        crate::core::mir::CanonicalMirRouteProfile::S8FlowTransition,
        crate::core::mir::CanonicalMirRouteProfile::FlowFailureRetry,
        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionStringVariant,
        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionNestedTupleVariant,
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionPredicate,
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionProjection,
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionProjectionFallback,
        crate::core::mir::CanonicalMirRouteProfile::GenericResultProjection,
        crate::core::mir::CanonicalMirRouteProfile::GenericResultProjectionFallback,
        crate::core::mir::CanonicalMirRouteProfile::ManagedResultCall,
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI32Variant,
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionBoolVariant,
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI64Variant,
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionF64Variant,
        crate::core::mir::CanonicalMirRouteProfile::CopyResultI32Variant,
        crate::core::mir::CanonicalMirRouteProfile::SessionChannel,
    ];
    for profile in PROFILES {
        if let Some(results) = verify_closed_mir_profile(program, profile, source_hash.clone())? {
            return Ok(Some(results));
        }
    }
    Ok(None)
}

/// Run the staged V2 consumer through canonical MIR for diagnostic evidence.
/// Its owner-scoped coverage NITS are appended by `verify_closed_mir_profile`;
/// callers must not fall through to a compatibility proof engine.
fn verify_staged_ordinary_map_owned_any_v2(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_program(program, source_hash)?.ok_or_else(|| {
        format!(
            "{}: staged ordinary Map/Any owner-v2 consumer did not materialize a verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the independently admitted CheckedGet slice. Its narrow operation
/// receipt and consumer checks are not interchangeable with broad V2
/// CompleteCoverage or the shared V2 production-readiness gate.
fn verify_checked_ordinary_map_owned_any_v2_get(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CheckedGet,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker CheckedGet admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the exact MapRoot → direct GetAny → checked String-unpack profile.
/// This profile uses a narrow Checker-aware verifier entry and remains
/// independent of the broad owner-v2 production-readiness gate.
fn verify_checked_ordinary_map_owned_any_v2_map_root_get_any_caller(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let route =
        crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
            let code = error
                .diagnostic_code()
                .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
            format!("{code}: Map-root GetAny-caller verifier route failed: {error}")
        })?;
    let receipt = route
        .ordinary_map_owned_any_v2_map_root_get_any_caller_route_receipt
        .ok_or_else(|| {
            format!(
                "{}: Checker Map-root GetAny-caller admission did not materialize its exact verifier profile",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    verify_mir_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
        program,
        &route.program,
        source_hash,
        &receipt,
    )
}

/// Verify the narrow Checker-sealed computed-key Get slice. Its operation
/// receipt remains independent of broad owner-v2 production readiness.
fn verify_checked_ordinary_map_owned_any_v2_computed_key_get(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedKeyGet,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker computed-key Get admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the source-qualified computed String-key owner chain through its
/// Checker-aware route and every backend capability gate.
fn verify_checked_ordinary_map_owned_any_v2_computed_string_key_owner_chain(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChain,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker ComputedStringKeyOwnerChain admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the exact Checker-sealed DirectPrint slice. Its profile receipt is
/// independent of broad V2 readiness and is validated by each consumer.
fn verify_checked_ordinary_map_owned_any_v2_direct_print(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrint,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker DirectPrint admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the nested Result DirectPrint profile through its Checker-bound
/// neutral L2 receipt. This remains available without Z3 because it returns
/// `NoObligations`, never a proof of Result tags or stdout semantics.
fn verify_checked_ordinary_map_owned_any_v2_direct_print_result(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrintResult,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker DirectPrintResult admission did not materialize its exact neutral L2 route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the exact three-path MapSetDropControl Result route through its
/// Checker-bound neutral consumer. This branch does not consult Z3: it can
/// return only a route-identified `NoObligations` observation.
fn verify_checked_ordinary_map_owned_any_v2_map_set_drop_control_result(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControlResult,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker MapSetDropControlResult admission did not materialize its exact neutral verifier route",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the independently admitted Values field-move slice. Its exact
/// Checker/MIR field-move receipt is separate from CheckedGet and broad V2
/// readiness, so it remains usable while the broad production gate is closed.
fn verify_checked_ordinary_map_owned_any_v2_values_field_move(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesFieldMove,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker Values field-move admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the narrow Values route whose index comes from one immutable local
/// initialized to i32 zero. The exact source/MIR receipt is checked at the
/// closed-profile boundary; this profile remains independent of broad V2
/// production readiness.
fn verify_checked_ordinary_map_owned_any_v2_values_local_index_i32(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker Values local-index-i32 admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the nested fallible New/Set route independently from the straight
/// line local-index profile. E0800 allocation paths remain ordinary MIR trap
/// obligations and are expected to make a complete callable proof Disproven.
fn verify_checked_ordinary_map_owned_any_v2_values_local_index_i32_result_branch(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranch,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker Values local-index-i32 Result-branch admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the sorted heterogeneous Values slice only after the exact
/// Checker/MIR receipt has been materialized. The source-specific verifier
/// also preflights bytecode, native codegen, and whole-program capability.
fn verify_checked_ordinary_map_owned_any_v2_values_heterogeneous_string_index1(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let route =
        crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
            let code = error
                .diagnostic_code()
                .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
            format!("{code}: Values heterogeneous String index-one verifier route failed: {error}")
        })?;
    let receipt = route
        .ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route_receipt
        .ok_or_else(|| {
            format!(
                "{}: Checker Values heterogeneous String index-one admission did not materialize its exact verifier receipt",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    verify_mir_with_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
        program,
        &route.program,
        &receipt,
        source_hash,
    )
}

/// Verify the independently admitted Keys snapshot profile. It remains
/// separate from broad owner-v2 readiness and requires its own source/MIR
/// receipt plus bytecode, verifier, and native consumer checks.
fn verify_checked_ordinary_map_owned_any_v2_keys_snapshot(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2KeysSnapshot,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker Keys snapshot admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the exact Checker-sealed argv[0] dynamic-key snapshot profile.
/// Its source/MIR receipt is required before any generic verifier path can
/// observe the argv projection or String output.
fn verify_checked_ordinary_map_owned_any_v2_dynamic_keys_get(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let route =
        crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
            let code = error
                .diagnostic_code()
                .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
            format!("{code}: DynamicKeysGet verifier route failed: {error}")
        })?;
    let receipt = route
        .ordinary_map_owned_any_v2_dynamic_keys_get_route_receipt
        .ok_or_else(|| {
            format!(
                "{}: Checker DynamicKeysGet admission did not materialize its exact verifier profile",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;
    verify_mir_with_ordinary_map_owned_any_v2_dynamic_keys_get_route(
        program,
        &route.program,
        &receipt,
        source_hash,
    )
}

/// Verify the exact Checker-sealed Values snapshot-replacement slice. Its
/// post-snapshot persistent Set, sorted-index reads, and owner settlements
/// are independent of broad owner-v2 readiness.
fn verify_checked_ordinary_map_owned_any_v2_values_snapshot_replacement(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesSnapshotReplacement,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker Values snapshot-replacement admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the exact Checker-sealed heterogeneous Values snapshot slice. Its
/// mixed i32/String/Label payload receipts are independent of broad owner-v2
/// readiness and are checked by both execution consumers before the
/// no-contract proof result can be accepted.
fn verify_checked_ordinary_map_owned_any_v2_values_mixed_snapshot(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshot,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker Values mixed-snapshot admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the exact Checker-sealed Values snapshot followed by persistent
/// removal slice. Its mixed payload unpacks and the post-snapshot Remove are
/// independent of broad owner-v2 readiness.
fn verify_checked_map_empty_root_root_queries_v1(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::MapEmptyRootRootQueriesV1,
        source_hash,
    )?
    .ok_or_else(|| {
        "MIR-COVERAGE-001: missing exact MapEmptyRootRootQueriesV1 verifier route".into()
    })
}

/// Verify the narrow Checker-sealed owner-v2 IsEmpty slice. This profile
/// remains independent from generic V2 CompleteCoverage and production
/// readiness, and its exact source/MIR receipt is consumed by each verifier
/// and capability boundary.
fn verify_checked_ordinary_map_owned_any_v2_is_empty(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker IsEmpty admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the exact mixed i32/Label/i32 RecordUnit slice through the closed
/// Checker-sealed MIR verifier route.
fn verify_checked_ordinary_map_owned_any_v2_record_unit(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RecordUnit,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker RecordUnit admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the no-query MapSetDropControl comparison through its independently
/// checked route. The profile remains outside broad owner-v2 readiness.
fn verify_checked_ordinary_map_owned_any_v2_map_set_drop_control(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControl,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker MapSetDropControl admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the narrow arm-local persistent Set diamond from its dedicated
/// Checker route. It remains separate from broad owner-v2 readiness.
fn verify_checked_ordinary_map_owned_any_v2_branch_local_set_diamond(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker BranchLocalSetDiamond admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the narrow Checker-sealed zero-miss i32 route independently from
/// broad owner-v2 readiness. The materialized route and exact receipt are
/// consumed by the same closed-profile verifier path as IsEmpty and
/// CheckedGet.
fn verify_checked_ordinary_map_owned_any_v2_zero_miss_i32(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ZeroMissI32,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker ZeroMissI32 admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the fused GetOrDefault<i32> operation through its exact
/// CheckedProgram-bound route. Its admission stays separate from broad V2
/// readiness and from raw MIR receipt authorization.
fn verify_checked_ordinary_map_owned_any_v2_get_or_default_i32(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let route =
        crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
            let code = error
                .diagnostic_code()
                .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
            format!("{code}: GetOrDefault<i32> verifier route failed: {error}")
        })?;
    let receipt = route
        .program
        .route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_I32_PROFILE);
    verify_mir_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
        program,
        &route.program,
        &receipt,
        source_hash,
    )
}

/// Verify the fused GetOrDefault<String> operation through its exact
/// CheckedProgram-bound MIR route. Raw MIR and the Legacy verifier do not
/// authorize this payload-specific Any cast.
fn verify_checked_ordinary_map_owned_any_v2_get_or_default_string(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let route =
        crate::core::mir::materialize_canonical_mir_route(program, None).map_err(|error| {
            let code = error
                .diagnostic_code()
                .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
            format!("{code}: GetOrDefault<String> verifier route failed: {error}")
        })?;
    let receipt = route
        .program
        .route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE);
    verify_mir_with_ordinary_map_owned_any_v2_get_or_default_string_route(
        program,
        &route.program,
        &receipt,
        source_hash,
    )
}

/// Verify the exact same-key persistent Remove followed by a checked i32
/// miss from that Remove successor. This has its own Checker receipt and
/// keeps the generic ZeroMissI32 route credentials disjoint.
fn verify_checked_ordinary_map_owned_any_v2_removed_key_checked_miss(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RemovedKeyCheckedMiss,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker RemovedKeyCheckedMiss admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the exact Checker-sealed CloneIsolation slice. The dedicated route
/// proves that the cloned root is independent of its source while the shared
/// owner-v2 production-readiness gate remains closed.
fn verify_checked_ordinary_map_owned_any_v2_clone_isolation(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CloneIsolation,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker CloneIsolation admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

/// Verify the exact Checker-sealed PersistentRootAliasReplacement route
/// through its own profile consumer preflight and source-aware verifier.
fn verify_checked_ordinary_map_owned_any_v2_persistent_root_alias_replacement(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2PersistentRootAliasReplacement,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker PersistentRootAliasReplacement admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

fn verify_checked_ordinary_map_owned_any_v2_values_mixed_snapshot_remove(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshotRemove,
        source_hash,
    )?
    .ok_or_else(|| {
        format!(
            "{}: Checker Values mixed-snapshot-remove admission did not materialize its exact verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

pub(crate) fn verify_scalar_actor_checked(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Option<Vec<VerificationResult>>, String> {
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if !admission.scalar_actor {
        return Ok(None);
    }
    validate_public_verifier_mir_boundary(program)?;
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::ScalarActor,
        source_hash,
    )
}

/// Verify one already-closed profile from the shared canonical route.
///
/// This function is the verifier-side consumer adapter only. Admission and
/// materialization are checker-owned by `core::mir::route`; profile-specific
/// TypeDesc/island validators remain explicit here because they are the
/// verifier's final consumer gate.
fn verify_closed_mir_profile(
    program: &crate::core::CheckedProgram,
    profile: crate::core::mir::CanonicalMirRouteProfile,
    source_hash: String,
) -> Result<Option<Vec<VerificationResult>>, String> {
    let Some(canonical) = materialize_closed_mir_island(program, profile)? else {
        return Ok(None);
    };
    if profile == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CheckedGet {
        let receipt = canonical.route_receipt(profile.as_str());
        return verify_mir_with_ordinary_map_owned_any_v2_checked_get_route(
            program,
            &canonical,
            &receipt,
            source_hash,
        )
        .map(Some);
    }
    match profile {
        crate::core::mir::CanonicalMirRouteProfile::ScalarActor => {
            crate::core::mir::validate_scalar_actor_island(&canonical).map_err(|errors| {
                format!("MIR-CAPABILITY-001: Actor receipt rejected: {errors:?}")
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(&canonical, &receipt)
                .map_err(|errors| {
                format!("MIR-CAPABILITY-001: Actor bytecode consumer rejected: {errors:?}")
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!("MIR-CAPABILITY-001: Actor native consumer rejected: {errors:?}")
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1 => {
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map owned-get bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map owned-get native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetAnyTupleL2Verifier => {
            return Err(format!(
                "{}: VerifierOnly legacy tuple GetAny L2 requires its CheckedProgram-aware theorem route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapDirectTupleGetUnpackL2Verifier => {
            return Err(format!(
                "{}: direct tuple Get L2 requires its CheckedProgram-aware semantic-projection verifier API",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1 => {
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map owner lifecycle bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map owner lifecycle native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleObserverV1 => {
            crate::core::mir::validate_ordinary_map_owner_lifecycle_observer_v1_mir_route(
                &canonical,
            )
            .map_err(|error| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map lifecycle observer-v1 receipt projection: {error}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map lifecycle observer-v1 bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map lifecycle observer-v1 native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1 => {
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the MapAnyOwner v1 bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the MapAnyOwner v1 native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::MapRoot => {
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the MapRoot bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the MapRoot native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2 => {
            crate::verifier::validate_mir_capabilities(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map/Any owner-v2 capability: {errors:?}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map/Any owner-v2 bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map/Any owner-v2 native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2GetOrDefaultI32 => {
            return Err(format!(
                "{}: GetOrDefault<i32> requires its exact CheckedProgram-aware verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2GetOrDefaultString => {
            return Err(format!(
                "{}: GetOrDefault<String> requires its exact CheckedProgram-aware verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrint => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_direct_print_mir_route(
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the DirectPrint operation receipt: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the DirectPrint bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_direct_print",
            );
            generator
                .compile_mir_native_with_route_receipt(&canonical, &receipt)
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the DirectPrint native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrintResult => {
            let receipt = canonical.route_receipt(
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_RESULT_L2_VERIFIER_PROFILE,
            );
            return verify_mir_with_ordinary_map_owned_any_v2_direct_print_result_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )
            .map(Some);
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControlResult => {
            let receipt = canonical.route_receipt(
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_RESULT_BRANCH_PROFILE,
            );
            return verify_mir_with_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )
            .map(Some);
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CheckedGet => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_checked_get_route(&canonical)
                .map_err(|message| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the CheckedGet operation receipt: {message}"
                    )
                })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the CheckedGet bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the CheckedGet native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedKeyGet => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_computed_key_get_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the computed-key Get operation receipt: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_computed_key_get_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the computed-key Get bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_computed_key_get",
            );
            generator
                .compile_checked_mir_native_with_computed_key_get_route(
                    program,
                    &canonical,
                    &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the computed-key Get native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChain => {
            let receipt = canonical.route_receipt(profile.as_str());
            return verify_mir_with_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )
            .map(Some);
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DynamicKeysGet => {
            // This source-qualified route is preflighted by the dedicated
            // CheckedProgram-aware verifier entry below. The raw receipt
            // path deliberately cannot authorize its argv projection.
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesFieldMove => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_field_move_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the Values field-move operation receipt: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_values_field_move_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the Values field-move bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_values_field_move",
            );
            generator
                .compile_checked_mir_native_with_route_receipt(program, &canonical, &receipt)
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Values field-move native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32 => {
            let expected =
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_route(
                    program,
                    &canonical,
                )
                .map_err(|message| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Values local-index-i32 Checker/MIR operation receipt: {message}"
                    )
                })?;
            let receipt = canonical.route_receipt(profile.as_str());
            if expected != receipt {
                return Err("MIR-CAPABILITY-001: Values local-index-i32 route receipt differs from the Checker-bound route".to_string());
            }
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_mir_route(
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the Values local-index-i32 MIR shape: {message}"
                )
            })?;
            crate::interp::bytecode::mir::compile_mir_program_with_values_local_index_i32_route(
                program, &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the Values local-index-i32 bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_values_local_index_i32",
            );
            generator
                .compile_checked_mir_native_with_values_local_index_i32_route(
                    program, &canonical, &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Values local-index-i32 native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranch => {
            let receipt = canonical.route_receipt(profile.as_str());
            return verify_mir_with_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )
            .map(Some);
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesHeterogeneousStringIndex1 => {
            let expected =
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
                    program,
                    &canonical,
                )
                .map_err(|message| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the heterogeneous Values String Checker/MIR receipt: {message}"
                    )
                })?;
            let receipt = canonical.route_receipt(profile.as_str());
            if expected != receipt {
                return Err(
                    "MIR-CAPABILITY-001: heterogeneous Values String route receipt differs from the Checker-bound route".into(),
                );
            }
            crate::interp::bytecode::compile_mir_program_with_values_heterogeneous_string_index1_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the heterogeneous Values String bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_values_heterogeneous_string_index1",
            );
            generator
                .compile_checked_mir_native_with_values_heterogeneous_string_index1_route(
                    program,
                    &canonical,
                    &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the heterogeneous Values String native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2KeysSnapshot => {
            // The complete source-bound verifier path runs immediately after
            // profile classification so bytecode is compiled before capability
            // admission and the raw-receipt verifier cannot authorize Keys.
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesSnapshotReplacement => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_snapshot_replacement_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the Values snapshot-replacement operation receipt: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_values_snapshot_replacement_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the Values snapshot-replacement bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_values_snapshot_replacement",
            );
            generator
                .compile_checked_mir_native_with_values_snapshot_replacement_route(
                    program,
                    &canonical,
                    &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Values snapshot-replacement native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshot => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the Values mixed-snapshot operation receipt: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_values_mixed_snapshot_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the Values mixed-snapshot bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_values_mixed_snapshot",
            );
            generator
                .compile_checked_mir_native_with_values_mixed_snapshot_route(
                    program,
                    &canonical,
                    &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Values mixed-snapshot native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::MapEmptyRootRootQueriesV1 => {
            crate::core::mir::validate_map_empty_root_root_queries_v1_route(program, &canonical)?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_empty_root_root_queries_v1_route(program, &canonical).map_err(|e| format!("MIR-COVERAGE-001: MapEmptyRootRootQueriesV1 bytecode: {e:?}"))?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(&context, "verifier_empty_root_root_queries_v1");
            generator.compile_checked_mir_native_with_empty_root_root_queries_v1_route(program, &canonical, &receipt).map_err(|e| format!("MIR-COVERAGE-001: MapEmptyRootRootQueriesV1 native: {e:?}"))?;
        }
        crate::core::mir::CanonicalMirRouteProfile::ScalarFfi => {}
        crate::core::mir::CanonicalMirRouteProfile::ScalarExport => {
            if canonical.export_declarations().is_empty() {
                return Err(
                    "MIR-CAPABILITY-001: scalar export route has no export declaration receipt"
                        .into(),
                );
            }
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the scalar export bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the scalar export native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshotRemove => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the Values mixed-snapshot-remove operation receipt: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_values_mixed_snapshot_remove_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the Values mixed-snapshot-remove bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_values_mixed_snapshot_remove",
            );
            generator
                .compile_checked_mir_native_with_values_mixed_snapshot_remove_route(
                    program,
                    &canonical,
                    &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Values mixed-snapshot-remove native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RecordUnit => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_record_unit_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the RecordUnit Checker/MIR operation receipt: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_record_unit_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the RecordUnit bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_record_unit",
            );
            generator
                .compile_checked_mir_native_with_record_unit_route(
                    program,
                    &canonical,
                    &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the RecordUnit native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_is_empty_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the IsEmpty Checker/MIR operation receipt: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_is_empty_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the IsEmpty bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_is_empty",
            );
            generator
                .compile_checked_mir_native_with_is_empty_route(
                    program,
                    &canonical,
                    &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the IsEmpty native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControl => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_map_set_drop_control_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the MapSetDropControl Checker/MIR operation receipt: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_map_set_drop_control_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the MapSetDropControl bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_map_set_drop_control",
            );
            generator
                .compile_checked_mir_native_with_map_set_drop_control_route(
                    program,
                    &canonical,
                    &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the MapSetDropControl native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ZeroMissI32 => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ZeroMissI32 Checker/MIR operation receipt: {message}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RemovedKeyCheckedMiss => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the RemovedKeyCheckedMiss Checker/MIR operation receipt: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the RemovedKeyCheckedMiss bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_removed_key_checked_miss",
            );
            generator
                .compile_checked_mir_native_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
                    program,
                    &canonical,
                    &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the RemovedKeyCheckedMiss native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the BranchLocalSetDiamond Checker/MIR operation receipt: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the BranchLocalSetDiamond bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_branch_local_set_diamond",
            );
            generator
                .compile_checked_mir_native_with_branch_local_set_diamond_route(
                    program,
                    &canonical,
                    &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the BranchLocalSetDiamond native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CloneIsolation => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_clone_isolation_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the CloneIsolation Checker/MIR operation receipt: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_clone_isolation_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the CloneIsolation bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_clone_isolation",
            );
            generator
                .compile_checked_mir_native_with_clone_isolation_route(
                    program,
                    &canonical,
                    &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the CloneIsolation native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2PersistentRootAliasReplacement => {
            crate::core::mir::validate_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
                program,
                &canonical,
            )
            .map_err(|message| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the PersistentRootAliasReplacement Checker/MIR route: {message}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
                program,
                &canonical,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the PersistentRootAliasReplacement bytecode consumer: {errors:?}"
                )
            })?;
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                "verifier_ordinary_map_owned_any_v2_persistent_root_alias_replacement",
            );
            generator
                .compile_checked_mir_native_with_persistent_root_alias_replacement_route(
                    program,
                    &canonical,
                    &receipt,
                )
                .map_err(|errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the PersistentRootAliasReplacement native consumer: {errors:?}"
                    )
                })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::ScalarNested => {}
        crate::core::mir::CanonicalMirRouteProfile::ScalarGenericIdentityI32 => {
            crate::core::mir::validate_scalar_generic_identity_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the generic scalar identity island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::ScalarGenericIdentityI64 => {
            crate::core::mir::validate_scalar_generic_identity_i64_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the generic scalar identity i64 island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::ScalarCollection => {
            crate::core::mir::validate_scalar_collection_graph(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the scalar collection island: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::FlatCopyRecord
        | crate::core::mir::CanonicalMirRouteProfile::S8FlowTransition
        | crate::core::mir::CanonicalMirRouteProfile::FlowFailureRetry => {}
        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionStringVariant => {
            crate::core::mir::validate_option_string_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Option<string> variant island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionNestedTupleVariant => {
            crate::core::mir::validate_option_nested_tuple_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the nested Option tuple variant island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI32Variant => {
            crate::core::mir::validate_copy_option_i32_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Copy Option<i32> variant island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionBoolVariant => {
            crate::core::mir::validate_copy_option_variant_island(
                &canonical,
                crate::core::PrimitiveType::Bool,
                crate::core::mir::COPY_OPTION_BOOL_VARIANT_ISLAND,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the Copy Option<bool> variant island: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI64Variant => {
            crate::core::mir::validate_copy_option_i64_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Copy Option<i64> variant island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionF64Variant => {
            crate::core::mir::validate_copy_option_f64_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Copy Option<f64> variant island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::CopyResultI32Variant => {
            crate::core::mir::validate_copy_result_i32_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Copy Result<i32, i32> variant island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionPredicate => {}
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionProjection => {}
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionProjectionFallback => {}
        crate::core::mir::CanonicalMirRouteProfile::GenericResultProjection => {}
        crate::core::mir::CanonicalMirRouteProfile::GenericResultProjectionFallback => {}
        crate::core::mir::CanonicalMirRouteProfile::ManagedResultCall => {
            crate::core::mir::validate_managed_result_call_island(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the managed Result direct-call island: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::SessionChannel => {
            crate::core::mir::validate_session_channel_island(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the session-channel island: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesProducerCaller => {
            return Err(format!(
                "{}: Values producer-caller requires its exact Checker-aware verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCaller => {
            return Err(format!(
                "{}: Result Values cross-call String caller V3 requires its exact CheckedProgram-aware verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCallerIndex1 => {
            return Err(format!(
                "{}: Result Values cross-call String caller V4 requires its exact CheckedProgram-aware verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesResultProducerCaller => {
            return Err(format!(
                "{}: NotInTrustedSubset: Result Values producer/caller has no implemented Z3 theorem consumer",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller => {
            return Err(format!(
                "{}: Values cross-call String caller requires its exact Checker-aware verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1 => {
            return Err(format!(
                "{}: Values cross-call String caller index-one requires its exact V2 Checker-aware verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootProducerCaller => {
            return Err(format!(
                "{}: Map-root producer/caller requires its exact Checker-aware verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller => {
            return Err(format!(
                "{}: Map-root GetAny-caller requires its exact Checker-aware verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2SetGetAnyI32 => {
            return Err(format!(
                "{}: ordinary Map Set/GetAny-i32 requires its dedicated CheckedProgram-aware L2 verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2SetGetAnyResultI32 => {
            return Err(format!(
                "{}: Result-valued ordinary Map Set/GetAny-i32 requires its dedicated CheckedProgram-aware Result L2 verifier route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
    }
    let mut dynamic_keys_get_results = if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DynamicKeysGet
    {
        let receipt = canonical.route_receipt(profile.as_str());
        Some(
            verify_mir_with_ordinary_map_owned_any_v2_dynamic_keys_get_route(
                program,
                &canonical,
                &receipt,
                source_hash.clone(),
            )?,
        )
    } else {
        None
    };
    let mut keys_snapshot_results = if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2KeysSnapshot
    {
        let receipt = canonical.route_receipt(profile.as_str());
        Some(
            verify_mir_with_ordinary_map_owned_any_v2_keys_snapshot_route(
                program,
                &canonical,
                &receipt,
                source_hash.clone(),
            )?,
        )
    } else {
        None
    };
    let capability_result = if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DynamicKeysGet
        || profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2KeysSnapshot
    {
        // The CheckedProgram-aware route verifier already preflighted each
        // consumer and produced the symbolic result batch above.
        Ok(())
    } else if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrint
    {
        let receipt = canonical.route_receipt(profile.as_str());
        crate::verifier::validate_mir_capabilities_with_direct_print_route_receipt(
            &canonical, &receipt,
        )
    } else if profile == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty {
        let receipt = canonical.route_receipt(profile.as_str());
        crate::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_is_empty_route(
            program, &canonical, &receipt,
        )
    } else if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RecordUnit
    {
        let receipt = canonical.route_receipt(profile.as_str());
        crate::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_record_unit_route(
            program, &canonical, &receipt,
        )
    } else if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32
    {
        let receipt = canonical.route_receipt(profile.as_str());
        crate::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_local_index_i32_route(
            program,
            &canonical,
            &receipt,
        )
    } else if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesHeterogeneousStringIndex1
    {
        let receipt = canonical.route_receipt(profile.as_str());
        crate::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
            program,
            &canonical,
            &receipt,
        )
    } else if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond
    {
        let receipt = canonical.route_receipt(profile.as_str());
        crate::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
            program,
            &canonical,
            &receipt,
        )
    } else if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ZeroMissI32
    {
        let receipt = canonical.route_receipt(profile.as_str());
        crate::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
            program,
            &canonical,
            &receipt,
        )
    } else if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RemovedKeyCheckedMiss
    {
        let receipt = canonical.route_receipt(profile.as_str());
        crate::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
            program,
            &canonical,
            &receipt,
        )
    } else if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller
    {
        let receipt = canonical.route_receipt(profile.as_str());
        crate::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
            program,
            &canonical,
            &receipt,
        )
    } else {
        crate::verifier::validate_mir_capabilities(&canonical)
    };
    capability_result.map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: canonical verifier TypeDesc/capability gate rejected the {}: {errors:?}",
            profile.as_str()
        )
    })?;
    let mut results = if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DynamicKeysGet
    {
        dynamic_keys_get_results.take().ok_or_else(|| {
            format!(
                "{}: DynamicKeysGet profile completed without its CheckedProgram-aware verifier result",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?
    } else if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2KeysSnapshot
    {
        keys_snapshot_results.take().ok_or_else(|| {
            format!(
                "{}: KeysSnapshot profile completed without its CheckedProgram-aware verifier result",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?
    } else if profile
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesHeterogeneousStringIndex1
    {
        let receipt = canonical.route_receipt(profile.as_str());
        verify_mir_with_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
            program,
            &canonical,
            &receipt,
            source_hash,
        )?
    } else if matches!(
        profile,
        crate::core::mir::CanonicalMirRouteProfile::MapRoot
            | crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1
            | crate::core::mir::CanonicalMirRouteProfile::ScalarActor
            | crate::core::mir::CanonicalMirRouteProfile::ScalarExport
            | crate::core::mir::CanonicalMirRouteProfile::ScalarNested
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleObserverV1
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CheckedGet
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesFieldMove
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesSnapshotReplacement
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshot
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshotRemove
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedKeyGet
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrint
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RecordUnit
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControl
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ZeroMissI32
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RemovedKeyCheckedMiss
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CloneIsolation
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2PersistentRootAliasReplacement
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller
            | crate::core::mir::CanonicalMirRouteProfile::MapEmptyRootRootQueriesV1
    ) {
        let receipt = canonical.route_receipt(profile.as_str());
        if profile == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RecordUnit {
            verify_mir_with_ordinary_map_owned_any_v2_record_unit_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )?
        } else if profile
            == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty
        {
            verify_mir_with_ordinary_map_owned_any_v2_is_empty_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )?
        } else if profile
            == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControl
        {
            verify_mir_with_ordinary_map_owned_any_v2_map_set_drop_control_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )?
        } else if profile
            == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond
        {
            verify_mir_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )?
        } else if profile
            == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CloneIsolation
        {
            verify_mir_with_ordinary_map_owned_any_v2_clone_isolation_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )?
        } else if profile
            == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2PersistentRootAliasReplacement
        {
            verify_mir_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )?
        } else if profile
            == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ZeroMissI32
        {
            verify_mir_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )?
        } else if profile
            == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RemovedKeyCheckedMiss
        {
            verify_mir_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )?
        } else if profile
            == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller
        {
            verify_mir_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
                program,
                &canonical,
                source_hash,
                &receipt,
            )?
        } else if profile
            == crate::core::mir::CanonicalMirRouteProfile::MapEmptyRootRootQueriesV1
        {
            verify_mir_with_map_empty_root_root_queries_v1_route(
                program,
                &canonical,
                &receipt,
                source_hash,
            )?
        } else {
            verify_mir_diagnostic_with_route_receipt(&canonical, &receipt, source_hash)?
        }
    } else {
        crate::verifier::verify_mir(&canonical, source_hash)?
    };
    // Public checked-verifier APIs historically expose source-level callable
    // names. Keep that display contract at the adapter boundary while proof
    // artifacts retain the canonical `function:` owner and MIR hash.
    for result in &mut results {
        if let Some(diagnostic) = result
            .diagnostic
            .as_mut()
            .filter(|diagnostic| diagnostic.span == crate::span::Span::UNKNOWN)
        {
            let subject = diagnostic
                .origin
                .as_ref()
                .and_then(|origin| origin.parent_node_id.as_ref());
            if let Some(meta) = subject
                .and_then(|subject| {
                    program
                        .node_meta()
                        .get(&crate::core::NodeId(subject.clone()))
                })
                .or_else(|| {
                    program
                        .node_meta()
                        .get(&crate::core::NodeId(result.func_name.clone()))
                })
            {
                diagnostic.span = meta.origin.user_span();
            }
        }
        if let Some(name) = result.func_name.strip_prefix("function:") {
            result.func_name = name.to_string();
        }
    }
    if profile == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2
        && !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY
    {
        results = enforce_ordinary_map_owned_any_v2_coverage(program, results);
    }
    Ok(Some(results))
}

/// Materialize one of the already-closed verifier islands through the shared
/// route envelope.  This is deliberately profile-driven: a verifier helper
/// must not grow a private `is_exact -> construct -> contains_*` policy when a
/// new consumer route is added.  Checker admission is read before construction
/// so non-target profiles do not pay for or accidentally trigger materialization.
fn materialize_closed_mir_island(
    program: &crate::core::CheckedProgram,
    island: crate::core::mir::CanonicalMirRouteProfile,
) -> Result<Option<crate::core::mir::reference::MirProgram>, String> {
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    let clone_isolation_profile =
        island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CloneIsolation;
    let clone_isolation_admission = admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation;
    if clone_isolation_profile != clone_isolation_admission {
        return Ok(None);
    }
    if clone_isolation_profile && !island.is_admitted(admission) {
        return Ok(None);
    }
    let persistent_root_alias_replacement_profile = island
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2PersistentRootAliasReplacement;
    let persistent_root_alias_replacement_admission = admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::PersistentRootAliasReplacement;
    if persistent_root_alias_replacement_profile != persistent_root_alias_replacement_admission {
        return Ok(None);
    }
    if persistent_root_alias_replacement_profile && !island.is_admitted(admission) {
        return Ok(None);
    }
    let zero_miss_i32_profile =
        island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ZeroMissI32;
    let zero_miss_i32_admission = admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ZeroMissI32;
    if zero_miss_i32_profile != zero_miss_i32_admission {
        return Ok(None);
    }
    if zero_miss_i32_profile && !island.is_admitted(admission) {
        return Ok(None);
    }
    let removed_key_checked_miss_profile = island
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RemovedKeyCheckedMiss;
    let removed_key_checked_miss_admission = admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RemovedKeyCheckedMiss;
    if removed_key_checked_miss_profile != removed_key_checked_miss_admission {
        return Ok(None);
    }
    if removed_key_checked_miss_profile && !island.is_admitted(admission) {
        return Ok(None);
    }
    let branch_local_set_diamond_profile = island
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond;
    let branch_local_set_diamond_admission =
        admission.ordinary_map_owned_any_v2_branch_local_set_diamond();
    if branch_local_set_diamond_profile != branch_local_set_diamond_admission {
        return Ok(None);
    }
    if branch_local_set_diamond_profile && !island.is_admitted(admission) {
        return Ok(None);
    }
    if island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map/Any owner-v2 candidate but withheld complete operation-attempt and result-receipt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    let v2_verifier_diagnostic = island
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2
        && (admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::StagedGetValues
            || (admission.ordinary_map_owned_any_v2
                == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage
                && !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY));
    let ordinary_map_v1_exception = admission.map_any_owner_complete()
        && program
            .resource_analyses()
            .values()
            .filter(|analysis| analysis.has_ordinary_map_owner_lifecycle_v1_attempts())
            .all(|analysis| analysis.ordinary_map_attempts_fully_covered_by_map_any_owner_v1());
    // A complete map-owner profile cannot coexist with a scalar FFI island.
    // Resolve that shared-route conflict before the no-Z3 mock boundary too:
    // otherwise `verify_ffi_checked` could skip MIR materialization and report
    // a compatibility/mock result for a program the run/build route rejects.
    let conflicting_map_owner_ffi_profiles = (admission.map_root_complete()
        || admission.map_any_owner_complete()
        || admission.ordinary_map_owned_any_v2_complete()
        || admission.ordinary_map_owned_get_i32_string_v1_complete()
        || admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
        || admission.ordinary_map_owner_lifecycle_observer_v1_complete())
        && admission.scalar_ffi;
    if island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1
        && admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
        && !admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
        && !ordinary_map_v1_exception
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map owner lifecycle attempt without complete operation-attempt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleObserverV1
        && admission
            .ordinary_map_owner_lifecycle_observer_v1
            .has_profile_signal()
        && !admission.ordinary_map_owner_lifecycle_observer_v1_complete()
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map lifecycle observer-v1 candidate without complete independent observer receipt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1
        && admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal()
        && !admission.ordinary_map_owned_get_i32_string_v1_complete()
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map owned-get attempt without complete independent operation-attempt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if island == crate::core::mir::CanonicalMirRouteProfile::MapRoot
        && admission.map_root.is_candidate()
        && !admission.map_root_complete()
    {
        return Err(format!(
            "{}: Checker recognized a MapRoot candidate but withheld its complete local lifecycle receipt",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if island == crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1
        && admission.map_any_owner.is_candidate()
        && !admission.map_any_owner_complete()
    {
        return Err(format!(
            "{}: Checker recognized a MapAnyOwner v1 candidate but withheld its complete independent lifecycle receipt",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if !is_z3_available()
        && !conflicting_map_owner_ffi_profiles
        && island != crate::core::mir::CanonicalMirRouteProfile::ScalarActor
        && island
            != crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrintResult
        && island
            != crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControlResult
    {
        if island
            == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1
            && admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
        {
            return Err(
                "MIR-VERIFY-001: ordinary Map owner lifecycle Checker receipt profile requires the MIR contract verifier; compatibility fallback is forbidden".into(),
            );
        }
        if island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleObserverV1
            && admission.ordinary_map_owner_lifecycle_observer_v1_complete()
        {
            return Err(
                "MIR-VERIFY-001: ordinary Map lifecycle observer-v1 Checker receipt profile requires the MIR contract verifier; compatibility fallback is forbidden".into(),
            );
        }
        if island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1
            && admission.ordinary_map_owned_get_i32_string_v1_complete()
        {
            return Err(
                "MIR-VERIFY-001: ordinary Map owned-get Checker receipt profile requires the MIR contract verifier; compatibility fallback is forbidden".into(),
            );
        }
        if branch_local_set_diamond_profile {
            return Err(
                "MIR-VERIFY-001: BranchLocalSetDiamond Checker receipt profile requires the MIR contract verifier; compatibility fallback is forbidden".into(),
            );
        }
        if island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2
            && admission.ordinary_map_owned_any_v2_complete()
        {
            return Err(
                "MIR-VERIFY-001: ordinary Map/Any owner-v2 Checker receipt profile requires the MIR contract verifier; compatibility fallback is forbidden".into(),
            );
        }
        if island == crate::core::mir::CanonicalMirRouteProfile::MapRoot
            && admission.map_root_complete()
        {
            return Err(
                "MIR-VERIFY-001: MapRoot Checker receipt profile requires the MIR contract verifier; compatibility fallback is forbidden".into(),
            );
        }
        if island == crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1
            && admission.map_any_owner_complete()
        {
            return Err(
                "MIR-VERIFY-001: MapAnyOwner v1 Checker receipt profile requires the MIR contract verifier; compatibility fallback is forbidden".into(),
            );
        }
        // Preserve the existing CheckedProgram mock infrastructure boundary;
        // without Z3 the public API does not claim a MIR proof.
        return Ok(None);
    }
    if !island.is_admitted(admission) && !v2_verifier_diagnostic {
        return Ok(None);
    }
    let materialized_route = if v2_verifier_diagnostic {
        crate::core::mir::materialize_ordinary_map_owned_any_v2_for_verifier_diagnostics(
            program, None,
        )
    } else {
        crate::core::mir::materialize_canonical_mir_route(program, None)
    };
    let route = match materialized_route {
        Ok(route) => route,
        Err(crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility { .. }) => {
            return Ok(None);
        }
        Err(error) => {
            let code = error
                .diagnostic_code()
                .expect("complete route materialization errors carry a diagnostic code");
            return Err(format!("{code}: {error}"));
        }
    };
    let nonproduction_v2_has_checker_actions = v2_verifier_diagnostic
        && route.program.functions().values().any(|function| {
            function
                .ownership
                .has_checker_ordinary_map_owned_any_v2_actions()
        });
    if !island.is_materialized(&route) && !nonproduction_v2_has_checker_actions {
        return Err(format!(
            "{}: {} admission did not materialize its canonical operation",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            island.as_str()
        ));
    }
    if clone_isolation_profile {
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_clone_isolation_route_receipt
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "{}: CloneIsolation admission did not produce its exact materialized route receipt",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
        let canonical_receipt = route
            .program
            .route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE);
        if route.admission.ordinary_map_owned_any_v2
            != crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation
            || !island.is_admitted(route.admission)
            || !route.materialized_ordinary_map_owned_any_v2_clone_isolation_candidate
            || materialized_receipt.profile
                != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE
            || materialized_receipt != &canonical_receipt
        {
            return Err(format!(
                "{}: CloneIsolation Checker admission, materialization, and route receipt are not bidirectionally bound",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
    }
    if persistent_root_alias_replacement_profile {
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_receipt
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "{}: PersistentRootAliasReplacement admission did not produce its exact materialized route receipt",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
        let canonical_receipt = route.program.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE,
        );
        if route.admission.ordinary_map_owned_any_v2
            != crate::core::mir::OrdinaryMapOwnedAnyV2Admission::PersistentRootAliasReplacement
            || !island.is_admitted(route.admission)
            || !route
                .materialized_ordinary_map_owned_any_v2_persistent_root_alias_replacement_candidate
            || materialized_receipt.profile
                != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE
            || materialized_receipt != &canonical_receipt
        {
            return Err(format!(
                "{}: PersistentRootAliasReplacement Checker admission, materialization, and route receipt are not bidirectionally bound",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
    }
    if zero_miss_i32_profile {
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_zero_miss_i32_route_receipt
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "{}: ZeroMissI32 admission did not produce its exact materialized route receipt",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
        let canonical_receipt = route
            .program
            .route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE);
        if route.admission.ordinary_map_owned_any_v2
            != crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ZeroMissI32
            || !island.is_admitted(route.admission)
            || !route.materialized_ordinary_map_owned_any_v2_zero_miss_i32_candidate
            || materialized_receipt.profile
                != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE
            || materialized_receipt != &canonical_receipt
        {
            return Err(format!(
                "{}: ZeroMissI32 Checker admission, materialization, and route receipt are not bidirectionally bound",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
    }
    if removed_key_checked_miss_profile {
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_removed_key_checked_miss_route_receipt
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "{}: RemovedKeyCheckedMiss admission did not produce its exact materialized route receipt",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
        let canonical_receipt = route.program.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE,
        );
        if route.admission.ordinary_map_owned_any_v2
            != crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RemovedKeyCheckedMiss
            || !island.is_admitted(route.admission)
            || !route.materialized_ordinary_map_owned_any_v2_removed_key_checked_miss_candidate
            || materialized_receipt.profile
                != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE
            || materialized_receipt != &canonical_receipt
        {
            return Err(format!(
                "{}: RemovedKeyCheckedMiss Checker admission, materialization, and route receipt are not bidirectionally bound",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
    }
    if branch_local_set_diamond_profile {
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_branch_local_set_diamond_route_receipt
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "{}: BranchLocalSetDiamond admission did not produce its exact materialized route receipt",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            })?;
        let canonical_receipt = route.program.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE,
        );
        if !route.materialized_ordinary_map_owned_any_v2_branch_local_set_diamond_candidate
            || !island.is_admitted(route.admission)
            || materialized_receipt.profile
                != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE
            || materialized_receipt != &canonical_receipt
        {
            return Err(format!(
                "{}: BranchLocalSetDiamond Checker admission, materialization, and route receipt are not bidirectionally bound",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
    }
    Ok(Some(route.program))
}

/// Verify the already-closed scalar collection island from canonical MIR.
#[cfg(test)]
fn verify_closed_scalar_collection_mir(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Option<Vec<VerificationResult>>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::ScalarCollection,
        source_hash,
    )
}

/// Verify the already-closed S8 silent-local Flow island from canonical MIR.
#[cfg(test)]
fn verify_closed_s8_flow_mir(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Option<Vec<VerificationResult>>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::S8FlowTransition,
        source_hash,
    )
}

/// Coarse verdict classes for divergence detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerdictClass {
    Proven,
    Disproven,
    Inconclusive,
    /// NoObligations / InfrastructureError — the engine attempted no proof;
    /// never counts as a divergence counterpart.
    NoOpinion,
}

fn verdict_class(status: &VerifStatus) -> VerdictClass {
    match status {
        VerifStatus::Proven => VerdictClass::Proven,
        VerifStatus::Disproven => VerdictClass::Disproven,
        VerifStatus::NoObligations | VerifStatus::InfrastructureError => VerdictClass::NoOpinion,
        _ => VerdictClass::Inconclusive,
    }
}

fn source_provenance_conflict(
    primary: &VerificationResult,
    secondary: &VerificationResult,
) -> bool {
    match (&primary.artifact, &secondary.artifact) {
        (Some(primary), Some(secondary)) => {
            !primary.source_hash.is_empty()
                && !secondary.source_hash.is_empty()
                && primary.source_hash != secondary.source_hash
        }
        _ => false,
    }
}

/// Return true when a merged batch carries proofs from more than one bound
/// source snapshot.  A per-function comparison is insufficient here because
/// the Flow engine can emit obligations (for example call-site obligations)
/// that have no same-named result in the resolved batch.  Those unmatched
/// results still become part of the merged proof observation and therefore
/// must obey the same single-snapshot provenance invariant.
fn batch_source_provenance_conflict(
    primary: &[VerificationResult],
    secondary: &[VerificationResult],
) -> bool {
    let mut source_hash: Option<&str> = None;
    for result in primary.iter().chain(secondary.iter()) {
        let Some(artifact) = result.artifact.as_ref() else {
            continue;
        };
        if artifact.source_hash.is_empty() {
            continue;
        }
        match source_hash {
            None => source_hash = Some(artifact.source_hash.as_str()),
            Some(existing) if existing != artifact.source_hash => return true,
            Some(_) => {}
        }
    }
    false
}

/// Convert a proof-bearing result into an explicit no-proof observation after
/// a batch-level provenance conflict.  Results without an artifact are left
/// untouched: they already carry no proof that could cross a source boundary.
fn discard_batch_provenance_artifact(mut result: VerificationResult) -> VerificationResult {
    if result.artifact.is_none() {
        return result;
    }
    let span = result
        .diagnostic
        .as_ref()
        .map(|diagnostic| diagnostic.span)
        .unwrap_or_else(|| crate::span::Span::new(1, 1, 1, 1));
    result.status = VerifStatus::InfrastructureError;
    result.trusted_subset_domain = None;
    result.constraint_count = 0;
    result.message = format!(
        "[{}] mixed source provenance mismatch in merged batch for '{}'; merged proof discarded",
        crate::diagnostic::codes::E0439,
        result.func_name
    );
    result.diagnostic = Some(crate::diagnostic::Diagnostic::error(
        format!(
            "{}: merged verification results for '{}' came from different source snapshots",
            crate::diagnostic::codes::E0439,
            result.func_name
        ),
        span,
    ));
    result.artifact = None;
    result
}

/// Merge per-function verdicts from the two engines (ADR-008 §3).
///
/// Rules:
/// - both agree (same class) → primary (resolved) result wins;
/// - one side is NoOpinion → the other side's result wins silently (no proof
///   was attempted on the silent side, so there is nothing to disagree with);
/// - both sides carry different non-empty source hashes → the merged result
///   is InfrastructureError with no artifact, even when verdict classes agree;
/// - classes disagree → fail-closed: Disproven beats Inconclusive beats
///   Proven, and the merged result carries the E0439 divergence diagnostic.
#[cfg(test)]
fn merge_engine_verdicts(
    primary: Vec<VerificationResult>,
    secondary: Vec<VerificationResult>,
) -> Vec<VerificationResult> {
    merge_engine_verdicts_with_secondary_label(primary, secondary, "flow_ast")
}

fn merge_engine_verdicts_with_secondary_label(
    primary: Vec<VerificationResult>,
    secondary: Vec<VerificationResult>,
    secondary_label: &str,
) -> Vec<VerificationResult> {
    // Normalize the inputs before matching names.  This closes the gap where
    // an unmatched Flow-only result could otherwise smuggle a proof from a
    // different source snapshot into the merged batch.
    let batch_source_conflict = batch_source_provenance_conflict(&primary, &secondary);
    let primary = if batch_source_conflict {
        primary
            .into_iter()
            .map(discard_batch_provenance_artifact)
            .collect()
    } else {
        primary
    };
    let secondary = if batch_source_conflict {
        secondary
            .into_iter()
            .map(discard_batch_provenance_artifact)
            .collect()
    } else {
        secondary
    };
    let mut merged: Vec<VerificationResult> = Vec::with_capacity(primary.len());
    let mut secondary_by_name: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (index, result) in secondary.iter().enumerate() {
        secondary_by_name
            .entry(result.func_name.clone())
            .or_insert(index);
    }
    let mut consumed: Vec<bool> = vec![false; secondary.len()];
    for mut result in primary {
        let Some(&sec_index) = secondary_by_name.get(&result.func_name) else {
            merged.push(result);
            continue;
        };
        consumed[sec_index] = true;
        let flow_result = &secondary[sec_index];
        let primary_class = verdict_class(&result.status);
        let flow_class = verdict_class(&flow_result.status);
        if source_provenance_conflict(&result, flow_result) {
            // A matching verdict is still unusable when the engines proved
            // different source snapshots. Drop the artifact and expose a
            // no-proof status instead of allowing agreement to hide the
            // provenance conflict.
            result.status = VerifStatus::InfrastructureError;
            result.trusted_subset_domain = None;
            result.constraint_count = 0;
            result.message = format!(
                "[{}] source provenance mismatch for '{}'; merged proof discarded",
                crate::diagnostic::codes::E0439,
                result.func_name
            );
            result.diagnostic = Some(crate::diagnostic::Diagnostic::error(
                format!(
                    "{}: verification results for '{}' came from different source snapshots",
                    crate::diagnostic::codes::E0439,
                    result.func_name
                ),
                result
                    .diagnostic
                    .as_ref()
                    .or(flow_result.diagnostic.as_ref())
                    .map(|diagnostic| diagnostic.span)
                    .unwrap_or_else(|| crate::span::Span::new(1, 1, 1, 1)),
            ));
            result.artifact = None;
            merged.push(result);
            continue;
        }
        if primary_class == VerdictClass::NoOpinion {
            // Resolved engine attempted no proof — take the flow verdict.
            merged.push(flow_result.clone());
            continue;
        }
        if flow_class == VerdictClass::NoOpinion || flow_class == primary_class {
            merged.push(result);
            continue;
        }
        // Divergence: fail-closed to the weaker conclusion.
        let weaker_is_flow = matches!(
            (primary_class, flow_class),
            (VerdictClass::Proven, VerdictClass::Disproven)
                | (VerdictClass::Proven, VerdictClass::Inconclusive)
                | (VerdictClass::Inconclusive, VerdictClass::Disproven)
        );
        let weaker = if weaker_is_flow {
            flow_result.clone()
        } else {
            result.clone()
        };
        result.status = weaker.status.clone();
        result.trusted_subset_domain = weaker.trusted_subset_domain;
        result.constraint_count = weaker.constraint_count;
        result.message = format!(
            "[{}] engine divergence for '{}': resolved={:?} vs {secondary_label}={:?}; \
             fail-closed to {:?} ({})",
            crate::diagnostic::codes::E0439,
            result.func_name,
            primary_class,
            flow_class,
            result.status,
            weaker.message
        );
        let divergence = crate::diagnostic::Diagnostic::error(
            format!(
                "{}: verification engines disagree on '{}' (resolved={:?}, \
                 {secondary_label}={:?}); the weaker conclusion wins",
                crate::diagnostic::codes::E0439,
                result.func_name,
                primary_class,
                flow_class
            ),
            weaker
                .diagnostic
                .as_ref()
                .or(result.diagnostic.as_ref())
                .map(|d| d.span)
                .unwrap_or_else(|| crate::span::Span::new(1, 1, 1, 1)),
        );
        result.diagnostic = Some(divergence);
        result.artifact = None; // a divergent verdict is no proof
        merged.push(result);
    }
    // Functions verified ONLY by the flow engine (e.g. call-site obligations
    // the resolved engine does not model yet) pass through untouched.
    for (index, result) in secondary.iter().enumerate() {
        if !consumed[index] {
            merged.push(result.clone());
        }
    }
    merged
}

/// Merge the exact Checker owner set in the Actor/Flow MIR projection with
/// its Resolved peers. Other Resolved roots remain in their original result
/// slots and are not turned into coverage failures merely because this slice
/// does not claim them. Stable owner IDs are used for pairing; display labels
/// are restored only after the pair has been merged.
fn merge_actor_flow_dual_results(
    mut resolved: Vec<VerificationResult>,
    batch: flow_mir::ActorFlowMirBatchV1,
) -> Result<Vec<VerificationResult>, String> {
    let flow_callsite_owners = actor_flow_callsite_owner_closure(&batch.projection);
    let mut owner_by_alias = std::collections::BTreeMap::<String, crate::core::NodeId>::new();
    for owner in &batch.projection.owner_scope {
        let name = batch.owner_names.get(owner).ok_or_else(|| {
            format!(
                "{}: Actor/Flow dual request omitted the result identity for Checker owner {}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                owner.0
            )
        })?;
        for alias in [name.clone(), owner.0.clone(), format!("{owner:?}")] {
            if owner_by_alias
                .insert(alias.clone(), owner.clone())
                .is_some_and(|previous| previous != *owner)
            {
                return Err(format!(
                    "{}: Actor/Flow dual request has an ambiguous display identity {alias}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ));
            }
        }
    }

    let mut resolved_indexes = std::collections::BTreeMap::<crate::core::NodeId, Vec<usize>>::new();
    for (index, result) in resolved.iter().enumerate() {
        if let Some(owner) = owner_by_alias.get(&result.func_name) {
            resolved_indexes
                .entry(owner.clone())
                .or_default()
                .push(index);
        }
    }

    let mut mir_by_owner =
        std::collections::BTreeMap::<crate::core::NodeId, VerificationResult>::new();
    for result in batch.results {
        let owner = owner_by_alias.get(&result.func_name).ok_or_else(|| {
            format!(
                "{}: Actor/Flow MIR result '{}' has no Checker owner identity",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                result.func_name
            )
        })?;
        if mir_by_owner.insert(owner.clone(), result).is_some() {
            return Err(format!(
                "{}: Actor/Flow MIR emitted duplicate results for Checker owner {}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                owner.0
            ));
        }
    }

    let mut paired_owners = std::collections::BTreeSet::new();
    for (owner, mut mir_result) in mir_by_owner {
        let Some(indexes) = resolved_indexes.get(&owner) else {
            if mir_result.status == VerifStatus::NoObligations {
                continue;
            }
            if flow_callsite_owners.contains(&owner) {
                // The Resolved owner historically did not emit a peer for
                // this Flow callsite obligation. Keep the source-identified
                // MIR observation once; the complete call edge and its
                // Checker callsite census are sealed by the same receipt.
                resolved.push(mir_result);
                paired_owners.insert(owner);
                continue;
            }
            return Err(format!(
                "{}: Actor/Flow MIR proof result '{}' has no Resolved peer for Checker owner {}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                mir_result.func_name,
                owner.0
            ));
        };
        if indexes.len() != 1 {
            return Err(format!(
                "{}: Actor/Flow dual request found {} Resolved results for Checker owner {}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                indexes.len(),
                owner.0
            ));
        }
        let index = indexes[0];
        let display_name = resolved[index].func_name.clone();
        mir_result.func_name = display_name.clone();
        let mut merged = merge_engine_verdicts_with_secondary_label(
            vec![resolved[index].clone()],
            vec![mir_result],
            "canonical_mir_actor_flow",
        );
        if merged.len() != 1 {
            return Err(format!(
                "{}: Actor/Flow dual owner merge returned an invalid result count",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        merged[0].func_name = display_name;
        resolved[index] = merged.pop().expect("one merged Actor/Flow owner result");
        paired_owners.insert(owner);
    }

    for (owner, indexes) in resolved_indexes {
        if indexes.len() != 1 {
            return Err(format!(
                "{}: Actor/Flow dual request found duplicate Resolved results for Checker owner {}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                owner.0
            ));
        }
        if !paired_owners.contains(&owner)
            && resolved[indexes[0]].status != VerifStatus::NoObligations
        {
            return Err(format!(
                "{}: Resolved proof result '{}' has no canonical MIR peer for Checker owner {}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                resolved[indexes[0]].func_name,
                owner.0
            ));
        }
    }

    Ok(resolved)
}

/// Follow exact Checker call edges back from the selected Flow transition
/// callers. MIR-only callsite results are retained only for owners in this
/// source-sealed caller closure.
fn actor_flow_callsite_owner_closure(
    projection: &crate::core::mir::verification::CheckerBoundActorFlowProjectionV1,
) -> std::collections::BTreeSet<crate::core::NodeId> {
    let mut owners = projection.transition_callers.clone();
    loop {
        let mut changed = false;
        for (caller, callee) in &projection.call_edges {
            if owners.contains(callee) && owners.insert(caller.clone()) {
                changed = true;
            }
        }
        if !changed {
            return owners;
        }
    }
}

fn format_check_errors(diagnostics: Vec<crate::diagnostic::Diagnostic>) -> String {
    diagnostics
        .into_iter()
        .map(|diagnostic| {
            format!(
                "{}:{}: {}",
                diagnostic.span.start_line, diagnostic.span.start_col, diagnostic.message
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod values_field_move_route_tests {
    use super::*;

    #[test]
    fn values_field_move_verifier_receipt_is_exact_and_cross_profile_is_rejected() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_values_field_move.mimi"
        ))
        .expect("read Values field-move fixture");
        let file = parse_memory_source(&source, "verifier-values-field-move")
            .expect("parse Values field-move fixture");
        let checked = crate::core::check_program(&file).expect("check Values field-move fixture");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesFieldMove
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };

        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize the exact Values field-move route");
        assert!(route.materialized_ordinary_map_owned_any_v2_values_field_move_candidate);
        let materialized_values_receipt = route
            .ordinary_map_owned_any_v2_values_field_move_route_receipt
            .clone()
            .expect("dedicated Values field-move route receipt");
        let canonical = route.program;
        let values_receipt = canonical
            .route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE);
        assert_eq!(values_receipt, materialized_values_receipt);
        values_receipt
            .validate_against_program(&canonical)
            .expect("Values field-move route receipt binds the canonical MIR");
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_field_move_route(
            &checked, &canonical,
        )
        .expect("Checker and MIR Values field-move receipts agree");
        validate_ordinary_map_owned_any_v2_receipt_binding(&canonical, &values_receipt)
            .expect("verifier accepts the exact Values field-move receipt");

        let checked_get_receipt = canonical
            .route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE);
        assert!(validate_ordinary_map_owned_any_v2_receipt_binding(
            &canonical,
            &checked_get_receipt
        )
        .is_err());

        let broad_receipt =
            canonical.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let broad_results =
            enforce_ordinary_map_owned_any_v2_mir_readiness(&canonical, &broad_receipt, Vec::new());
        assert_eq!(broad_results.len(), 1);
        assert_eq!(broad_results[0].status, VerifStatus::NotInTrustedSubset);
        assert!(broad_results[0].artifact.is_none());

        let mut retagged_receipt = values_receipt.clone();
        retagged_receipt.profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND.to_string();
        assert!(retagged_receipt
            .verify_against_receipt(&values_receipt)
            .is_err());
    }

    #[test]
    fn public_verify_checked_accepts_the_values_field_move_profile_with_z3() {
        if !is_z3_available() {
            return;
        }

        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_values_field_move.mimi"
        ))
        .expect("read Values field-move fixture");
        let file = parse_memory_source(&source, "public-verifier-values-field-move")
            .expect("parse Values field-move fixture");
        let checked = crate::core::check_program(&file).expect("check Values field-move fixture");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesFieldMove
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };

        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let results = verify_checked(&checked, source_hash)
            .expect("public CheckedProgram verifier accepts the exact Values field-move route");
        assert!(
            results.is_empty(),
            "the fixture deliberately has no contracts, so successful route preflight returns no proof results"
        );
    }

    #[test]
    fn public_verify_checked_keeps_values_route_no_z3_fallback() {
        if is_z3_available() {
            return;
        }

        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_values_field_move.mimi"
        ))
        .expect("read Values field-move fixture");
        let file = parse_memory_source(&source, "public-verifier-values-field-move-no-z3")
            .expect("parse Values field-move fixture");
        let checked = crate::core::check_program(&file).expect("check Values field-move fixture");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesFieldMove
        );

        let results = verify_checked(&checked, String::new())
            .expect("no-Z3 CheckedProgram fallback handles Values field-move admission");
        assert!(
            !results.is_empty(),
            "the fixture's main callable is reported"
        );
        assert!(results.iter().all(|result| {
            result.status == VerifStatus::InfrastructureError && result.artifact.is_none()
        }));
    }
}

#[cfg(test)]
mod values_snapshot_replacement_route_tests {
    use super::*;

    fn checked_fixture() -> (String, crate::core::CheckedProgram) {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_values_snapshot_replacement.mimi"
        ))
        .expect("read Values snapshot-replacement fixture");
        let file = parse_memory_source(&source, "verifier-values-snapshot-replacement")
            .expect("parse Values snapshot-replacement fixture");
        let checked =
            crate::core::check_program(&file).expect("check Values snapshot-replacement fixture");
        (source, checked)
    }

    #[test]
    fn values_snapshot_replacement_verifier_receipt_is_exact_and_cross_profile_is_rejected() {
        let (_source, checked) = checked_fixture();
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesSnapshotReplacement
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };

        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize the exact Values snapshot-replacement route");
        let profile = crate::core::mir::CanonicalMirRouteProfile::
            OrdinaryMapOwnedAnyV2ValuesSnapshotReplacement;
        assert!(profile.is_admitted(route.admission));
        assert!(profile.is_materialized(&route));
        let canonical = route.program;
        let receipt = canonical.route_receipt(profile.as_str());
        receipt
            .validate_against_program(&canonical)
            .expect("snapshot-replacement route receipt binds the exact MIR");
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_snapshot_replacement_route(
            &checked, &canonical,
        )
        .expect("Checker and MIR snapshot-replacement receipts agree");
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_snapshot_replacement_mir_route(
            &canonical,
        )
        .expect("raw MIR snapshot-replacement receipt is exact");
        validate_ordinary_map_owned_any_v2_receipt_binding(&canonical, &receipt)
            .expect("raw verifier accepts the exact snapshot-replacement profile");

        for wrong_profile in [
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE,
        ] {
            let wrong_receipt = canonical.route_receipt(wrong_profile);
            assert!(
                validate_ordinary_map_owned_any_v2_receipt_binding(&canonical, &wrong_receipt)
                    .is_err(),
                "snapshot-replacement MIR must not borrow {wrong_profile}"
            );
        }

        let broad_receipt =
            canonical.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let broad_results =
            enforce_ordinary_map_owned_any_v2_mir_readiness(&canonical, &broad_receipt, Vec::new());
        assert_eq!(broad_results.len(), 1);
        assert_eq!(broad_results[0].status, VerifStatus::NotInTrustedSubset);
        assert!(broad_results[0].artifact.is_none());
    }

    #[test]
    fn public_checked_verifiers_keep_the_no_contract_snapshot_route_empty() {
        if !is_z3_available() {
            return;
        }

        let (source, checked) = checked_fixture();
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        assert!(verify_checked(&checked, source_hash.clone())
            .expect("public verifier accepts the exact snapshot-replacement route")
            .is_empty());
        assert!(verify_checked_dual(&checked, source_hash)
            .expect("dual verifier accepts the exact snapshot-replacement route")
            .is_empty());
    }
}

#[cfg(test)]
mod values_mixed_snapshot_route_tests {
    use super::*;

    fn checked_fixture() -> (String, crate::core::CheckedProgram) {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_values_mixed_snapshot.mimi"
        ))
        .expect("read mixed Values snapshot fixture");
        let file = parse_memory_source(&source, "verifier-values-mixed-snapshot")
            .expect("parse mixed Values snapshot fixture");
        let checked =
            crate::core::check_program(&file).expect("check mixed Values snapshot fixture");
        (source, checked)
    }

    #[test]
    fn values_mixed_snapshot_verifier_binds_exact_profile_and_keeps_broad_readiness_closed() {
        let (_source, checked) = checked_fixture();
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshot
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };

        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact mixed Values snapshot route");
        let profile =
            crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshot;
        assert_eq!(
            profile.as_str(),
            "map-any-owner-v2-values-mixed-snapshot-v1"
        );
        assert!(profile.is_admitted(route.admission));
        assert!(profile.is_materialized(&route));
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_values_mixed_snapshot_route_receipt
            .clone()
            .expect("dedicated mixed-snapshot route receipt");
        let canonical = route.program;
        let receipt = canonical.route_receipt(profile.as_str());
        assert_eq!(receipt, materialized_receipt);
        receipt
            .validate_against_program(&canonical)
            .expect("mixed-snapshot receipt binds the exact MIR");
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_route(
            &checked, &canonical,
        )
        .expect("Checker and MIR mixed-snapshot receipts agree");
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_mir_route(
            &canonical,
        )
        .expect("raw MIR mixed-snapshot receipt is exact");
        validate_ordinary_map_owned_any_v2_receipt_binding(&canonical, &receipt)
            .expect("raw verifier accepts only the exact mixed-snapshot binding");

        for wrong_profile in [
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE,
        ] {
            let wrong_receipt = canonical.route_receipt(wrong_profile);
            assert!(
                validate_ordinary_map_owned_any_v2_receipt_binding(&canonical, &wrong_receipt)
                    .is_err(),
                "mixed-snapshot MIR must not borrow {wrong_profile}"
            );
        }

        let broad_receipt =
            canonical.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let broad_results =
            enforce_ordinary_map_owned_any_v2_mir_readiness(&canonical, &broad_receipt, Vec::new());
        assert_eq!(broad_results.len(), 1);
        assert_eq!(broad_results[0].status, VerifStatus::NotInTrustedSubset);
        assert!(broad_results[0].artifact.is_none());

        let other_source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_values_snapshot_replacement.mimi"
        ))
        .expect("read unrelated Values snapshot fixture");
        let other_file = parse_memory_source(&other_source, "unrelated-values-snapshot")
            .expect("parse unrelated Values snapshot fixture");
        let unrelated =
            crate::core::check_program(&other_file).expect("check unrelated Values snapshot");
        assert!(
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_route(
                &unrelated, &canonical
            )
            .is_err(),
            "a different CheckedProgram cannot authorize the mixed-snapshot MIR"
        );
    }

    #[test]
    fn checked_mixed_snapshot_no_contract_exception_follows_both_consumer_preflights() {
        if !is_z3_available() {
            return;
        }

        let (source, checked) = checked_fixture();
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        assert!(verify_checked(&checked, source_hash.clone())
            .expect("checked verifier runs the exact mixed-snapshot consumer preflights")
            .is_empty());
        assert!(verify_checked_dual(&checked, source_hash)
            .expect("dual verifier runs the exact mixed-snapshot consumer preflights")
            .is_empty());
    }
}

#[cfg(test)]
mod values_mixed_snapshot_remove_route_tests {
    use super::*;

    fn checked_fixture() -> (String, crate::core::CheckedProgram) {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_values_snapshot_remove.mimi"
        ))
        .expect("read Values mixed-snapshot-remove fixture");
        let file = parse_memory_source(&source, "verifier-values-mixed-snapshot-remove")
            .expect("parse Values mixed-snapshot-remove fixture");
        let checked =
            crate::core::check_program(&file).expect("check Values mixed-snapshot-remove fixture");
        (source, checked)
    }

    #[test]
    fn values_mixed_snapshot_remove_verifier_binds_exact_profile_and_keeps_broad_readiness_closed()
    {
        let (_source, checked) = checked_fixture();
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshotRemove
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };

        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact Values mixed-snapshot-remove route");
        let profile = crate::core::mir::CanonicalMirRouteProfile::
            OrdinaryMapOwnedAnyV2ValuesMixedSnapshotRemove;
        assert_eq!(
            profile.as_str(),
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE
        );
        assert_eq!(
            profile.as_str(),
            "map-any-owner-v2-values-mixed-snapshot-remove-v1"
        );
        assert!(profile.is_admitted(route.admission));
        assert!(profile.is_materialized(&route));
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route_receipt
            .clone()
            .expect("dedicated Values mixed-snapshot-remove route receipt");
        let canonical = route.program;
        let receipt = canonical.route_receipt(profile.as_str());
        assert_eq!(receipt, materialized_receipt);
        receipt
            .validate_against_program(&canonical)
            .expect("mixed-snapshot-remove receipt binds the exact MIR");
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route(
            &checked, &canonical,
        )
        .expect("Checker and MIR mixed-snapshot-remove receipts agree");
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_mir_route(
            &canonical,
        )
        .expect("raw MIR mixed-snapshot-remove receipt is exact");
        validate_ordinary_map_owned_any_v2_receipt_binding(&canonical, &receipt)
            .expect("raw verifier accepts only the exact mixed-snapshot-remove binding");

        for wrong_profile in [
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE,
        ] {
            let wrong_receipt = canonical.route_receipt(wrong_profile);
            assert!(
                validate_ordinary_map_owned_any_v2_receipt_binding(&canonical, &wrong_receipt)
                    .is_err(),
                "mixed-snapshot-remove MIR must not borrow {wrong_profile}"
            );
        }

        let broad_receipt =
            canonical.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let broad_results =
            enforce_ordinary_map_owned_any_v2_mir_readiness(&canonical, &broad_receipt, Vec::new());
        assert_eq!(broad_results.len(), 1);
        assert_eq!(broad_results[0].status, VerifStatus::NotInTrustedSubset);
        assert!(broad_results[0].artifact.is_none());

        let unrelated_source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_values_mixed_snapshot.mimi"
        ))
        .expect("read unrelated mixed-snapshot fixture");
        let unrelated_file = parse_memory_source(&unrelated_source, "unrelated-mixed-snapshot")
            .expect("parse unrelated mixed-snapshot fixture");
        let unrelated =
            crate::core::check_program(&unrelated_file).expect("check unrelated mixed snapshot");
        assert!(
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route(
                &unrelated,
                &canonical,
            )
            .is_err(),
            "a different CheckedProgram cannot authorize the mixed-snapshot-remove MIR"
        );
    }

    #[test]
    fn checked_mixed_snapshot_remove_no_contract_exception_follows_both_consumer_preflights() {
        if !is_z3_available() {
            return;
        }

        let (source, checked) = checked_fixture();
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        assert!(verify_checked(&checked, source_hash.clone())
            .expect("checked verifier runs both exact mixed-snapshot-remove consumer preflights")
            .is_empty());
        assert!(verify_checked_dual(&checked, source_hash)
            .expect("dual verifier runs both exact mixed-snapshot-remove consumer preflights")
            .is_empty());
    }
}

#[cfg(test)]
mod ordinary_map_owned_any_v2_is_empty_route_tests {
    use super::*;

    fn checked_fixture() -> (String, crate::core::CheckedProgram) {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_v2_is_empty.mimi"
        ))
        .expect("read owner-v2 IsEmpty fixture");
        let file = parse_memory_source(&source, "verifier-owner-v2-is-empty")
            .expect("parse owner-v2 IsEmpty fixture");
        let checked = crate::core::check_program(&file).expect("check owner-v2 IsEmpty fixture");
        (source, checked)
    }

    #[test]
    fn is_empty_requires_the_exact_checker_and_mir_route_across_capability_and_raw_apis() {
        let (_source, checked) = checked_fixture();
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IsEmpty
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };

        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact owner-v2 IsEmpty route");
        let profile = crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty;
        assert_eq!(
            profile.as_str(),
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE
        );
        assert!(profile.is_admitted(route.admission));
        assert!(profile.is_materialized(&route));

        let canonical = route.program;
        let receipt = canonical.route_receipt(profile.as_str());
        let is_empty_count = canonical
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { .. }
                )
            })
            .count();
        assert_eq!(
            is_empty_count, 2,
            "the exact profile includes both the empty-root and persistent-successor queries"
        );
        crate::core::mir::validate_ordinary_map_owned_any_v2_is_empty_route(&checked, &canonical)
            .expect("Checker and MIR IsEmpty receipts agree");
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_is_empty_route(
            &checked, &canonical, &receipt,
        )
        .expect("the exact Checker-aware capability entry accepts IsEmpty");
        assert!(
            validate_mir_capabilities(&canonical).is_err(),
            "generic MIR capability must not accept an IsEmpty bool by itself"
        );

        assert!(
            verify_mir_with_route_receipt(&canonical, &receipt, String::new()).is_err(),
            "raw MIR verification has no Checker authority for IsEmpty"
        );
        let generic_receipt =
            canonical.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        assert!(
            verify_mir_with_route_receipt(&canonical, &generic_receipt, String::new()).is_err(),
            "generic V2 receipt cannot borrow IsEmpty"
        );
        let wrong_receipt = canonical.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE,
        );
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_is_empty_route(
                &checked,
                &canonical,
                &wrong_receipt,
                String::new(),
            )
            .is_err(),
            "a different exact V2 profile cannot authorize IsEmpty"
        );

        let unrelated_source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_values_mixed_snapshot.mimi"
        ))
        .expect("read unrelated Values fixture");
        let unrelated_file = parse_memory_source(&unrelated_source, "unrelated-owner-v2-values")
            .expect("parse unrelated Values fixture");
        let unrelated =
            crate::core::check_program(&unrelated_file).expect("check unrelated Values fixture");
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_is_empty_route(
                &unrelated,
                &canonical,
                &receipt,
                String::new(),
            )
            .is_err(),
            "a different CheckedProgram cannot authorize the IsEmpty MIR"
        );

        let mut replaced_operation = canonical.clone();
        let (mut function, instruction_id) = replaced_operation
            .functions()
            .values()
            .find_map(|function| {
                function
                    .blocks
                    .values()
                    .flat_map(|block| &block.instructions)
                    .find(|instruction| {
                        matches!(
                            instruction.kind,
                            crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty {
                                ..
                            }
                        )
                    })
                    .map(|instruction| (function.clone(), instruction.id.clone()))
            })
            .expect("the Checker-lowered MIR contains an IsEmpty instruction");
        let instruction = function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| instruction.id == instruction_id)
            .expect("same IsEmpty instruction in its owner");
        instruction.kind = crate::core::mir::MirInstructionKind::Nop;
        replaced_operation.replace_function_for_test_only(function);
        let resealed = replaced_operation.route_receipt(profile.as_str());
        assert!(
            crate::core::mir::validate_ordinary_map_owned_any_v2_is_empty_route(
                &checked,
                &replaced_operation,
            )
            .is_err(),
            "a re-sealed MIR cannot replace the Checker-bound IsEmpty operation receipt"
        );
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_is_empty_route(
                &checked,
                &replaced_operation,
                &resealed,
            )
            .is_err(),
            "capability validation rejects a replacement IsEmpty operation receipt"
        );
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_is_empty_route(
                &checked,
                &replaced_operation,
                &resealed,
                String::new(),
            )
            .is_err(),
            "verifier validation rejects a replacement IsEmpty operation receipt"
        );
    }

    #[test]
    fn checked_single_and_dual_verifiers_keep_the_narrow_is_empty_route() {
        assert!(
            is_z3_available(),
            "the IsEmpty L1/L2 verifier gate requires a real Z3 solver; it must not pass by skipping the proof"
        );
        eprintln!("MIMI_IS_EMPTY_VERIFIER z3_available=true outcome=executed");
        let (source, checked) = checked_fixture();
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        assert!(verify_checked(&checked, source_hash.clone())
            .expect("single verifier consumes the exact IsEmpty route")
            .is_empty());
        assert!(verify_checked_dual(&checked, source_hash)
            .expect("dual verifier consumes the exact IsEmpty route")
            .is_empty());
    }
}

#[cfg(test)]
mod ordinary_map_owned_any_v2_record_unit_route_tests {
    use super::*;

    fn checked_fixture() -> crate::core::CheckedProgram {
        let source =
            include_str!("../../tests/fixtures/mir_ordinary_map_any_owner_v2_record_unit.mimi");
        let file = parse_memory_source(source, "verifier-owner-v2-record-unit")
            .expect("parse owner-v2 RecordUnit fixture");
        crate::core::check_program(&file).expect("check owner-v2 RecordUnit fixture")
    }

    #[test]
    fn record_unit_verifier_rejects_raw_and_substituted_receipts() {
        let checked = checked_fixture();
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RecordUnit
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact mixed-payload RecordUnit route");
        let canonical = route.program;
        let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE;
        let receipt = canonical.route_receipt(profile);
        crate::core::mir::validate_ordinary_map_owned_any_v2_record_unit_route(
            &checked, &canonical,
        )
        .expect("Checker and MIR RecordUnit receipts agree");
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_record_unit_route(
            &checked, &canonical, &receipt,
        )
        .expect("the exact Checker-aware RecordUnit capability gate accepts its graph");
        assert_eq!(receipt.mir_digest, canonical.canonical_digest());
        // This fixture declares no contract obligations, so successful route
        // admission may produce no symbolic results. The assertion below
        // exercises the dedicated receipt and capability checks; it is not a
        // contract proof claim.
        verify_mir_with_ordinary_map_owned_any_v2_record_unit_route(
            &checked,
            &canonical,
            &receipt,
            String::new(),
        )
        .expect("the dedicated verifier validates the same Checker/MIR receipt pair");
        let generic_errors = validate_mir_capabilities(&canonical)
            .expect_err("generic MIR capability cannot authorize RecordUnit String-field output");
        assert!(
            generic_errors.iter().any(|error| {
                error.contains(
                    "String-field borrow output requires exact Checker/MIR route validation",
                )
            }),
            "generic capability rejection identifies missing route ownership: {generic_errors:?}"
        );

        let raw_profile_error = verify_mir_with_route_receipt(&canonical, &receipt, String::new())
            .expect_err("a raw exact-profile receipt cannot substitute for Checker authority");
        assert!(
            raw_profile_error.contains(
                "RecordUnit String-field output requires the exact CheckedProgram-aware verifier route",
            ),
            "raw receipt rejection identifies its route-owner gap: {raw_profile_error}"
        );
        let broad_receipt =
            canonical.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let broad_error = verify_mir_with_route_receipt(&canonical, &broad_receipt, String::new())
            .expect_err("the broad owner-v2 receipt cannot substitute for RecordUnit");
        assert!(
            broad_error.contains(
                "RecordUnit String-field output requires the exact CheckedProgram-aware verifier route",
            ),
            "broad receipt rejection identifies its route-owner gap: {broad_error}"
        );
        let is_empty_receipt =
            canonical.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE);
        let error = verify_mir_with_ordinary_map_owned_any_v2_record_unit_route(
            &checked,
            &canonical,
            &is_empty_receipt,
            String::new(),
        )
        .expect_err("another exact V2 profile cannot authorize RecordUnit");
        assert!(
            error.contains("RecordUnit verifier requires exact route profile"),
            "{error}"
        );
    }
}

#[cfg(test)]
mod ordinary_map_owned_any_v2_zero_miss_i32_route_tests {
    use super::*;

    fn checked_fixture() -> (String, crate::core::CheckedProgram) {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_zero_miss_i32.mimi"
        ))
        .expect("read owner-v2 ZeroMissI32 fixture");
        let file = parse_memory_source(&source, "verifier-owner-v2-zero-miss-i32")
            .expect("parse owner-v2 ZeroMissI32 fixture");
        let checked = crate::core::check_program(&file).expect("check ZeroMissI32 fixture");
        (source, checked)
    }

    #[test]
    fn zero_miss_i32_requires_the_exact_checker_route_receipt() {
        let (_source, checked) = checked_fixture();
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ZeroMissI32
        );

        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact owner-v2 ZeroMissI32 route");
        let profile = crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ZeroMissI32;
        assert_eq!(
            profile.as_str(),
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE
        );
        assert!(profile.is_admitted(route.admission));
        assert!(profile.is_materialized(&route));
        assert!(route
            .ordinary_map_owned_any_v2_zero_miss_i32_route_receipt
            .is_some());

        let canonical = route.program;
        let receipt = canonical.route_receipt(profile.as_str());
        crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_route(
            &checked, &canonical,
        )
        .expect("Checker and MIR ZeroMissI32 receipts agree");
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
            &checked, &canonical, &receipt,
        )
        .expect("exact Checker-aware ZeroMissI32 capability gate accepts the route");
        assert!(
            validate_mir_capabilities(&canonical).is_err(),
            "generic MIR capability must not accept a canonical i32 miss unpack"
        );

        assert!(
            verify_mir(&canonical, String::new()).is_err(),
            "raw MIR verification has no Checker authority for the zero-miss ABI"
        );
        assert!(
            verify_mir_with_route_receipt(&canonical, &receipt, String::new()).is_err(),
            "a raw exact-profile receipt cannot substitute for Checker authority"
        );
        verify_mir_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
            &checked,
            &canonical,
            &receipt,
            String::new(),
        )
        .expect("Checker-aware ZeroMissI32 verifier accepts the exact receipt");

        let mut fake_receipt = receipt.clone();
        fake_receipt.mir_digest = "forged-mir-digest".into();
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
                &checked,
                &canonical,
                &fake_receipt,
                String::new(),
            )
            .is_err(),
            "a forged route receipt cannot be rebound to the canonical MIR"
        );
        let mut wrong_profile = receipt.clone();
        wrong_profile.profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND.into();
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
                &checked,
                &canonical,
                &wrong_profile,
                String::new(),
            )
            .is_err(),
            "a generic V2 route receipt cannot authorize ZeroMissI32"
        );

        let (mut duplicated_function, get_any_instruction) = canonical
            .functions()
            .values()
            .find_map(|function| {
                function.blocks.iter().find_map(|(block_id, block)| {
                    block
                        .instructions
                        .iter()
                        .find(|instruction| {
                            matches!(
                                instruction.kind,
                                crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny {
                                    ..
                                }
                            )
                        })
                        .map(|instruction| {
                            (function.clone(), (block_id.clone(), instruction.clone()))
                        })
                })
            })
            .expect("canonical route contains GetAny");
        duplicated_function
            .blocks
            .get_mut(&get_any_instruction.0)
            .expect("GetAny block")
            .instructions
            .push(get_any_instruction.1);
        let mut duplicated_operation = canonical.clone();
        duplicated_operation.replace_function_for_test_only(duplicated_function);
        let duplicated_receipt = duplicated_operation.route_receipt(profile.as_str());
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
                &checked,
                &duplicated_operation,
                &duplicated_receipt,
            )
            .is_err(),
            "duplicating a receipted GetAny site must fail the exact operation census"
        );
    }

    #[test]
    fn checked_single_and_dual_verifiers_keep_the_narrow_zero_miss_i32_route() {
        if !is_z3_available() {
            return;
        }
        let (source, checked) = checked_fixture();
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        assert!(verify_checked(&checked, source_hash.clone())
            .expect("single verifier consumes the exact ZeroMissI32 route")
            .is_empty());
        assert!(verify_checked_dual(&checked, source_hash)
            .expect("dual verifier consumes the exact ZeroMissI32 route")
            .is_empty());
    }
}

#[cfg(test)]
mod ordinary_map_owned_any_v2_clone_isolation_route_tests {
    use super::*;

    fn checked_fixture(path: &str, label: &str) -> (String, crate::core::CheckedProgram) {
        let source =
            std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
                .expect("read CloneIsolation fixture");
        let file = parse_memory_source(&source, label).expect("parse CloneIsolation fixture");
        let checked = crate::core::check_program(&file).expect("check CloneIsolation fixture");
        (source, checked)
    }

    #[test]
    fn clone_isolation_requires_exact_checker_profile_and_source_bound_receipt() {
        let (source, checked) = checked_fixture(
            "tests/fixtures/mir_ordinary_map_owned_any_v2_clone_source_first.mimi",
            "verifier-clone-isolation-source-first",
        );
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };

        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact CloneIsolation route");
        let profile =
            crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CloneIsolation;
        assert_eq!(
            profile.as_str(),
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE
        );
        assert_eq!(
            route.admission.ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation
        );
        assert!(profile.is_admitted(route.admission));
        assert!(route.materialized_ordinary_map_owned_any_v2_clone_isolation_candidate);
        assert!(profile.is_materialized(&route));

        let materialized_receipt = route
            .ordinary_map_owned_any_v2_clone_isolation_route_receipt
            .clone()
            .expect("dedicated CloneIsolation route receipt");
        let canonical = route.program;
        let receipt = canonical.route_receipt(profile.as_str());
        assert_eq!(receipt, materialized_receipt);
        crate::core::mir::validate_ordinary_map_owned_any_v2_clone_isolation_route(
            &checked, &canonical,
        )
        .expect("Checker and MIR CloneIsolation receipts agree");

        let broad_receipt =
            canonical.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        assert!(
            verify_mir_with_route_receipt(&canonical, &broad_receipt, String::new()).is_err(),
            "the broad owner-v2 receipt cannot authorize CloneIsolation"
        );
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_clone_isolation_route(
                &checked,
                &canonical,
                &broad_receipt,
                String::new(),
            )
            .is_err(),
            "a different route profile cannot authorize CloneIsolation"
        );

        let (other_source, other_checked) = checked_fixture(
            "tests/fixtures/mir_ordinary_map_owned_any_v2_clone_clone_first.mimi",
            "verifier-clone-isolation-clone-first",
        );
        assert_ne!(source, other_source);
        assert!(
            crate::core::mir::validate_ordinary_map_owned_any_v2_clone_isolation_route(
                &other_checked,
                &canonical,
            )
            .is_err(),
            "a different CheckedProgram cannot authorize the CloneIsolation MIR"
        );
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_clone_isolation_route(
                &other_checked,
                &canonical,
                &receipt,
                String::new(),
            )
            .is_err(),
            "the verifier rejects a mismatched CheckedProgram/MIR pair"
        );
    }

    #[test]
    fn checked_single_and_dual_verifiers_use_the_narrow_clone_isolation_route() {
        if !is_z3_available() {
            eprintln!("MIMI_CLONE_ISOLATION_VERIFIER z3_available=false outcome=skipped");
            return;
        }
        eprintln!("MIMI_CLONE_ISOLATION_VERIFIER z3_available=true outcome=executed");
        for (path, label) in [
            (
                "tests/fixtures/mir_ordinary_map_owned_any_v2_clone_source_first.mimi",
                "verifier-clone-isolation-source-first",
            ),
            (
                "tests/fixtures/mir_ordinary_map_owned_any_v2_clone_clone_first.mimi",
                "verifier-clone-isolation-clone-first",
            ),
        ] {
            let (source, checked) = checked_fixture(path, label);
            let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            assert!(verify_checked(&checked, source_hash.clone())
                .expect("single verifier consumes CloneIsolation route")
                .is_empty());
            assert!(verify_checked_dual(&checked, source_hash)
                .expect("dual verifier consumes CloneIsolation route")
                .is_empty());
        }
    }
}

#[cfg(test)]
mod ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_tests {
    use super::*;

    fn checked_fixture(path: &str, label: &str) -> crate::core::CheckedProgram {
        let source =
            std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
                .unwrap_or_else(|error| {
                    panic!("read PersistentRootAliasReplacement fixture: {error}")
                });
        let file = parse_memory_source(&source, label).unwrap_or_else(|error| {
            panic!("parse PersistentRootAliasReplacement fixture: {error}")
        });
        crate::core::check_program(&file).unwrap_or_else(|errors| {
            panic!("check PersistentRootAliasReplacement fixture: {errors:?}")
        })
    }

    #[test]
    fn persistent_root_alias_replacement_requires_exact_checker_source_and_profile() {
        let checked = checked_fixture(
            "tests/fixtures/mir_ordinary_map_owned_any_v2_clone.mimi",
            "verifier-persistent-root-alias-replacement",
        );
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::PersistentRootAliasReplacement
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };

        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact PersistentRootAliasReplacement route");
        let profile = crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2PersistentRootAliasReplacement;
        assert_eq!(
            profile.as_str(),
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE
        );
        assert!(profile.is_admitted(route.admission));
        assert!(profile.is_materialized(&route));
        assert!(
            route
                .materialized_ordinary_map_owned_any_v2_persistent_root_alias_replacement_candidate
        );
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_receipt
            .clone()
            .expect("dedicated materialized receipt exists");
        let canonical = route.program;
        let receipt = canonical.route_receipt(profile.as_str());
        assert_eq!(receipt, materialized_receipt);
        crate::core::mir::validate_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
            &checked,
            &canonical,
        )
        .expect("Checker source and MIR are bound by the exact route validator");
        assert_eq!(receipt.mir_digest, canonical.canonical_digest());

        assert!(
            verify_mir_with_route_receipt(&canonical, &receipt, String::new()).is_err(),
            "raw MIR route receipts cannot authorize the source-bound profile"
        );
        let wrong_profile = canonical
            .route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE);
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
                &checked,
                &canonical,
                &wrong_profile,
                String::new(),
            )
            .is_err()
        );

        let other_checked = checked_fixture(
            "tests/fixtures/mir_ordinary_map_owned_any_v2_clone_source_first.mimi",
            "verifier-persistent-root-alias-replacement-mismatched-source",
        );
        assert!(
            crate::core::mir::validate_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
                &other_checked,
                &canonical,
            )
            .is_err(),
            "a different Checker route cannot authorize this MIR graph"
        );

        if !is_z3_available() {
            return;
        }
        let source_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/mir_ordinary_map_owned_any_v2_clone.mimi"),
        )
        .expect("read source bytes for provenance");
        let source_hash = blake3::hash(&source_bytes).to_hex().to_string();
        let exact_results =
            verify_mir_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
                &checked,
                &canonical,
                &receipt,
                source_hash.clone(),
            )
            .expect("source-aware verifier accepts exact profile");
        let single_results = verify_checked(&checked, source_hash.clone())
            .expect("single CheckedProgram verifier dispatches exact profile");
        let dual_results = verify_checked_dual(&checked, source_hash.clone())
            .expect("dual CheckedProgram verifier dispatches exact profile");

        let assert_capacity_trap = |results: &[VerificationResult], entry: &str| {
            assert_eq!(
                results.len(),
                1,
                "{entry} preserves the sole owner-scoped result: {results:#?}"
            );
            let result = &results[0];
            assert_eq!(
                result.status,
                VerifStatus::Disproven,
                "{entry} must not convert the reachable Map capacity trap to a proof"
            );
            assert!(
                result
                    .message
                    .contains("canonical MIR body can reach trap 'E0800' under requires"),
                "{entry} retains the exact reachable E0800 trap: {result:#?}"
            );
            let artifact = result
                .artifact
                .as_ref()
                .expect("the counterexample retains its proof-context artifact");
            assert_eq!(artifact.source_hash, source_hash);
            assert_eq!(artifact.mir_hash, canonical.canonical_digest());
            assert_eq!(artifact.mir_route_receipt.as_ref(), Some(&receipt));
            (
                result
                    .func_name
                    .strip_prefix("function:")
                    .unwrap_or(&result.func_name)
                    .to_owned(),
                result.status.clone(),
                result.message.clone(),
            )
        };

        let exact_verdict = assert_capacity_trap(&exact_results, "source-aware verifier");
        assert_eq!(
            assert_capacity_trap(&single_results, "single CheckedProgram verifier"),
            exact_verdict,
            "single checked dispatch preserves the exact source-aware capacity verdict"
        );
        assert_eq!(
            assert_capacity_trap(&dual_results, "dual CheckedProgram verifier"),
            exact_verdict,
            "dual checked dispatch preserves the exact source-aware capacity verdict"
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
    }
}

#[cfg(test)]
mod computed_key_get_route_tests {
    use super::*;

    fn checked_fixture() -> (String, crate::core::CheckedProgram) {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_computed_key_get.mimi"
        ))
        .expect("read computed-key Get fixture");
        let file = parse_memory_source(&source, "verifier-computed-key-get")
            .expect("parse computed-key Get fixture");
        let checked = crate::core::check_program(&file).expect("check computed-key Get fixture");
        (source, checked)
    }

    #[test]
    fn computed_key_get_verifier_binds_exact_profile_and_keeps_broad_readiness_closed() {
        let (_source, checked) = checked_fixture();
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedKeyGet
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };

        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact computed-key Get route");
        let profile =
            crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedKeyGet;
        assert_eq!(
            profile.as_str(),
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE
        );
        assert_eq!(profile.as_str(), "map-any-owner-v2-computed-key-get-v1");
        assert!(profile.is_admitted(route.admission));
        assert!(profile.is_materialized(&route));

        let canonical = route.program;
        let receipt = canonical.route_receipt(profile.as_str());
        receipt
            .validate_against_program(&canonical)
            .expect("computed-key Get route receipt binds the exact MIR");
        crate::core::mir::validate_ordinary_map_owned_any_v2_computed_key_get_route(
            &checked, &canonical,
        )
        .expect("Checker and MIR computed-key Get receipts agree");
        crate::core::mir::validate_ordinary_map_owned_any_v2_computed_key_get_mir_route(&canonical)
            .expect("raw MIR computed-key Get receipt is exact");
        validate_ordinary_map_owned_any_v2_receipt_binding(&canonical, &receipt)
            .expect("raw verifier accepts only the exact computed-key Get profile");

        for wrong_profile in [
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE,
        ] {
            let wrong_receipt = canonical.route_receipt(wrong_profile);
            assert!(
                validate_ordinary_map_owned_any_v2_receipt_binding(&canonical, &wrong_receipt)
                    .is_err(),
                "computed-key Get MIR must not borrow {wrong_profile}"
            );
        }

        let broad_receipt =
            canonical.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let broad_results =
            enforce_ordinary_map_owned_any_v2_mir_readiness(&canonical, &broad_receipt, Vec::new());
        assert_eq!(broad_results.len(), 1);
        assert_eq!(broad_results[0].status, VerifStatus::NotInTrustedSubset);
        assert!(broad_results[0].artifact.is_none());

        let unrelated_source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_miss_i32.mimi"
        ))
        .expect("read unrelated CheckedGet fixture");
        let unrelated_file = parse_memory_source(&unrelated_source, "unrelated-checked-get")
            .expect("parse unrelated CheckedGet fixture");
        let unrelated =
            crate::core::check_program(&unrelated_file).expect("check unrelated CheckedGet");
        assert!(
            crate::core::mir::validate_ordinary_map_owned_any_v2_computed_key_get_route(
                &unrelated, &canonical,
            )
            .is_err(),
            "an unrelated CheckedProgram cannot authorize computed-key Get MIR"
        );
    }

    #[test]
    fn checked_computed_key_get_no_contract_exception_runs_both_consumer_preflights() {
        if !is_z3_available() {
            return;
        }

        let (source, checked) = checked_fixture();
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        assert!(verify_checked(&checked, source_hash.clone())
            .expect("checked verifier accepts the exact computed-key Get preflights")
            .is_empty());
        assert!(verify_checked_dual(&checked, source_hash)
            .expect("dual verifier accepts the exact computed-key Get preflights")
            .is_empty());
    }
}

#[cfg(test)]
mod direct_print_route_tests {
    use super::*;

    fn fixture() -> (String, crate::core::CheckedProgram) {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_direct_print_hit_i32.mimi"
        ))
        .expect("read V2 DirectPrint fixture");
        let file = parse_memory_source(&source, "verifier-v2-direct-print")
            .expect("parse V2 DirectPrint fixture");
        let checked = crate::core::check_program(&file).expect("check V2 DirectPrint fixture");
        (source, checked)
    }

    #[test]
    fn direct_print_verifier_requires_its_exact_route_receipt() {
        let (source, checked) = fixture();
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::DirectPrint
        );

        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact DirectPrint route");
        let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE;
        assert!(route.materialized_ordinary_map_owned_any_v2_direct_print_candidate);
        let canonical = route.program;
        let receipt = canonical.route_receipt(profile);
        receipt
            .validate_against_program(&canonical)
            .expect("DirectPrint route receipt binds the exact MIR");
        crate::core::mir::validate_ordinary_map_owned_any_v2_direct_print_mir_route(&canonical)
            .expect("the MIR carries the exact Checker GetAny/println/Drop shape");
        validate_ordinary_map_owned_any_v2_receipt_binding(&canonical, &receipt)
            .expect("verifier accepts the exact DirectPrint profile");
        assert!(
            validate_mir_capabilities(&canonical).is_err(),
            "the generic verifier capability path must still reject unreceipted DirectPrint"
        );
        validate_mir_capabilities_with_direct_print_route_receipt(&canonical, &receipt)
            .expect("the exact DirectPrint receipt authorizes the complete MIR capability gate");

        for wrong_profile in [
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE,
        ] {
            let wrong_receipt = canonical.route_receipt(wrong_profile);
            assert!(
                validate_ordinary_map_owned_any_v2_receipt_binding(&canonical, &wrong_receipt,)
                    .is_err(),
                "DirectPrint MIR must not borrow {wrong_profile}"
            );
            assert!(
                validate_mir_capabilities_with_direct_print_route_receipt(
                    &canonical,
                    &wrong_receipt,
                )
                .is_err(),
                "DirectPrint capability must reject route profile {wrong_profile}"
            );
        }

        if is_z3_available() {
            let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            assert!(
                verify_mir_with_route_receipt(&canonical, &receipt, source_hash.clone())
                    .expect("verifier consumes the exact DirectPrint receipt")
                    .is_empty()
            );
            assert!(verify_mir(&canonical, source_hash)
                .expect("the convenience verifier infers DirectPrint")
                .is_empty());
            let checked_source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            assert!(verify_checked(&checked, checked_source_hash.clone())
                .expect("checked verifier uses the exact DirectPrint consumer profile")
                .is_empty());
            assert!(verify_checked_dual(&checked, checked_source_hash)
                .expect("dual checked verifier uses the exact DirectPrint consumer profile")
                .is_empty());
        }
    }

    #[test]
    fn direct_print_route_receipt_rejects_resealed_operation_mutation() {
        let (_source, checked) = fixture();
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact DirectPrint route");
        let mut forged = route.program;
        let (owner, mut function, instruction_id) = forged
            .functions()
            .iter()
            .find_map(|(owner, function)| {
                function
                    .blocks
                    .values()
                    .flat_map(|block| &block.instructions)
                    .find(|instruction| {
                        matches!(
                            instruction.kind,
                            crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Println {
                                ..
                            }
                        )
                    })
                    .map(|instruction| (owner.clone(), function.clone(), instruction.id.clone()))
            })
            .expect("Checker-lowered DirectPrint instruction");
        let instruction = function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| instruction.id == instruction_id)
            .expect("same DirectPrint instruction in owner");
        instruction.kind = crate::core::mir::MirInstructionKind::Nop;
        forged.replace_function_for_test_only(function);
        let resealed =
            forged.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE);
        assert!(
            validate_ordinary_map_owned_any_v2_receipt_binding(&forged, &resealed).is_err(),
            "a fresh whole-MIR digest cannot replace the exact DirectPrint receipt"
        );
        assert!(
            verify_mir_with_route_receipt(&forged, &resealed, String::new()).is_err(),
            "the public verifier rejects the operation mutation before proof analysis"
        );
        assert_eq!(owner.0, "function:main");
    }
}

#[cfg(test)]
mod ordinary_map_owned_any_v2_keys_snapshot_verifier_route_tests {
    use super::*;

    fn fixture() -> (String, crate::core::CheckedProgram) {
        let source = r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "answer", 41)
    let snapshot = keys(root)
    drop(empty)
    drop(root)
    let key = snapshot[0]
    drop(snapshot)
    println(key)
    drop(key)
    0
}
"#
        .to_owned();
        let file = parse_memory_source(&source, "verifier-keys-snapshot")
            .expect("parse KeysSnapshot verifier fixture");
        let checked = crate::core::check_program(&file).expect("check KeysSnapshot fixture");
        (source, checked)
    }

    #[test]
    fn keys_snapshot_checked_verifier_preflights_ast_free_bytecode_and_keeps_raw_receipts_closed() {
        let (source, checked) = fixture();
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::KeysSnapshot
        );
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact KeysSnapshot verifier route");
        let profile = crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2KeysSnapshot;
        assert!(profile.is_admitted(route.admission));
        let canonical = route.program;
        let receipt = canonical.route_receipt(profile.as_str());
        crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_route(
            &checked, &canonical,
        )
        .expect("Checker and MIR source receipts agree");
        crate::interp::bytecode::mir::compile_mir_program_with_keys_snapshot_route(
            &checked, &canonical,
        )
        .expect("exact CheckedProgram/MIR pair compiles through the AST-free bytecode consumer");
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_keys_snapshot_route(
            &checked, &canonical, &receipt,
        )
        .expect("exact KeysSnapshot route passes the capability gate");
        assert!(
            verify_mir_with_route_receipt(
                &canonical,
                &receipt,
                blake3::hash(source.as_bytes()).to_hex().to_string(),
            )
            .is_err(),
            "a raw public receipt cannot borrow the CheckedProgram-aware Keys authority"
        );

        if is_z3_available() {
            let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            assert!(
                verify_mir_with_ordinary_map_owned_any_v2_keys_snapshot_route(
                    &checked,
                    &canonical,
                    &receipt,
                    source_hash.clone(),
                )
                .expect("source-bound KeysSnapshot verifier consumes exact route")
                .is_empty()
            );
            assert!(verify_checked(&checked, source_hash)
                .expect("closed-profile routing uses the CheckedProgram-aware Keys verifier")
                .is_empty());
        }
    }

    #[test]
    fn keys_snapshot_checked_verifier_rejects_wrong_source_profile_stale_receipt_and_missing_witness(
    ) {
        let (_source, checked) = fixture();
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact KeysSnapshot verifier route");
        let canonical = route.program;
        let profile = crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2KeysSnapshot;
        let receipt = canonical.route_receipt(profile.as_str());

        let unrelated_file =
            parse_memory_source("func main() -> i32 { 0 }", "unrelated-keys-source")
                .expect("parse unrelated source");
        let unrelated =
            crate::core::check_program(&unrelated_file).expect("check unrelated source");
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_keys_snapshot_route(
                &unrelated,
                &canonical,
                &receipt,
                String::new(),
            )
            .is_err()
        );

        let mut wrong_profile = receipt.clone();
        wrong_profile.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE.to_owned();
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_keys_snapshot_route(
                &checked,
                &canonical,
                &wrong_profile,
                String::new(),
            )
            .is_err()
        );

        let mut stale = canonical.clone();
        let owner = crate::core::NodeId("function:main".into());
        let mut main = stale.functions()[&owner].clone();
        let keys = main
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { .. }
                )
            })
            .expect("Keys instruction");
        let crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { root, .. } =
            &mut keys.kind
        else {
            unreachable!("selected Keys instruction")
        };
        *root = crate::core::mir::MirValueId::new("local:forged_keys_root")
            .expect("well-formed forged MIR identity");
        stale.replace_function_for_test_only(main);
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_keys_snapshot_route(
                &checked,
                &stale,
                &receipt,
                String::new(),
            )
            .is_err()
        );

        let mut missing_witness = canonical;
        let mut main = missing_witness.functions()[&owner].clone();
        main.blocks.values_mut().for_each(|block| {
            block.instructions.retain(|instruction| {
                !matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack { .. }
                )
            });
        });
        missing_witness.replace_function_for_test_only(main);
        let resealed = missing_witness.route_receipt(profile.as_str());
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_keys_snapshot_route(
                &checked,
                &missing_witness,
                &resealed,
                String::new(),
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ordinary_map_owned_any_v2_map_root_get_any_caller_route_tests {
    use super::*;

    fn fixture() -> (String, crate::core::CheckedProgram) {
        let source = include_str!(
            "../../tests/fixtures/mir_ordinary_map_owned_any_v2_map_root_get_any_caller.mimi"
        )
        .to_owned();
        let file = parse_memory_source(&source, "verifier-v2-map-root-get-any-caller")
            .expect("parse Map-root GetAny caller fixture");
        let checked =
            crate::core::check_program(&file).expect("check Map-root GetAny caller fixture");
        (source, checked)
    }

    #[test]
    fn map_root_get_any_caller_verifier_requires_its_exact_checker_and_mir_receipts() {
        let (source, checked) = fixture();
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapRootGetAnyCaller
        );
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize exact Map-root GetAny caller profile");
        let profile =
            crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller;
        assert!(profile.is_admitted(route.admission));
        assert!(profile.is_materialized(&route));
        assert!(route.materialized_ordinary_map_owned_any_v2_map_root_get_any_caller_candidate);
        assert!(!route.materialized_ordinary_map_owned_any_v2_map_root_producer_caller_candidate);
        assert!(!route.materialized_ordinary_map_owned_any_v2_candidate);
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_map_root_get_any_caller_route_receipt
            .clone()
            .expect("the GetAny caller receives its own profile receipt");
        assert!(route
            .ordinary_map_owned_any_v2_map_root_producer_caller_route_receipt
            .is_none());
        let canonical = route.program;
        let receipt = canonical.route_receipt(profile.as_str());
        assert_eq!(receipt, materialized_receipt);
        crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
            &checked, &canonical,
        )
        .expect("Checker producer, GetAny unpack, and settlements bind to MIR");
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
            &checked, &canonical, &receipt,
        )
        .expect("the exact route passes strict capability validation");

        let wrong_receipt = canonical.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_PRODUCER_CALLER_PROFILE,
        );
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
                &checked,
                &canonical,
                String::new(),
                &wrong_receipt,
            )
            .is_err(),
            "the producer-only route receipt cannot authorize GetAny or String unpack"
        );

        let mut forged = canonical.clone();
        let (mut function, instruction_id) = forged
            .functions()
            .values()
            .find_map(|function| {
                function
                    .blocks
                    .values()
                    .flat_map(|block| &block.instructions)
                    .find(|instruction| {
                        matches!(
                            instruction.kind,
                            crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { .. }
                        )
                    })
                    .map(|instruction| (function.clone(), instruction.id.clone()))
            })
            .expect("one canonical checked String unpack");
        function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| instruction.id == instruction_id)
            .expect("same unpack instruction in its owner")
            .kind = crate::core::mir::MirInstructionKind::Nop;
        forged.replace_function_for_test_only(function);
        let resealed = forged.route_receipt(profile.as_str());
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
                &checked,
                &forged,
                String::new(),
                &resealed,
            )
            .is_err(),
            "resealing a changed unpack does not replace the Checker/MIR receipt proof"
        );

        let mut retargeted = canonical.clone();
        let caller_owner = crate::core::NodeId("function:direct_caller".into());
        let mut caller = retargeted.functions()[&caller_owner].clone();
        let root_call = caller
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::Call {
                        callee: crate::core::ir::ResolvedCallee::Function(target),
                        ..
                    } if target.0 == "function:make_root"
                )
            })
            .expect("direct caller's root-producing call");
        let crate::core::mir::MirInstructionKind::Call { callee, .. } = &mut root_call.kind else {
            unreachable!("the selected instruction is a direct call")
        };
        *callee =
            crate::core::ir::ResolvedCallee::Function(crate::core::NodeId("function:main".into()));
        retargeted.replace_function_for_test_only(caller);
        let retargeted_receipt = retargeted.route_receipt(profile.as_str());
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
                &checked,
                &retargeted,
                String::new(),
                &retargeted_receipt,
            )
            .is_err(),
            "retargeting the direct producer call invalidates the Checker/MIR bridge"
        );

        if !is_z3_available() {
            return;
        }
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
                &checked,
                &canonical,
                source_hash.clone(),
                &receipt,
            )
            .expect("the exact route reaches symbolic verification")
            .is_empty()
        );
        assert!(verify_checked(&checked, source_hash.clone())
            .expect("single checked verifier selects the narrow profile")
            .is_empty());
        assert!(verify_checked_dual(&checked, source_hash)
            .expect("dual checked verifier selects the narrow profile")
            .is_empty());
    }
}

#[cfg(test)]
mod ordinary_map_owned_any_v2_branch_local_set_diamond_route_tests {
    use super::*;

    const SOURCE: &str = r#"
        type Label { text: string }
        func main() -> i32 {
            let payload = Label { text: "shared" }
            let base = map_new()
            let flag = true
            if flag {
                let left = map_set(base, "left", payload)
                let left_size = map_size(left)
                drop(left)
            } else {
                let right = map_set(base, "right", payload)
                let right_size = map_size(right)
                drop(right)
            }
            drop(base)
            0
        }
    "#;

    fn checked_fixture() -> crate::core::CheckedProgram {
        let file = parse_memory_source(SOURCE, "verifier-owner-v2-branch-local-set-diamond")
            .expect("parse BranchLocalSetDiamond source");
        crate::core::check_program(&file).expect("check BranchLocalSetDiamond source")
    }

    #[test]
    fn branch_local_set_diamond_requires_its_exact_checker_route_and_rejects_forgery() {
        let checked = checked_fixture();
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::BranchLocalPersistentSetDiamond
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize the dedicated branch-local route");
        let profile =
            crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond;
        assert!(profile.is_admitted(route.admission));
        assert!(profile.is_materialized(&route));
        let canonical = route.program;
        let receipt = canonical.route_receipt(profile.as_str());
        crate::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
            &checked, &canonical,
        )
        .expect("Checker and MIR source-flow proofs agree");
        crate::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_mir_route(
            &canonical,
        )
        .expect("the MIR census is exactly New=1, Set=2, Size=2, Drop=3");
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
            &checked, &canonical, &receipt,
        )
        .expect("the exact Checker route passes the strict capability gate");
        assert!(
            validate_mir_capabilities(&canonical).is_ok(),
            "ordinary New/Set/Size/Drop retain their existing structural support"
        );
        let broad_receipt =
            canonical.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        assert!(
            verify_mir_with_route_receipt(&canonical, &broad_receipt, String::new()).is_err(),
            "raw MIR verification must not consume a BranchLocalSetDiamond under broad readiness"
        );
        let wrong_receipt = canonical.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE,
        );
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
                &checked,
                &canonical,
                &wrong_receipt,
                String::new(),
            )
            .is_err(),
            "a neighboring V2 profile cannot borrow this Checker proof"
        );
        let unrelated = crate::core::check_program(
            &parse_memory_source("func main() -> i32 { 0 }", "unrelated-branch-profile")
                .expect("parse unrelated source"),
        )
        .expect("check unrelated source");
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
                &unrelated,
                &canonical,
                &receipt,
                String::new(),
            )
            .is_err(),
            "a different CheckedProgram cannot authorize this MIR graph"
        );

        let mut forged = canonical.clone();
        let (mut function, instruction_id) = forged
            .functions()
            .values()
            .find_map(|function| {
                function
                    .blocks
                    .values()
                    .flat_map(|block| &block.instructions)
                    .find(|instruction| {
                        matches!(
                            instruction.kind,
                            crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Size { .. }
                        )
                    })
                    .map(|instruction| (function.clone(), instruction.id.clone()))
            })
            .expect("one branch-local Size instruction");
        function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| instruction.id == instruction_id)
            .expect("same Size instruction in its owner")
            .kind = crate::core::mir::MirInstructionKind::Nop;
        forged.replace_function_for_test_only(function);
        let resealed = forged.route_receipt(profile.as_str());
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
                &checked,
                &forged,
                &resealed,
            )
            .is_err(),
            "a fresh graph digest cannot replace the Checker/MIR operation proof"
        );
        assert!(
            verify_mir_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
                &checked,
                &forged,
                &resealed,
                String::new(),
            )
            .is_err(),
            "the dedicated verifier rejects a re-sealed operation mutation"
        );
    }

    #[test]
    fn branch_local_set_diamond_direct_verifier_accepts_exact_checker_and_mir_receipts() {
        let checked = checked_fixture();
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize the dedicated branch-local route");
        let profile =
            crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond;
        assert!(profile.is_admitted(route.admission));
        assert!(profile.is_materialized(&route));
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_branch_local_set_diamond_route_receipt
            .clone()
            .expect("the Checker-approved route materializes its exact receipt");
        let canonical = route.program;
        let mir_receipt = canonical.route_receipt(profile.as_str());
        assert_eq!(
            mir_receipt, materialized_receipt,
            "the exact MIR receipt must match the Checker-approved route receipt"
        );
        crate::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
            &checked, &canonical,
        )
        .expect("the Checker and canonical MIR receipts form the exact route");

        let results = verify_mir_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
            &checked,
            &canonical,
            &mir_receipt,
            String::new(),
        )
        .expect("the dedicated verifier accepts the exact Checker/MIR receipts");
        assert!(
            results.is_empty(),
            "the branch-local ownership fixture has no contract obligations"
        );
    }

    #[test]
    fn checked_single_and_dual_verifiers_keep_branch_local_diamond_narrow() {
        if !is_z3_available() {
            eprintln!("MIMI_BRANCH_LOCAL_SET_DIAMOND_VERIFIER z3_available=false outcome=skipped");
            return;
        }
        eprintln!("MIMI_BRANCH_LOCAL_SET_DIAMOND_VERIFIER z3_available=true outcome=executed");
        let checked = checked_fixture();
        assert!(verify_checked(&checked, String::new())
            .expect("single verifier consumes the dedicated route")
            .is_empty());
        assert!(verify_checked_dual(&checked, String::new())
            .expect("dual verifier consumes the dedicated route")
            .is_empty());
    }
}

#[cfg(test)]
mod values_local_index_i32_result_branch_route_tests {
    use super::*;

    fn source() -> String {
        source_with_ensures("result == 41 || result == 2 || result == 3")
    }

    fn source_with_ensures(ensures: &str) -> String {
        include_str!(
            "../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_local_index_i32_result_branch.mimi"
        )
        .replace(
            "func main() -> i32 {",
            &format!("func main() -> i32 {{\n    ensures: {ensures}"),
        )
    }

    fn check_source(source: &str) -> crate::core::CheckedProgram {
        let file = parse_memory_source(source, "verifier-values-local-index-i32-result-branch")
            .expect("parse Values Result-branch source");
        crate::core::check_program(&file).expect("check Values Result-branch source")
    }

    #[test]
    fn result_branch_route_binds_exact_checker_mir_and_backend_receipts() {
        let checked = check_source(&source());
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesLocalIndexI32ResultBranch
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };

        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("materialize the exact nested Result-branch Values route");
        let profile = crate::core::mir::CanonicalMirRouteProfile::
            OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranch;
        assert!(profile.is_admitted(route.admission));
        assert!(profile.is_materialized(&route));
        assert!(!route.materialized_ordinary_map_owned_any_v2_candidate);
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route_receipt
            .clone()
            .expect("dedicated Result-branch receipt");
        let canonical = route.program;
        let receipt = canonical.route_receipt(profile.as_str());
        assert_eq!(receipt, materialized_receipt);
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
            &checked,
            &canonical,
        )
        .expect("Checker and MIR bind the nested map_new/map_set Result branches");
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_mir_route(
            &canonical,
        )
        .expect("raw MIR independently replays the exact seven-block branch graph");
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
            &checked,
            &canonical,
            &receipt,
        )
        .expect("whole-program capability gate accepts exact branch ownership settlements");

        let results =
            verify_mir_with_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
                &checked,
                &canonical,
                &receipt,
                "values-local-index-i32-result-branch".into(),
            )
            .expect("dedicated verifier consumes the source-bound branch receipts");
        let proof = results
            .iter()
            .find(|result| result.func_name.ends_with("main"))
            .expect("main contract result");
        assert_eq!(
            proof.status,
            VerifStatus::Disproven,
            "unexpected Result-branch verifier outcome: {}",
            proof.message
        );
        assert!(proof.message.contains("can reach trap 'E0800'"));
        assert!(proof.constraint_count > 0);
        assert_ne!(proof.status, VerifStatus::NoObligations);
    }

    #[test]
    fn checked_result_branch_verifiers_keep_reachable_e0800_disproven() {
        if !is_z3_available() {
            eprintln!("MIMI_VALUES_LOCAL_INDEX_I32_RESULT_BRANCH_VERIFIER z3_available=false outcome=skipped");
            return;
        }
        let checked = check_source(&source());
        let single = verify_checked(
            &checked,
            "values-local-index-i32-result-branch-single".into(),
        )
        .expect("single verifier consumes the exact branch route");
        let proof = single
            .iter()
            .find(|result| result.func_name.ends_with("main"))
            .expect("single main contract result");
        assert_eq!(
            proof.status,
            VerifStatus::Disproven,
            "unexpected single-engine Result-branch verifier outcome: {}",
            proof.message
        );
        assert!(proof.message.contains("can reach trap 'E0800'"));
        assert!(proof.constraint_count > 0);

        let dual =
            verify_checked_dual(&checked, "values-local-index-i32-result-branch-dual".into())
                .expect("dual verifier preflights both branch consumers");
        let dual_proof = dual
            .iter()
            .find(|result| result.func_name.ends_with("main"))
            .expect("dual main contract result");
        assert_eq!(dual_proof.status, VerifStatus::Disproven);
        assert!(dual_proof.message.contains("can reach trap 'E0800'"));
        assert!(dual_proof.constraint_count > 0);
    }

    #[test]
    fn result_branch_near_miss_and_omitted_status_contracts_remain_closed() {
        let exact = check_source(&source());
        let route = crate::core::mir::materialize_canonical_mir_route(&exact, None)
            .expect("materialize exact branch route");
        let canonical = route.program;
        let wrong_profile = canonical.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE,
        );
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
                &exact,
                &canonical,
                &wrong_profile,
            )
            .is_err(),
            "the straightline route profile cannot authorize the Result-branch shape"
        );

        let nonzero_source = source().replace("let index: i32 = 0", "let index: i32 = 1");
        let nonzero_file =
            parse_memory_source(&nonzero_source, "verifier-values-local-index-i32-nonzero")
                .expect("parse nonzero-index Values Result-branch source");
        match crate::core::check_program(&nonzero_file) {
            Ok(nonzero) => {
                assert_ne!(
                    crate::core::mir::classify_canonical_mir_route_admission(&nonzero)
                        .ordinary_map_owned_any_v2,
                    crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesLocalIndexI32ResultBranch,
                    "a nonzero index cannot acquire the narrow branch route"
                );
                assert!(
                    crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
                        &nonzero,
                        &canonical,
                    )
                    .is_err(),
                    "the source-bound validator rejects a nonzero index against exact MIR"
                );
            }
            Err(diagnostics) => assert!(
                diagnostics.iter().any(|diagnostic| {
                    matches!(diagnostic.code.as_deref(), Some("E0200" | "E0218"))
                }),
                "a nonzero index must fail closed through the exact Values ownership receipt gate: {diagnostics:?}"
            ),
        }

        if !is_z3_available() {
            return;
        }
        for (name, ensures) in [
            ("omit-set-err-2", "result == 41 || result == 3"),
            ("omit-new-err-3", "result == 41 || result == 2"),
        ] {
            let checked = check_source(&source_with_ensures(ensures));
            let results = verify_checked(&checked, format!("values-local-index-i32-{name}"))
                .expect("the verifier preserves an explicit proof result");
            let proof = results
                .iter()
                .find(|result| result.func_name.ends_with("main"))
                .expect("omitted-status main contract result");
            assert_eq!(
                proof.status,
                VerifStatus::Disproven,
                "unexpected omitted-status Result-branch verifier outcome for {name}: {}",
                proof.message
            );
            assert!(proof.message.contains("can reach trap 'E0800'"));
            assert!(proof.constraint_count > 0);
        }
    }
}
