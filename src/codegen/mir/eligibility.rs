//! Native MIR admission and symbol eligibility.
//! This module owns only pre-emission admission plumbing; shape rules live in
//! the validator and TypeDesc ABI modules.

use super::*;
use crate::core::{ResolvedBlock, ResolvedExpr, ResolvedExprKind, ResolvedStmtKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeMirError {
    pub(super) subject: String,
    pub(super) message: String,
    pub(super) span: Span,
    /// Optional cross-layer diagnostic code.  Most native shape failures are
    /// ordinary backend diagnostics; route receipt failures additionally
    /// expose their stable code structurally instead of hiding it in text.
    pub(super) code: Option<&'static str>,
}

impl NativeMirError {
    pub(super) fn new(subject: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            message: message.into(),
            span: Span::UNKNOWN,
            code: None,
        }
    }

    pub(super) fn with_span(mut self, span: Span) -> Self {
        self.span = span;
        self
    }

    pub(super) fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    pub(super) fn diagnostic(self) -> Diagnostic {
        let message = format!(
            "canonical MIR native backend rejected {}: {}",
            self.subject, self.message
        );
        match self.code {
            Some(code) => Diagnostic::error_code(code, message, self.span).with_origin(
                crate::diagnostic::DiagnosticOrigin::runtime_system("mir.route"),
            ),
            None => Diagnostic::error(message, self.span),
        }
    }
}

fn reject_verifier_only_program(program: &MirProgram) -> Result<(), Vec<Diagnostic>> {
    if program.purpose() == crate::core::mir::reference::MirProgramPurpose::VerifierOnly {
        return Err(vec![NativeMirError::new(
            "mir-program",
            "verifier-only MIR cannot be validated or compiled by the native backend",
        )
        .diagnostic()]);
    }
    Ok(())
}

fn has_owned_get_result_branch_v3(program: &MirProgram) -> bool {
    program.functions().values().any(|function| {
        function
            .ownership
            .ordinary_map_owned_get_result_branch_v3_receipt()
            .is_some()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1NewResult {
                            ..
                        } | crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1SetResult {
                            ..
                        }
                    )
                })
            })
    })
}

fn reject_unbound_owned_get_result_branch_v3(program: &MirProgram) -> Result<(), Vec<Diagnostic>> {
    if has_owned_get_result_branch_v3(program) {
        return Err(vec![NativeMirError::new(
            "ordinary Map owned-Get ResultBranchV3",
            "fallible New/Set MIR requires the exact CheckedProgram-bound native route",
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    Ok(())
}

/// Validate a Canonical MIR program against the native shape contract without
/// creating LLVM declarations.  The CLI uses this as part of the atomic
/// default-route capability gate so run/build/verify make the same route
/// decision before any production backend starts.
pub fn validate_mir_native(program: &MirProgram) -> Result<(), Vec<Diagnostic>> {
    validate_mir_native_with_checked_get_bool(program, false, false)
}

/// Validate a CheckedGet MIR program only after its exact route receipt has
/// been tied to the same canonical graph. This is the sole native capability
/// entry that may admit Bool in the V2 Set/unpack TypeDesc checks.
pub fn validate_mir_native_checked_get_route(
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    let allowed_profiles = [
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
        crate::core::mir::MIR_NATIVE_DIRECT_ROUTE_PROFILE,
        crate::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
    ];
    if !allowed_profiles.contains(&receipt.profile.as_str()) {
        return Err(vec![NativeMirError::new(
            "CheckedGet route receipt",
            format!(
                "exact CheckedGet native profile required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "CheckedGet route receipt",
                format!(
                    "{}: CheckedGet native route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_checked_get_route(program).map_err(
        |message| {
            vec![NativeMirError::new(
                "CheckedGet route receipt",
                format!(
                    "{}: CheckedGet native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        },
    )?;
    validate_mir_native_with_checked_get_bool(program, true, false)
}

/// Validate DynamicKeysGet only with its exact, Checker-bound MIR route
/// receipt. This dedicated entry is used by default-route preflight and the
/// checked native emitter; ordinary capability validation stays closed.
pub fn validate_mir_native_dynamic_keys_get_route(
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    if receipt.profile != CheckedOrdinaryMapOwnedAnyV2Route::DynamicKeysGet.profile() {
        return Err(vec![NativeMirError::new(
            "DynamicKeysGet route receipt",
            format!(
                "exact DynamicKeysGet profile required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "DynamicKeysGet route receipt",
                format!(
                    "{}: DynamicKeysGet native receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_dynamic_keys_get_mir_route(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "DynamicKeysGet route receipt",
                format!(
                    "{}: DynamicKeysGet native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    validate_mir_native_with_checked_get_bool(program, false, true)
}

/// Validate the first executable direct tuple-Get L1 profile against its
/// Checker source receipt, exact Core route digest, executable MIR graph, and
/// native TypeDesc/branch shape. This is independent from composite i32 and
/// generic GetAny route authorization.
pub fn validate_checked_mir_native_ordinary_map_owned_any_v2_direct_tuple_get_unpack_l1_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let expected_profile = crate::core::mir::ORDINARY_MAP_DIRECT_TUPLE_GET_UNPACK_L1_CORE_PROFILE;
    if receipt.profile != expected_profile {
        return Err(vec![NativeMirError::new(
            "ordinary Map direct tuple Get L1 route receipt",
            format!(
                "exact direct tuple Get L1 profile required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    let expected =
        crate::core::mir::validate_ordinary_map_direct_tuple_get_unpack_l1_reference_route(
            checked, program,
        )
        .map_err(|message| {
            vec![NativeMirError::new(
                "ordinary Map direct tuple Get L1 CheckedProgram/MIR route",
                format!(
                    "{}: direct tuple Get L1 Checker/MIR replay failed: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    if !expected.same_semantic_identity(receipt) {
        return Err(vec![NativeMirError::new(
            "ordinary Map direct tuple Get L1 route receipt",
            "supplied route receipt differs from the exact Checker-derived executable L1 route digest",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    super::validate::validate_ordinary_map_direct_tuple_get_unpack_l1_native_route(
        checked, program, receipt,
    )
    .map_err(|error| vec![error.diagnostic()])?;
    validate_mir_native_with_direct_tuple_get_l1_authorization(program)
}

/// Validate the exact CheckedProgram/MIR computed String-key owner chain for
/// native preflight. The source route authorizes this one profile while the
/// global owner-v2 readiness flag remains closed.
pub fn validate_mir_native_computed_string_key_owner_chain_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let route = CheckedOrdinaryMapOwnedAnyV2Route::ComputedStringKeyOwnerChain;
    if receipt.profile != route.profile() {
        return Err(vec![NativeMirError::new(
            "ComputedStringKeyOwnerChain route receipt",
            format!(
                "exact ComputedStringKeyOwnerChain profile required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "ComputedStringKeyOwnerChain route receipt",
                format!(
                    "{}: ComputedStringKeyOwnerChain native receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    receipt.manifest_round_trip().map_err(|message| {
        vec![NativeMirError::new(
            "ComputedStringKeyOwnerChain route receipt",
            format!(
                "{}: ComputedStringKeyOwnerChain native receipt replay failed: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]
    })?;
    route.validate(checked, program).map_err(|message| {
        vec![NativeMirError::new(
            "ComputedStringKeyOwnerChain CheckedProgram route",
            format!(
                "{}: ComputedStringKeyOwnerChain CheckedProgram replay failed: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]
    })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_mir_route(
        program,
    )
    .map_err(|message| {
        vec![NativeMirError::new(
            "ComputedStringKeyOwnerChain route receipt",
            format!(
                "{}: ComputedStringKeyOwnerChain native MIR route rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]
    })?;
    super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_native_contract(
        checked,
        program,
    )
    .map_err(|messages| {
        vec![NativeMirError::new(
            "ComputedStringKeyOwnerChain native contract",
            messages.join("; "),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]
    })?;
    reject_verifier_only_program(program)?;
    if let Err(messages) = program.validate_ffi_source_census() {
        return Err(messages
            .into_iter()
            .map(|message| NativeMirError::new("FFI source census", message).diagnostic())
            .collect());
    }
    if let Err(messages) = validate_scalar_export_receipts(program) {
        return Err(messages
            .into_iter()
            .map(|message| NativeMirError::new("scalar export receipt", message).diagnostic())
            .collect());
    }
    NativeMirValidator::with_computed_string_key_owner_chain_authorization(program)
        .validate()
        .map_err(|errors| errors.into_iter().map(NativeMirError::diagnostic).collect())
}

/// Validate the exact CheckedProgram/MIR MapSetDropControl result-branch
/// profile before native emission. This profile owns its own source receipt
/// and route token; the legacy no-result MapSetDropControl adapter and the
/// computed-string owner-chain authorization remain independent.
pub fn validate_mir_native_map_set_drop_control_result_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let route = CheckedOrdinaryMapOwnedAnyV2Route::MapSetDropControlResult;
    if receipt.profile != route.profile() {
        return Err(vec![NativeMirError::new(
            route.label(),
            format!(
                "exact dedicated profile '{}' required, got '{}'",
                route.profile(),
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    receipt.manifest_round_trip().map_err(|message| {
        vec![NativeMirError::new(
            format!("{} route receipt", route.label()),
            format!(
                "{}: canonical route receipt replay failed: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]
    })?;
    route.validate(checked, program).map_err(|message| {
        vec![NativeMirError::new(
            format!("{} route receipt", route.label()),
            format!(
                "{}: exact Checker/MIR replay failed: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]
    })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_mir_route(
        program,
    )
    .map_err(|message| {
        vec![NativeMirError::new(
            format!("{} MIR route", route.label()),
            format!(
                "{}: native route rejected: {message}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]
    })?;
    super::validate::validate_ordinary_map_owned_any_v2_map_set_drop_control_result_l1_native_route(
        checked, program, receipt,
    )
    .map_err(|error| vec![error.diagnostic()])?;
    NativeMirValidator::with_map_set_drop_control_result_authorization(program)
        .validate()
        .map_err(|errors| errors.into_iter().map(NativeMirError::diagnostic).collect())
}

fn validate_mir_native_with_checked_get_bool(
    program: &MirProgram,
    checked_get_bool_authorized: bool,
    dynamic_keys_get_authorized: bool,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    if let Err(messages) = program.validate_ffi_source_census() {
        return Err(messages
            .into_iter()
            .map(|message| NativeMirError::new("FFI source census", message).diagnostic())
            .collect());
    }
    if let Err(messages) = validate_scalar_export_receipts(program) {
        return Err(messages
            .into_iter()
            .map(|message| NativeMirError::new("scalar export receipt", message).diagnostic())
            .collect());
    }
    let validator = if checked_get_bool_authorized {
        NativeMirValidator::with_checked_get_bool_authorization(program)
    } else if dynamic_keys_get_authorized {
        NativeMirValidator::with_dynamic_keys_get_authorization(program)
    } else {
        NativeMirValidator::new(program)
    };
    validator
        .validate()
        .map_err(|errors| errors.into_iter().map(NativeMirError::diagnostic).collect())
}

fn validate_mir_native_with_direct_tuple_get_l1_authorization(
    program: &MirProgram,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    if let Err(messages) = program.validate_ffi_source_census() {
        return Err(messages
            .into_iter()
            .map(|message| NativeMirError::new("FFI source census", message).diagnostic())
            .collect());
    }
    let validator = NativeMirValidator::with_direct_tuple_get_l1_authorization(program);
    validator
        .validate()
        .map_err(|errors| errors.into_iter().map(NativeMirError::diagnostic).collect())
}

fn validate_mir_native_with_direct_print_result_authorization(
    program: &MirProgram,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    if let Err(messages) = program.validate_ffi_source_census() {
        return Err(messages
            .into_iter()
            .map(|message| NativeMirError::new("FFI source census", message).diagnostic())
            .collect());
    }
    if let Err(messages) = validate_scalar_export_receipts(program) {
        return Err(messages
            .into_iter()
            .map(|message| NativeMirError::new("scalar export receipt", message).diagnostic())
            .collect());
    }
    NativeMirValidator::with_direct_print_result_authorization(program)
        .validate()
        .map_err(|errors| errors.into_iter().map(NativeMirError::diagnostic).collect())
}

/// Preflight the nested Result DirectPrint profile against the exact checked
/// source receipt, dedicated MIR route validator, and native L1 shape. This
/// profile never consults the older flat DirectPrint receipt or broad owner-v2
/// readiness state.
pub fn validate_checked_mir_native_ordinary_map_owned_any_v2_direct_print_result_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let expected_profile =
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrintResult.as_str();
    if receipt.profile != expected_profile {
        return Err(vec![NativeMirError::new(
            "ordinary Map nested Result DirectPrint route receipt",
            format!(
                "exact nested Result DirectPrint profile required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "ordinary Map nested Result DirectPrint route receipt",
                format!(
                    "{}: nested Result DirectPrint route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;

    let expected =
        crate::core::mir::reference::MirProgram::from_checked_program_ordinary_map_direct_print_result(
            checked,
        )
        .map_err(|reason| {
            vec![NativeMirError::new(
                "ordinary Map nested Result DirectPrint CheckedProgram",
                format!(
                    "{}: dedicated Checker receipt could not rebuild native MIR: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    let expected_receipt = expected.route_receipt(expected_profile);
    if expected.canonical_digest() != program.canonical_digest()
        || !expected_receipt.same_semantic_identity(receipt)
    {
        return Err(vec![NativeMirError::new(
            "ordinary Map nested Result DirectPrint route receipt",
            "native MIR differs from the exact nested Result DirectPrint graph rebuilt from CheckedProgram",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_direct_print_result_mir_route(program)
        .map_err(|reason| {
            vec![NativeMirError::new(
                "ordinary Map nested Result DirectPrint Core MIR route",
                format!(
                    "{}: dedicated nested Result DirectPrint MIR validator rejected: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    super::validate::validate_ordinary_map_direct_print_result_l1_native_route(program)
        .map_err(|error| vec![error.diagnostic()])?;
    validate_mir_native_with_direct_print_result_authorization(program)
}

/// Preflight the exact Checker-bound NewResult native slice without creating
/// LLVM declarations. Its dedicated validator admits only the canonical
/// one-function map_new/size/drop graph; the broad owner-v2 native gate stays
/// closed for every other Map/Any shape.
pub fn validate_checked_mir_native_ordinary_map_new_result_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    if receipt.profile != crate::core::mir::ORDINARY_MAP_NEW_RESULT_EXECUTION_PROFILE {
        return Err(vec![NativeMirError::new(
            "ordinary Map NewResult route receipt",
            format!(
                "exact NewResult native profile required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "ordinary Map NewResult route receipt",
                format!(
                    "{}: NewResult native route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let expected =
        crate::core::mir::materialize_ordinary_map_new_result_route(checked).map_err(|reason| {
            vec![NativeMirError::new(
                "ordinary Map NewResult CheckedProgram",
                format!(
                    "{}: NewResult source receipt could not rebuild its native graph: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    let expected_receipt = expected.route_receipt(receipt.profile.as_str());
    if expected.canonical_digest() != program.canonical_digest()
        || !expected_receipt.same_semantic_identity(receipt)
    {
        return Err(vec![NativeMirError::new(
            "ordinary Map NewResult route receipt",
            "native MIR differs from the exact NewResult graph rebuilt from CheckedProgram",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    crate::core::mir::validate_ordinary_map_new_result_l1_core_mir_route(program).map_err(
        |reason| {
            vec![NativeMirError::new(
                "ordinary Map NewResult native Core MIR route",
                format!(
                    "{}: NewResult native Core MIR validation failed: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        },
    )?;
    super::validate::validate_ordinary_map_new_result_l1_native_route(program)
        .map_err(|error| vec![error.diagnostic()])
}

/// Preflight the exact Checker-bound Map SetResult native slice without
/// creating LLVM declarations. Its independent SetLineage receipt and
/// rebuilt canonical MIR graph authorize the fallible Set instruction.
pub fn validate_checked_mir_native_ordinary_map_set_result_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    if receipt.profile != crate::core::mir::ORDINARY_MAP_SET_RESULT_EXECUTION_PROFILE {
        return Err(vec![NativeMirError::new(
            "ordinary Map SetResult route receipt",
            format!(
                "exact SetResult native profile required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "ordinary Map SetResult route receipt",
                format!(
                    "{}: SetResult native route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let expected =
        crate::core::mir::materialize_ordinary_map_set_result_route(checked).map_err(|reason| {
            vec![NativeMirError::new(
                "ordinary Map SetResult CheckedProgram",
                format!(
                    "{}: SetLineage source receipt could not rebuild its native graph: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    let expected_receipt = expected.route_receipt(receipt.profile.as_str());
    if expected.canonical_digest() != program.canonical_digest()
        || !expected_receipt.same_semantic_identity(receipt)
    {
        return Err(vec![NativeMirError::new(
            "ordinary Map SetResult route receipt",
            "native MIR differs from the exact SetLineage graph rebuilt from CheckedProgram",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    crate::core::mir::validate_ordinary_map_set_result_l1_core_mir_route(program).map_err(
        |reason| {
            vec![NativeMirError::new(
                "ordinary Map SetResult native Core MIR route",
                format!(
                    "{}: SetResult native Core MIR validation failed: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        },
    )?;
    super::validate::validate_ordinary_map_set_result_l1_native_route(program)
        .map_err(|error| vec![error.diagnostic()])
}

/// Preflight the exact Checker-bound New/Set/Get-i32 native slice without
/// creating LLVM declarations. Its own SetGet lineage receipt and rebuilt
/// canonical MIR graph authorize the borrowed typed lookup.
pub fn validate_checked_mir_native_ordinary_map_set_get_i32_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    if receipt.profile != crate::core::mir::ORDINARY_MAP_SET_GET_I32_EXECUTION_PROFILE {
        return Err(vec![NativeMirError::new(
            "ordinary Map Set/Get-i32 route receipt",
            format!(
                "exact Set/Get-i32 native profile required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "ordinary Map Set/Get-i32 route receipt",
                format!(
                    "{}: Set/Get-i32 native route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let expected = crate::core::mir::materialize_ordinary_map_set_get_i32_route(checked).map_err(
        |reason| {
            vec![NativeMirError::new(
                "ordinary Map Set/Get-i32 CheckedProgram",
                format!(
                    "{}: SetGet source receipt could not rebuild its native graph: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        },
    )?;
    let expected_receipt = expected.route_receipt(receipt.profile.as_str());
    if expected.canonical_digest() != program.canonical_digest()
        || !expected_receipt.same_semantic_identity(receipt)
    {
        return Err(vec![NativeMirError::new(
            "ordinary Map Set/Get-i32 route receipt",
            "native MIR differs from the exact SetGet graph rebuilt from CheckedProgram",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    crate::core::mir::validate_ordinary_map_set_get_i32_l1_core_mir_route(program).map_err(
        |reason| {
            vec![NativeMirError::new(
                "ordinary Map Set/Get-i32 native Core MIR route",
                format!(
                    "{}: Set/Get-i32 native Core MIR validation failed: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        },
    )?;
    super::validate::validate_ordinary_map_set_get_i32_l1_native_route(program)
        .map_err(|error| vec![error.diagnostic()])
}

/// Preflight the exact Checker-issued New/Set/GetAny/unpack-i32 graph without
/// creating LLVM declarations. This is a separate Result-aware route: its
/// authority comes only from the composite SetGetAny-i32 receipt and its
/// replayed Core MIR graph, never from typed Get-i32, CheckedGet, or a generic
/// owner-v2 receipt.
pub fn validate_checked_mir_native_ordinary_map_owned_any_v2_set_get_any_i32_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    if !crate::core::mir::has_ordinary_map_set_get_any_i32_route_candidate(checked) {
        return Err(vec![NativeMirError::new(
            "ordinary Map Set/GetAny-i32 CheckedProgram",
            format!(
                "{}: CheckedProgram has no exact composite Set/GetAny-i32 source receipt",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    let expected = crate::core::mir::materialize_ordinary_map_set_get_any_i32_route(checked)
        .map_err(|reason| {
            vec![NativeMirError::new(
                "ordinary Map Set/GetAny-i32 CheckedProgram",
                format!(
                    "{}: composite SetGetAny-i32 source receipt could not rebuild its native graph: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    let expected_receipt = expected.route_receipt(receipt.profile.as_str());
    if expected.canonical_digest() != program.canonical_digest()
        || !expected_receipt.same_semantic_identity(receipt)
    {
        return Err(vec![NativeMirError::new(
            "ordinary Map Set/GetAny-i32 route receipt",
            "native MIR differs from the exact SetGetAny-i32 graph rebuilt from CheckedProgram",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "ordinary Map Set/GetAny-i32 route receipt",
                format!(
                    "{}: Set/GetAny-i32 native route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    // Rebuild the source-bound GetAny/unpack result witness independently
    // against both the replayed expected graph and the caller-supplied graph.
    // Full receipt equality binds the payload-selection digest, selected
    // TypeDesc IDs/digests, key constants, unpack callsite, pair/root cleanup,
    // and the hit-versus-miss payload commitments.
    let expected_unpack_receipt = expected
        .ordinary_map_owned_any_v2_set_get_any_i32_unpack_result_receipt(checked)
        .map_err(|message| {
            vec![NativeMirError::new(
                "ordinary Map Set/GetAny-i32 expected unpack receipt",
                format!(
                    "{}: expected MIR could not replay the Checker-bound unpack result receipt: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?
        .ok_or_else(|| {
            vec![NativeMirError::new(
                "ordinary Map Set/GetAny-i32 expected unpack receipt",
                "replayed expected MIR has no Checker-bound GetAny/unpack result receipt",
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    expected_unpack_receipt
        .validate_payload_selection_commitment()
        .map_err(|message| {
            vec![NativeMirError::new(
                "ordinary Map Set/GetAny-i32 expected unpack receipt",
                format!(
                    "{}: expected payload-selection commitment is invalid: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let program_unpack_receipt = program
        .ordinary_map_owned_any_v2_set_get_any_i32_unpack_result_receipt(checked)
        .map_err(|message| {
            vec![NativeMirError::new(
                "ordinary Map Set/GetAny-i32 native unpack receipt",
                format!(
                    "{}: native MIR could not replay the Checker-bound unpack result receipt: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?
        .ok_or_else(|| {
            vec![NativeMirError::new(
                "ordinary Map Set/GetAny-i32 native unpack receipt",
                "native MIR has no Checker-bound GetAny/unpack result receipt",
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    program_unpack_receipt
        .validate_payload_selection_commitment()
        .map_err(|message| {
            vec![NativeMirError::new(
                "ordinary Map Set/GetAny-i32 native unpack receipt",
                format!(
                    "{}: native payload-selection commitment is invalid: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    if expected_unpack_receipt != program_unpack_receipt
        || &expected_unpack_receipt.route_receipt != receipt
        || &program_unpack_receipt.route_receipt != receipt
    {
        return Err(vec![NativeMirError::new(
            "ordinary Map Set/GetAny-i32 unpack receipt",
            "Checker-bound GetAny/unpack payload-selection receipt differs between replayed and native MIR, or from the supplied route receipt",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    // Only after exact CheckedProgram receipt replay and canonical graph
    // comparison do we authorize the dedicated Result-aware route profile.
    if receipt.profile
        != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_EXECUTION_PROFILE
    {
        return Err(vec![NativeMirError::new(
            "ordinary Map Set/GetAny-i32 route receipt",
            format!(
                "exact Set/GetAny-i32 native profile required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    crate::core::mir::validate_ordinary_map_set_get_any_i32_l1_core_mir_route(program).map_err(
        |reason| {
            vec![NativeMirError::new(
                "ordinary Map Set/GetAny-i32 native Core MIR route",
                format!(
                    "{}: Set/GetAny-i32 native Core MIR validation failed: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        },
    )?;
    super::validate::validate_ordinary_map_set_get_any_i32_l1_native_route(program)
        .map_err(|error| vec![error.diagnostic()])
}

/// Preflight the exact Checker-issued New/Set/Result-valued GetAny graph
/// without creating LLVM declarations. This route is independent from the
/// tuple-valued GetAny/unpack-i32 profile and must be rebuilt from its own
/// Checker receipt before the dedicated emitter can be authorized.
pub fn validate_checked_mir_native_ordinary_map_owned_any_v2_set_get_any_result_i32_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    if !crate::core::mir::has_ordinary_map_set_get_any_result_i32_route_candidate(checked) {
        return Err(vec![NativeMirError::new(
            "ordinary Map Set/GetAny-Result-i32 CheckedProgram",
            format!(
                "{}: CheckedProgram has no exact Result-valued MapGetAny source receipt",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    let expected = crate::core::mir::materialize_ordinary_map_set_get_any_result_i32_route(checked)
        .map_err(|reason| {
            vec![NativeMirError::new(
                "ordinary Map Set/GetAny-Result-i32 CheckedProgram",
                format!(
                    "{}: Result-valued MapGetAny receipt could not rebuild its native MIR graph: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    let expected_receipt = expected.route_receipt(receipt.profile.as_str());
    if expected.canonical_digest() != program.canonical_digest()
        || !expected_receipt.same_semantic_identity(receipt)
    {
        return Err(vec![NativeMirError::new(
            "ordinary Map Set/GetAny-Result-i32 route receipt",
            "native MIR differs from the exact Result-valued Set/GetAny graph rebuilt from CheckedProgram",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "ordinary Map Set/GetAny-Result-i32 route receipt",
                format!(
                    "{}: Result-valued Set/GetAny native route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    if receipt.profile
        != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_EXECUTION_PROFILE
    {
        return Err(vec![NativeMirError::new(
            "ordinary Map Set/GetAny-Result-i32 route receipt",
            format!(
                "exact Result-valued Set/GetAny native profile required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    crate::core::mir::validate_ordinary_map_set_get_any_result_i32_l1_core_mir_route(program)
        .map_err(|reason| {
            vec![NativeMirError::new(
                "ordinary Map Set/GetAny-Result-i32 native Core MIR route",
                format!(
                    "{}: Result-valued Set/GetAny Core MIR validation failed: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    super::validate::validate_ordinary_map_set_get_any_result_i32_l1_native_route(program)
        .map_err(|error| vec![error.diagnostic()])
}

/// Validate the exact Checker/MIR execution receipt for the independent
/// String-valued Set/Get profile before native declarations are created.
pub fn validate_checked_mir_native_ordinary_map_set_get_string_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    if receipt.profile != crate::core::mir::ORDINARY_MAP_SET_GET_STRING_EXECUTION_PROFILE {
        return Err(vec![NativeMirError::new(
            "ordinary Map Set/Get-String route receipt",
            format!(
                "exact Set/Get-String native profile required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "ordinary Map Set/Get-String route receipt",
                format!(
                    "{}: Set/Get-String native route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let expected = crate::core::mir::materialize_ordinary_map_set_get_string_route(checked)
        .map_err(|reason| {
            vec![NativeMirError::new(
                "ordinary Map Set/Get-String CheckedProgram",
                format!(
                    "{}: Set/Get-String source receipt could not rebuild its native graph: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    let expected_receipt = expected.route_receipt(receipt.profile.as_str());
    if expected.canonical_digest() != program.canonical_digest()
        || !expected_receipt.same_semantic_identity(receipt)
    {
        return Err(vec![NativeMirError::new(
            "ordinary Map Set/Get-String route receipt",
            "native MIR differs from the exact Set/Get-String graph rebuilt from CheckedProgram",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    crate::core::mir::validate_ordinary_map_set_get_string_l1_core_mir_route(program).map_err(
        |reason| {
            vec![NativeMirError::new(
                "ordinary Map Set/Get-String native Core MIR route",
                format!(
                    "{}: Set/Get-String native Core MIR validation failed: {reason}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        },
    )?;
    super::validate::validate_ordinary_map_set_get_string_l1_native_route(program)
        .map_err(|error| vec![error.diagnostic()])
}

/// Preflight the exact CheckedProgram/MIR Values producer-caller profile
/// without creating LLVM declarations. The CLI uses this same gate before it
/// commits to the native route, and the dedicated emitter entry reuses it so
/// admission never depends on the profile string alone.
pub fn validate_checked_mir_native_values_producer_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let route = CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller;
    if receipt.profile != route.profile() {
        return Err(vec![NativeMirError::new(
            format!("{} route receipt", route.label()),
            format!(
                "exact dedicated profile '{}' required, got '{}'",
                route.profile(),
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let checked_receipt =
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_route(
            checked, program,
        )
        .map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    if !checked_receipt.same_semantic_identity(receipt) {
        return Err(vec![NativeMirError::new(
            format!("{} route receipt", route.label()),
            "the supplied native receipt differs from the CheckedProgram/MIR route identity",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_values_producer_caller_native_contract(
        program,
    )
    .map_err(|messages| {
        vec![NativeMirError::new(
            "ValuesProducerCaller native contract",
            messages.join("; "),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]
    })?;
    validate_mir_native(program)
}

/// Preflight the exact Checker-receipted Values producer and String-unpacking
/// caller route before native code generation creates declarations.
pub fn validate_checked_mir_native_values_crosscall_string_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_PROFILE;
    if receipt.profile != profile {
        return Err(vec![NativeMirError::new(
            "ValuesCrosscallStringCaller route receipt",
            format!(
                "exact dedicated profile '{profile}' required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let checked_receipt =
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
            checked, program,
        )
        .map_err(|message| {
            vec![NativeMirError::new(
                "ValuesCrosscallStringCaller route receipt",
                format!(
                    "{}: Values cross-call String caller native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    if !checked_receipt.same_semantic_identity(receipt) {
        return Err(vec![NativeMirError::new(
            "ValuesCrosscallStringCaller route receipt",
            "the supplied native receipt differs from the CheckedProgram/MIR route identity",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_native_contract(
        program,
    )
    .map_err(|messages| {
        vec![NativeMirError::new(
            "ValuesCrosscallStringCaller native contract",
            messages.join("; "),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]
    })?;
    validate_mir_native(program)
}

/// Preflight the separately versioned, sorted two-entry/index-one Values
/// cross-call route. The V1 entry above remains bound to its index-zero
/// profile; this gate requires the dedicated Index1 route identity end to end.
pub fn validate_checked_mir_native_values_crosscall_string_caller_index1_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let profile = crate::core::mir::CanonicalMirRouteProfile::
        OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1
        .as_str();
    if receipt.profile != profile {
        return Err(vec![NativeMirError::new(
            "ValuesCrosscallStringCallerIndex1 route receipt",
            format!(
                "exact dedicated profile '{profile}' required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let checked_receipt = crate::core::mir::
        validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
            checked, program,
        )
        .map_err(|message| {
            vec![NativeMirError::new(
                "ValuesCrosscallStringCallerIndex1 route receipt",
                format!(
                    "{}: Values cross-call String index-one native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    if !checked_receipt.same_semantic_identity(receipt) {
        return Err(vec![NativeMirError::new(
            "ValuesCrosscallStringCallerIndex1 route receipt",
            "the supplied native receipt differs from the CheckedProgram/MIR index-one route identity",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    super::ordinary_map_owned_any_v2::
        validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_v2_native_contract(
            program,
        )
        .map_err(|messages| {
            vec![NativeMirError::new(
                "ValuesCrosscallStringCallerIndex1 native contract",
                messages.join("; "),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    validate_mir_native(program)
}

/// Preflight the additive Result-branch Values/String caller profile V3.
/// This remains a separate admission path from the straight-line V1 route.
pub fn validate_checked_mir_native_values_result_crosscall_string_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let profile = crate::core::mir::CanonicalMirRouteProfile::
        OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCaller
        .as_str();
    if receipt.profile != profile {
        return Err(vec![NativeMirError::new(
            "ValuesResultCrosscallStringCallerV3 route receipt",
            format!(
                "exact dedicated profile '{profile}' required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let checked_receipt = crate::core::mir::
        validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
            checked, program,
        )
        .map_err(|message| {
            vec![NativeMirError::new(
                "ValuesResultCrosscallStringCallerV3 route receipt",
                format!(
                    "{}: Result-branch Values String caller V3 native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    if checked_receipt.profile != profile || !checked_receipt.same_semantic_identity(receipt) {
        return Err(vec![NativeMirError::new(
            "ValuesResultCrosscallStringCallerV3 route receipt",
            "the supplied native receipt differs from the exact CheckedProgram/MIR V3 route identity",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    super::ordinary_map_owned_any_v2::
        validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_native_contract(
            program,
        )
        .map_err(|messages| {
            vec![NativeMirError::new(
                "ValuesResultCrosscallStringCallerV3 native contract",
                messages.join("; "),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    validate_mir_native(program)
}

/// Preflight the separately versioned Result-branch Values/String caller V4
/// index-one profile. It cannot be authorized by V3 or the straight-line V2
/// gate, even when the same MIR graph contains compatible operations.
pub fn validate_checked_mir_native_values_result_crosscall_string_caller_index1_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let profile = crate::core::mir::CanonicalMirRouteProfile::
        OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCallerIndex1
        .as_str();
    if receipt.profile != profile {
        return Err(vec![NativeMirError::new(
            "ValuesResultCrosscallStringCallerIndex1 route receipt",
            format!(
                "exact dedicated profile '{profile}' required, got '{}'",
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let checked_receipt = crate::core::mir::
        validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
            checked, program,
        )
        .map_err(|message| {
            vec![NativeMirError::new(
                "ValuesResultCrosscallStringCallerIndex1 route receipt",
                format!(
                    "{}: Result-branch Values String caller index-one native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    crate::core::mir::
        validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_mir_route(
            program,
        )
        .map_err(|message| {
            vec![NativeMirError::new(
                "ValuesResultCrosscallStringCallerIndex1 MIR route",
                format!(
                    "{}: Result-branch Values String index-one MIR route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    if checked_receipt.profile != profile || !checked_receipt.same_semantic_identity(receipt) {
        return Err(vec![NativeMirError::new(
            "ValuesResultCrosscallStringCallerIndex1 route receipt",
            "the supplied native receipt differs from the exact CheckedProgram/MIR index-one route identity",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    super::ordinary_map_owned_any_v2::
        validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_native_contract(
            program,
        )
        .map_err(|messages| {
            vec![NativeMirError::new(
                "ValuesResultCrosscallStringCallerIndex1 native contract",
                messages.join("; "),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    validate_mir_native(program)
}

/// Preflight the exact CheckedProgram/MIR Map-root producer-caller profile
/// without creating LLVM declarations. The root-return ABI is admitted only
/// through its own Checker and MIR receipt pair; a generic owner-v2 route
/// receipt cannot authorize it.
pub fn validate_checked_mir_native_map_root_producer_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let route = CheckedOrdinaryMapOwnedAnyV2Route::MapRootProducerCaller;
    if receipt.profile != route.profile() {
        return Err(vec![NativeMirError::new(
            format!("{} route receipt", route.label()),
            format!(
                "exact dedicated profile '{}' required, got '{}'",
                route.profile(),
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let checked_receipt =
        crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_route(
            checked, program,
        )
        .map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    if !checked_receipt.same_semantic_identity(receipt) {
        return Err(vec![NativeMirError::new(
            format!("{} route receipt", route.label()),
            "the supplied native receipt differs from the CheckedProgram/MIR route identity",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_map_root_producer_caller_native_contract(
        program,
    )
    .map_err(|messages| {
        vec![NativeMirError::new(
            "MapRootProducerCaller native contract",
            messages.join("; "),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]
    })?;
    validate_mir_native(program)
}

/// Preflight the exact CheckedProgram/MIR Map-root → GetAny → checked String
/// caller profile without creating LLVM declarations.  This profile owns a
/// distinct bridge receipt from the scalar Size caller and cannot borrow its
/// native authorization.
pub fn validate_checked_mir_native_map_root_get_any_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let route = CheckedOrdinaryMapOwnedAnyV2Route::MapRootGetAnyCaller;
    if receipt.profile != route.profile() {
        return Err(vec![NativeMirError::new(
            format!("{} route receipt", route.label()),
            format!(
                "exact dedicated profile '{}' required, got '{}'",
                route.profile(),
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let checked_receipt =
        crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
            checked, program,
        )
        .map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    if !checked_receipt.same_semantic_identity(receipt) {
        return Err(vec![NativeMirError::new(
            format!("{} route receipt", route.label()),
            "the supplied native receipt differs from the CheckedProgram/MIR route identity",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_native_contract(
        program,
    )
    .map_err(|messages| {
        vec![NativeMirError::new(
            "MapRootGetAnyCaller native contract",
            messages.join("; "),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]
    })?;
    validate_mir_native(program)
}

/// Validate the complete Checker-to-MIR profile receipt for the fused
/// `std::maps.get_or_default::<i32>` native consumer. The typed operation
/// receipt is additionally checked by the native contract before LLVM
/// declarations are created.
pub fn validate_checked_mir_native_get_or_default_i32_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let route = CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultI32;
    if receipt.profile != route.profile() {
        return Err(vec![NativeMirError::new(
            format!("{} route receipt", route.label()),
            format!(
                "exact dedicated profile '{}' required, got '{}'",
                route.profile(),
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let checked_receipt =
        crate::core::mir::validate_ordinary_map_owned_any_v2_get_or_default_i32_route(
            checked, program,
        )
        .map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    if !checked_receipt.same_semantic_identity(receipt) {
        return Err(vec![NativeMirError::new(
            format!("{} route receipt", route.label()),
            "the supplied native receipt differs from the complete CheckedProgram/MIR profile identity",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_get_or_default_i32_native_contract(
        program,
    )
    .map_err(|messages| {
        vec![NativeMirError::new(
            "GetOrDefault<i32> native contract",
            messages.join("; "),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]
    })?;
    validate_mir_native(program)
}

/// Validate the complete Checker-to-MIR profile receipt for the fused
/// `std::maps.get_or_default::<String>` native consumer. Its operation receipt
/// is separately checked by the native contract before LLVM declarations are
/// created.
pub fn validate_checked_mir_native_get_or_default_string_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<Diagnostic>> {
    reject_verifier_only_program(program)?;
    checked.validate_backend(crate::core::BackendProfile::Native)?;
    let route = CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultString;
    if receipt.profile != route.profile() {
        return Err(vec![NativeMirError::new(
            format!("{} route receipt", route.label()),
            format!(
                "exact dedicated profile '{}' required, got '{}'",
                route.profile(),
                receipt.profile
            ),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]);
    }
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
    let checked_receipt =
        crate::core::mir::validate_ordinary_map_owned_any_v2_get_or_default_string_route(
            checked, program,
        )
        .map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
    if !checked_receipt.same_semantic_identity(receipt) {
        return Err(vec![NativeMirError::new(
            format!("{} route receipt", route.label()),
            "the supplied native receipt differs from the complete CheckedProgram/MIR profile identity",
        )
        .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        .diagnostic()]);
    }
    super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_get_or_default_string_native_contract(
        program,
    )
    .map_err(|messages| {
        vec![NativeMirError::new(
            "GetOrDefault<String> native contract",
            messages.join("; "),
        )
        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        .diagnostic()]
    })?;
    validate_mir_native(program)
}

fn validate_scalar_export_receipts(program: &MirProgram) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    for (owner, declaration) in program.export_declarations() {
        let Some(function) = program.functions().get(owner) else {
            errors.push(format!("export owner '{}' is absent from MIR", owner.0));
            continue;
        };
        if declaration.owner != *owner
            || declaration.abi != "C"
            || declaration.public_symbol.trim().is_empty()
            || declaration.body_symbol != format!("$mimi.export.body.{}", declaration.public_symbol)
        {
            errors.push(format!(
                "export '{}' has an invalid symbol/ABI receipt",
                owner.0
            ));
        }
        if declaration.parameters.len() != function.parameters.len() {
            errors.push(format!(
                "export '{}' parameter receipt arity differs from MIR",
                owner.0
            ));
            continue;
        }
        for ((ty, kind), parameter) in declaration.parameters.iter().zip(&function.parameters) {
            let Some(value) = function.values.get(parameter) else {
                errors.push(format!("export '{}' parameter value is absent", owner.0));
                continue;
            };
            if ty != &value.ty || !export_kind_matches(program, ty, *kind, false) {
                errors.push(format!(
                    "export '{}' parameter ABI receipt is invalid",
                    owner.0
                ));
            }
        }
        if !export_kind_matches(program, &declaration.result.0, declaration.result.1, true)
            || declaration.result.0 != function.result
        {
            errors.push(format!(
                "export '{}' result ABI receipt is invalid",
                owner.0
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn export_kind_matches(
    program: &MirProgram,
    ty: &crate::core::ResolvedTypeId,
    kind: crate::core::ir::NativeExportScalarKind,
    result: bool,
) -> bool {
    let Some(descriptor) = program.type_catalog().get(ty) else {
        return false;
    };
    if kind.is_unit() {
        return result && descriptor.is_canonical_ffi_unit();
    }
    descriptor.is_canonical_ffi_scalar()
        && matches!(
            (kind, &descriptor.kind),
            (
                crate::core::ir::NativeExportScalarKind::I32,
                MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
            ) | (
                crate::core::ir::NativeExportScalarKind::I64,
                MirTypeKind::Primitive(crate::core::PrimitiveType::I64)
            ) | (
                crate::core::ir::NativeExportScalarKind::F32,
                MirTypeKind::Primitive(crate::core::PrimitiveType::F32)
            ) | (
                crate::core::ir::NativeExportScalarKind::F64,
                MirTypeKind::Primitive(crate::core::PrimitiveType::F64)
            ) | (
                crate::core::ir::NativeExportScalarKind::Bool,
                MirTypeKind::Primitive(crate::core::PrimitiveType::Bool)
            )
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckedOrdinaryMapOwnedAnyV2Route {
    EmptyRootRootQueriesV1,
    GetOrDefaultI32,
    GetOrDefaultString,
    ZeroMissI32,
    RemovedKeyCheckedMiss,
    CheckedGet,
    IsEmpty,
    RecordUnit,
    MapSetDropControl,
    MapSetDropControlResult,
    CloneIsolation,
    PersistentRootAliasReplacement,
    ComputedKeyGet,
    ComputedStringKeyOwnerChain,
    DynamicKeysGet,
    ValuesFieldMove,
    ValuesLocalIndexI32,
    ValuesLocalIndexI32ResultBranch,
    ValuesHeterogeneousStringIndex1,
    ValuesSnapshotReplacement,
    ValuesMixedSnapshot,
    ValuesMixedSnapshotRemove,
    KeysSnapshot,
    ValuesProducerCaller,
    ValuesCrosscallStringCaller,
    ValuesCrosscallStringCallerIndex1,
    ValuesResultCrosscallStringCaller,
    ValuesResultCrosscallStringCallerIndex1,
    MapRootProducerCaller,
    MapRootGetAnyCaller,
    BranchLocalSetDiamond,
}

impl CheckedOrdinaryMapOwnedAnyV2Route {
    const fn profile(self) -> &'static str {
        match self {
            Self::EmptyRootRootQueriesV1 => {
                crate::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE
            }
            Self::GetOrDefaultI32 => {
                crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2GetOrDefaultI32
                    .as_str()
            }
            Self::GetOrDefaultString => crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2GetOrDefaultString.as_str(),
            Self::ZeroMissI32 => crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE,
            Self::RemovedKeyCheckedMiss => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE
            }
            Self::CheckedGet => crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
            Self::IsEmpty => crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE,
            Self::RecordUnit => crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE,
            Self::MapSetDropControl => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE
            }
            Self::MapSetDropControlResult => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_RESULT_BRANCH_PROFILE
            }
            Self::CloneIsolation => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE
            }
            Self::PersistentRootAliasReplacement => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE
            }
            Self::ComputedKeyGet => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE
            }
            Self::ComputedStringKeyOwnerChain => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE
            }
            Self::DynamicKeysGet => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE
            }
            Self::ValuesFieldMove => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
            }
            Self::ValuesLocalIndexI32 => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE
            }
            Self::ValuesLocalIndexI32ResultBranch => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_RESULT_BRANCH_PROFILE
            }
            Self::ValuesHeterogeneousStringIndex1 => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE
            }
            Self::ValuesSnapshotReplacement => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE
            }
            Self::ValuesMixedSnapshot => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE
            }
            Self::ValuesMixedSnapshotRemove => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE
            }
            Self::KeysSnapshot => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE
            }
            Self::ValuesProducerCaller => crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesProducerCaller.as_str(),
            Self::ValuesCrosscallStringCaller => crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_PROFILE,
            Self::ValuesCrosscallStringCallerIndex1 => crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1.as_str(),
            Self::ValuesResultCrosscallStringCaller => crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCaller.as_str(),
            Self::ValuesResultCrosscallStringCallerIndex1 => crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCallerIndex1.as_str(),
            Self::MapRootProducerCaller => crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootProducerCaller.as_str(),
            Self::MapRootGetAnyCaller => crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller.as_str(),
            Self::BranchLocalSetDiamond => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE
            }
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::EmptyRootRootQueriesV1 => "MapEmptyRootRootQueriesV1",
            Self::GetOrDefaultI32 => "GetOrDefaultI32",
            Self::GetOrDefaultString => "GetOrDefaultString",
            Self::ZeroMissI32 => "ZeroMissI32",
            Self::RemovedKeyCheckedMiss => "RemovedKeyCheckedMiss",
            Self::CheckedGet => "CheckedGet",
            Self::IsEmpty => "IsEmpty",
            Self::RecordUnit => "RecordUnit",
            Self::MapSetDropControl => "MapSetDropControl",
            Self::MapSetDropControlResult => "MapSetDropControlResult",
            Self::CloneIsolation => "CloneIsolation",
            Self::PersistentRootAliasReplacement => "PersistentRootAliasReplacement",
            Self::ComputedKeyGet => "ComputedKeyGet",
            Self::ComputedStringKeyOwnerChain => "ComputedStringKeyOwnerChain",
            Self::DynamicKeysGet => "DynamicKeysGet",
            Self::ValuesFieldMove => "ValuesFieldMove",
            Self::ValuesLocalIndexI32 => "ValuesLocalIndexI32",
            Self::ValuesLocalIndexI32ResultBranch => "ValuesLocalIndexI32ResultBranch",
            Self::ValuesHeterogeneousStringIndex1 => "ValuesHeterogeneousStringIndex1",
            Self::ValuesSnapshotReplacement => "ValuesSnapshotReplacement",
            Self::ValuesMixedSnapshot => "ValuesMixedSnapshot",
            Self::ValuesMixedSnapshotRemove => "ValuesMixedSnapshotRemove",
            Self::KeysSnapshot => "KeysSnapshot",
            Self::ValuesProducerCaller => "ValuesProducerCaller",
            Self::ValuesCrosscallStringCaller => "ValuesCrosscallStringCaller",
            Self::ValuesCrosscallStringCallerIndex1 => "ValuesCrosscallStringCallerIndex1",
            Self::ValuesResultCrosscallStringCaller => "ValuesResultCrosscallStringCallerV3",
            Self::ValuesResultCrosscallStringCallerIndex1 => {
                "ValuesResultCrosscallStringCallerIndex1V4"
            }
            Self::MapRootProducerCaller => "MapRootProducerCaller",
            Self::MapRootGetAnyCaller => "MapRootGetAnyCaller",
            Self::BranchLocalSetDiamond => "BranchLocalSetDiamond",
        }
    }

    fn validate(
        self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
    ) -> Result<(), String> {
        match self {
            Self::EmptyRootRootQueriesV1 => crate::core::mir::validate_map_empty_root_root_queries_v1_route(checked, program).map(|_| ()),
            Self::GetOrDefaultI32 => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_get_or_default_i32_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::GetOrDefaultString => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_get_or_default_string_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::ZeroMissI32 => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_route(
                    checked, program,
                )
            }
            Self::RemovedKeyCheckedMiss => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::CheckedGet => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_checked_get_route(program)
            }
            Self::IsEmpty => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_is_empty_route(
                    checked, program,
                )
            }
            Self::RecordUnit => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_record_unit_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::MapSetDropControl => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_map_set_drop_control_route(
                    checked, program,
                )
            }
            Self::MapSetDropControlResult => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::CloneIsolation => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_clone_isolation_route(
                    checked, program,
                )
            }
            Self::PersistentRootAliasReplacement => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::ComputedKeyGet => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_computed_key_get_route(
                    checked, program,
                )
            }
            Self::ComputedStringKeyOwnerChain => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::DynamicKeysGet => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_dynamic_keys_get_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::ValuesFieldMove => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_field_move_route(
                    checked, program,
                )
            }
            Self::ValuesLocalIndexI32 => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::ValuesLocalIndexI32ResultBranch => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::ValuesHeterogeneousStringIndex1 => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::ValuesSnapshotReplacement => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_snapshot_replacement_route(
                    checked, program,
                )
            }
            Self::ValuesMixedSnapshot => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_route(
                    checked, program,
                )
            }
            Self::ValuesMixedSnapshotRemove => {
                // This exact profile owns a nested snapshot receipt plus the
                // later Remove receipt; the base mixed-snapshot MIR profile
                // has a different whole-program action census.
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route(
                    checked, program,
                )
            }
            Self::KeysSnapshot => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::ValuesProducerCaller => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::ValuesCrosscallStringCaller => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::ValuesCrosscallStringCallerIndex1 => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::ValuesResultCrosscallStringCaller
            | Self::ValuesResultCrosscallStringCallerIndex1 => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
                    checked, program,
                )
                .map(|receipt| {
                    if receipt.profile == self.profile() {
                        Ok(())
                    } else {
                        Err(format!(
                            "{} Checker route receipt profile mismatch: expected '{}', got '{}'",
                            self.label(),
                            self.profile(),
                            receipt.profile
                        ))
                    }
                })?
            }
            Self::MapRootProducerCaller => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::MapRootGetAnyCaller => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
                    checked, program,
                )
                .map(|_| ())
            }
            Self::BranchLocalSetDiamond => {
                crate::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
                    checked, program,
                )?;
                validate_branch_local_set_diamond_payload_selection(program)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckerRouteAuthorizationMismatch {
    RequiredButMissing(CheckedOrdinaryMapOwnedAnyV2Route),
    AuthorizationForDifferentRoute(CheckedOrdinaryMapOwnedAnyV2Route),
}

fn checker_route_authorization_mismatch(
    required: Option<CheckedOrdinaryMapOwnedAnyV2Route>,
    authorized: Option<CheckedOrdinaryMapOwnedAnyV2Route>,
) -> Option<CheckerRouteAuthorizationMismatch> {
    if let Some(required_profile) = required {
        if authorized != Some(required_profile) {
            return Some(CheckerRouteAuthorizationMismatch::RequiredButMissing(
                required_profile,
            ));
        }
    }
    if let Some(authorized_profile) = authorized {
        if required != Some(authorized_profile) {
            return Some(
                CheckerRouteAuthorizationMismatch::AuthorizationForDifferentRoute(
                    authorized_profile,
                ),
            );
        }
    }
    None
}

fn exactly_one_selected_type(
    selected_types: &[crate::core::mir::OrdinaryMapOwnedAnyV2SelectedType],
) -> Result<&crate::core::ir::ResolvedTypeId, String> {
    match selected_types {
        [selected] => Ok(&selected.type_id),
        [] => {
            Err("BranchLocalSetDiamond payload-selection receipt has no selected TypeDesc".into())
        }
        _ => Err(
            "BranchLocalSetDiamond payload-selection receipt must select exactly one TypeDesc"
                .into(),
        ),
    }
}

fn has_only_field_named(fields: &[(String, String)], expected_name: &str) -> bool {
    matches!(fields, [(name, _)] if name == expected_name)
}

fn has_single_equality_read_for_literal<'a>(
    reads: impl Iterator<Item = (bool, &'a str)>,
    expected_literal: &str,
) -> bool {
    let mut reads = reads;
    let Some((not_equal, peer_literal)) = reads.next() else {
        return false;
    };
    !not_equal && peer_literal == expected_literal && reads.next().is_none()
}

/// Keep this payload-selection check beside the Checker/MIR route admission.
/// The branch profile is authorized by exactly two persisted Sets of one
/// concrete, non-generic nominal record. The core receipt binds those entries
/// back to the private Checker actions, MIR operands, and TypeDesc catalog.
fn validate_branch_local_set_diamond_payload_selection(program: &MirProgram) -> Result<(), String> {
    let selection = program
        .ordinary_map_owned_any_v2_payload_selection_receipt()?
        .ok_or_else(|| {
            "BranchLocalSetDiamond has no Checker-owned payload-selection receipt".to_string()
        })?;
    selection.validate_against_program(program)?;
    if selection.entries.len() != 2 {
        return Err("BranchLocalSetDiamond requires exactly two Set entries".into());
    }
    let selected_type = exactly_one_selected_type(&selection.selected_types)?;
    if selection.entries.iter().any(|entry| {
        entry.owner.0 != "function:main"
            || &entry.checker_type != selected_type
            || &entry.mir_type != selected_type
    }) {
        return Err(
            "BranchLocalSetDiamond Set entries do not select the same main-callable TypeDesc"
                .into(),
        );
    }
    let catalog = program.type_catalog();
    let descriptor = catalog
        .get(selected_type)
        .ok_or_else(|| "BranchLocalSetDiamond selected TypeDesc is absent".to_string())?;
    let concrete_nominal_record = matches!(
        &descriptor.kind,
        crate::core::mir::types::MirTypeKind::Nominal
    ) && matches!(
        &descriptor.layout,
        crate::core::mir::types::MirLayout::Record { .. }
    ) && catalog
        .resolved_type_children(selected_type)
        .is_some_and(|children| children.is_empty())
        && descriptor
            .native_record_abi
            .as_ref()
            .is_some_and(|record| record.generic_arity == 0);
    if !concrete_nominal_record {
        return Err(
            "BranchLocalSetDiamond requires one concrete non-generic nominal record payload".into(),
        );
    }
    Ok(())
}

/// Compile a validated scalar/flat-aggregate MIR program directly to LLVM.
///
/// This is an explicit migration entry point. It is not used by the default
/// `build` path until the wider MIR shape and differential gates are closed.
impl<'ctx> CodeGenerator<'ctx> {
    pub fn compile_mir_native(&mut self, program: &MirProgram) -> Result<(), Vec<Diagnostic>> {
        self.compile_mir_native_inner(program, true, None, None)
    }

    /// Diagnostic component entry for L1/L2 tests. It still validates the
    /// complete MIR and runs the native backend, but deliberately does not
    /// claim that an owner-v2 candidate has crossed production readiness.
    #[cfg(test)]
    pub(crate) fn compile_mir_native_for_component(
        &mut self,
        program: &MirProgram,
    ) -> Result<(), Vec<Diagnostic>> {
        self.compile_mir_native_inner(program, false, None, None)
    }

    fn compile_mir_native_inner(
        &mut self,
        program: &MirProgram,
        enforce_production_readiness: bool,
        route_receipt: Option<&crate::core::mir::CanonicalMirRouteReceipt>,
        checker_authorized_route: Option<CheckedOrdinaryMapOwnedAnyV2Route>,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        reject_unbound_owned_get_result_branch_v3(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        let direct_print_result_profile =
            crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrintResult
                .as_str();
        let has_direct_print_result_signal = program.functions().values().any(|function| {
            function
                .ownership
                .ordinary_map_owned_any_v2_direct_print_result_receipt()
                .is_some()
        });
        if has_direct_print_result_signal
            || route_receipt.is_some_and(|receipt| receipt.profile == direct_print_result_profile)
        {
            return Err(vec![NativeMirError::new(
                "ordinary Map nested Result DirectPrint route receipt",
                "nested Result DirectPrint requires its dedicated Checker-bound native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        let observer_instruction_count = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson {
                        ..
                    } | crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln {
                        ..
                    }
                )
            })
            .count();
        let observer_profile =
            crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleObserverV1
                .as_str();
        if observer_instruction_count > 0
            || route_receipt.is_some_and(|receipt| receipt.profile == observer_profile)
        {
            let Some(receipt) = route_receipt else {
                return Err(vec![NativeMirError::new(
                    "OrdinaryMapOwnerLifecycleObserverV1 route receipt",
                    "lifecycle-v1 Map observer instructions require the selected observer route receipt",
                )
                .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                .diagnostic()]);
            };
            if receipt.profile != observer_profile {
                return Err(vec![NativeMirError::new(
                    "OrdinaryMapOwnerLifecycleObserverV1 route receipt",
                    format!(
                        "observer instructions require profile {observer_profile}, received {}",
                        receipt.profile
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                .diagnostic()]);
            }
            if let Err(message) = receipt.validate_against_program(program) {
                return Err(vec![NativeMirError::new(
                    "OrdinaryMapOwnerLifecycleObserverV1 route receipt",
                    format!(
                        "{}: observer route receipt does not bind the native MIR input: {message}",
                        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                .diagnostic()]);
            }
            let observer_projection =
                match crate::core::mir::validate_ordinary_map_owner_lifecycle_observer_v1_mir_route(
                    program,
                ) {
                    Ok(projection) => projection,
                    Err(message) => {
                        return Err(vec![NativeMirError::new(
                            "OrdinaryMapOwnerLifecycleObserverV1 route receipt",
                            format!(
                                "{}: observer native route rejected: {message}",
                                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                            ),
                        )
                        .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                        .diagnostic()]);
                    }
                };
            if observer_projection.is_empty()
                || observer_projection.len() != observer_instruction_count
            {
                return Err(vec![NativeMirError::new(
                    "OrdinaryMapOwnerLifecycleObserverV1 route receipt",
                    format!(
                        "observer projection contains {} Checker/MIR receipt(s) for {} native observer instruction(s); a selected profile must contain at least one exact observer",
                        observer_projection.len(),
                        observer_instruction_count
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                .diagnostic()]);
            }
        }
        let exact_checked_get_receipt = route_receipt.is_some_and(|receipt| {
            (receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE
                || receipt.profile == crate::core::mir::MIR_NATIVE_DIRECT_ROUTE_PROFILE
                || receipt.profile == crate::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE)
                && receipt.validate_against_program(program).is_ok()
                && crate::core::mir::validate_ordinary_map_owned_any_v2_checked_get_route(program)
                    .is_ok()
        });
        let direct_print_profile = route_receipt.is_some_and(|receipt| {
            receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE
        });
        let exact_direct_print_receipt = if direct_print_profile {
            match crate::core::mir::validate_ordinary_map_owned_any_v2_direct_print_mir_route(
                program,
            ) {
                Ok(()) => true,
                Err(message) => {
                    return Err(vec![NativeMirError::new(
                        "OrdinaryMapOwnedAnyV2 DirectPrint route receipt",
                        format!(
                            "{}: DirectPrint native route rejected: {message}",
                            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                    .diagnostic()]);
                }
            }
        } else {
            false
        };
        let exact_values_field_move_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesFieldMove)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
                    && receipt.validate_against_program(program).is_ok()
            });
        let exact_computed_key_get_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ComputedKeyGet)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::ComputedKeyGet.profile()
                    && receipt.validate_against_program(program).is_ok()
            });
        let exact_computed_string_key_owner_chain_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ComputedStringKeyOwnerChain)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::ComputedStringKeyOwnerChain.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_dynamic_keys_get_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::DynamicKeysGet)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::DynamicKeysGet.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_dynamic_keys_get_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_values_snapshot_replacement_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesSnapshotReplacement)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::ValuesSnapshotReplacement.profile()
                    && receipt.validate_against_program(program).is_ok()
            });
        let exact_values_local_index_i32_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_values_local_index_i32_result_branch_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32ResultBranch)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32ResultBranch.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_values_heterogeneous_string_index1_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesHeterogeneousStringIndex1)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::ValuesHeterogeneousStringIndex1.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_values_mixed_snapshot_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshot)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshot.profile()
                    && receipt.validate_against_program(program).is_ok()
            });
        let exact_values_mixed_snapshot_remove_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshotRemove)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshotRemove.profile()
                    && receipt.validate_against_program(program).is_ok()
            });
        let exact_keys_snapshot_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::KeysSnapshot)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::KeysSnapshot.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_values_producer_caller_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_values_crosscall_string_caller_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCaller)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCaller.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_values_crosscall_string_caller_index1_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCallerIndex1)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCallerIndex1
                        .profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_values_result_crosscall_string_caller_v3_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCaller)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCaller
                        .profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_values_result_crosscall_string_caller_v4_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCallerIndex1)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCallerIndex1
                        .profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_map_root_producer_caller_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::MapRootProducerCaller)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::MapRootProducerCaller.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_map_root_get_any_caller_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::MapRootGetAnyCaller)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::MapRootGetAnyCaller.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_get_or_default_i32_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultI32)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultI32.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && program
                        .ordinary_map_owned_any_v2_get_or_default_i32_receipts()
                        .is_ok_and(|receipts| receipts.len() == 1)
            });
        let exact_get_or_default_string_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultString)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultString.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && program
                        .ordinary_map_owned_any_v2_get_or_default_string_receipts()
                        .is_ok_and(|receipts| receipts.len() == 1)
            });
        let exact_empty_root_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::EmptyRootRootQueriesV1)
            && route_receipt.is_some_and(|r| {
                r.profile == crate::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE
                    && r.validate_against_program(program).is_ok()
            });
        let exact_is_empty_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::IsEmpty)
            && route_receipt.is_some_and(|r| {
                r.profile == CheckedOrdinaryMapOwnedAnyV2Route::IsEmpty.profile()
                    && r.validate_against_program(program).is_ok()
            });
        let exact_record_unit_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::RecordUnit)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::RecordUnit.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_record_unit_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_map_set_drop_control_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::MapSetDropControl)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::MapSetDropControl.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_map_set_drop_control_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_branch_local_set_diamond_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::BranchLocalSetDiamond)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::BranchLocalSetDiamond.profile()
                    && receipt.validate_against_program(program).is_ok()
            });
        let exact_clone_isolation_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::CloneIsolation)
            && route_receipt.is_some_and(|r| {
                r.profile == CheckedOrdinaryMapOwnedAnyV2Route::CloneIsolation.profile()
                    && r.validate_against_program(program).is_ok()
            });
        let exact_persistent_root_alias_replacement_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::PersistentRootAliasReplacement)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::PersistentRootAliasReplacement.profile()
                    && receipt.validate_against_program(program).is_ok()
            });
        let exact_zero_miss_i32_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::ZeroMissI32)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::ZeroMissI32.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_mir_route(
                        program,
                    )
                    .is_ok()
            });
        let exact_removed_key_checked_miss_receipt = checker_authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::RemovedKeyCheckedMiss)
            && route_receipt.is_some_and(|receipt| {
                receipt.profile
                    == CheckedOrdinaryMapOwnedAnyV2Route::RemovedKeyCheckedMiss.profile()
                    && receipt.validate_against_program(program).is_ok()
                    && crate::core::mir::validate_ordinary_map_owned_any_v2_removed_key_checked_miss_mir_route(
                        program,
                    )
                    .is_ok()
            });
        if enforce_production_readiness
            && !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY
            && !exact_zero_miss_i32_receipt
            && !exact_removed_key_checked_miss_receipt
            && !exact_empty_root_receipt
            && !exact_is_empty_receipt
            && !exact_record_unit_receipt
            && !exact_clone_isolation_receipt
            && !exact_persistent_root_alias_replacement_receipt
            && !exact_checked_get_receipt
            && !exact_direct_print_receipt
            && !exact_map_set_drop_control_receipt
            && !exact_branch_local_set_diamond_receipt
            && !exact_computed_key_get_receipt
            && !exact_computed_string_key_owner_chain_receipt
            && !exact_dynamic_keys_get_receipt
            && !exact_values_field_move_receipt
            && !exact_values_local_index_i32_receipt
            && !exact_values_local_index_i32_result_branch_receipt
            && !exact_values_heterogeneous_string_index1_receipt
            && !exact_values_snapshot_replacement_receipt
            && !exact_values_mixed_snapshot_receipt
            && !exact_values_mixed_snapshot_remove_receipt
            && !exact_keys_snapshot_receipt
            && !exact_values_producer_caller_receipt
            && !exact_values_crosscall_string_caller_receipt
            && !exact_values_crosscall_string_caller_index1_receipt
            && !exact_values_result_crosscall_string_caller_v3_receipt
            && !exact_values_result_crosscall_string_caller_v4_receipt
            && !exact_map_root_producer_caller_receipt
            && !exact_map_root_get_any_caller_receipt
            && !exact_get_or_default_i32_receipt
            && !exact_get_or_default_string_receipt
            && program.functions().values().any(|function| {
                function
                    .ownership
                    .has_checker_ordinary_map_owned_any_v2_actions()
            })
        {
            return Err(vec![NativeMirError::new(
                "mir-program",
                "ordinary Map/Any owner-v2 native emission is diagnostic-only while V2 production readiness is disabled",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                return Ok(());
            }
            return Err(vec![NativeMirError::new(
                "mir-program",
                "native generator already contains a different canonical MIR program",
            )
            .diagnostic()]);
        }
        validate_mir_native_with_checked_get_bool(
            program,
            exact_checked_get_receipt,
            exact_dynamic_keys_get_receipt,
        )?;

        NativeMirEmitter::new(self, program, exact_dynamic_keys_get_receipt)
            .compile()
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        Ok(())
    }

    /// Compile canonical MIR after checking the route receipt supplied by the
    /// caller. This keeps native emission on the exact immutable graph whose
    /// receipt admitted the route, matching the bytecode consumer boundary.
    pub fn compile_mir_native_with_route_receipt(
        &mut self,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.compile_mir_native_with_route_receipt_inner(program, receipt, None)
    }

    /// Values field-move and snapshot-replacement admissions include Checker
    /// source evidence that is unavailable at the raw MIR receipt boundary.
    /// Keep their authorized paths private to checked native entries.
    fn compile_mir_native_with_route_receipt_inner(
        &mut self,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        checker_authorized_route: Option<CheckedOrdinaryMapOwnedAnyV2Route>,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        reject_unbound_owned_get_result_branch_v3(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        receipt.manifest_round_trip().map_err(|message| {
            vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt manifest replay failed: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
        let checker_required_profile = if receipt.profile
            == crate::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::EmptyRootRootQueriesV1)
        } else if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultI32.profile() {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultI32)
        } else if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultString.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultString)
        } else if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::IsEmpty.profile() {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::IsEmpty)
        } else if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::RecordUnit.profile() {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::RecordUnit)
        } else if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::MapSetDropControl.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::MapSetDropControl)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::MapSetDropControlResult.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::MapSetDropControlResult)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::BranchLocalSetDiamond.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::BranchLocalSetDiamond)
        } else if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::CloneIsolation.profile() {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::CloneIsolation)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::PersistentRootAliasReplacement.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::PersistentRootAliasReplacement)
        } else if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::ZeroMissI32.profile() {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ZeroMissI32)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::RemovedKeyCheckedMiss.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::RemovedKeyCheckedMiss)
        } else if receipt.profile
            == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesFieldMove)
        } else if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::ComputedKeyGet.profile() {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ComputedKeyGet)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ComputedStringKeyOwnerChain.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ComputedStringKeyOwnerChain)
        } else if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::DynamicKeysGet.profile() {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::DynamicKeysGet)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesSnapshotReplacement.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesSnapshotReplacement)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32ResultBranch.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32ResultBranch)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesHeterogeneousStringIndex1.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesHeterogeneousStringIndex1)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshot.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshot)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshotRemove.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshotRemove)
        } else if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::KeysSnapshot.profile() {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::KeysSnapshot)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCaller.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCaller)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCallerIndex1.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCallerIndex1)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCaller.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCaller)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCallerIndex1.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCallerIndex1)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::MapRootProducerCaller.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::MapRootProducerCaller)
        } else if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::MapRootGetAnyCaller.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::MapRootGetAnyCaller)
        } else {
            None
        };
        if let Some(mismatch) =
            checker_route_authorization_mismatch(checker_required_profile, checker_authorized_route)
        {
            let (profile, message) = match mismatch {
                CheckerRouteAuthorizationMismatch::RequiredButMissing(profile) => (
                    profile,
                    format!(
                        "the dedicated {} native route requires CheckedProgram validation for its exact profile",
                        profile.label()
                    ),
                ),
                CheckerRouteAuthorizationMismatch::AuthorizationForDifferentRoute(profile) => (
                    profile,
                    format!(
                        "{} CheckedProgram validation cannot authorize a different route profile",
                        profile.label()
                    ),
                ),
            };
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", profile.label()),
                message,
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        let has_v2_owner_actions = program.functions().values().any(|function| {
            function
                .ownership
                .has_checker_ordinary_map_owned_any_v2_actions()
        });
        let checked_get_profile =
            receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE;
        let checked_get_validation = if checked_get_profile
            || (has_v2_owner_actions
                && (receipt.profile == crate::core::mir::MIR_NATIVE_DIRECT_ROUTE_PROFILE
                    || receipt.profile == crate::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE))
        {
            Some(crate::core::mir::validate_ordinary_map_owned_any_v2_checked_get_route(program))
        } else {
            None
        };
        if let Some(Err(message)) = checked_get_validation {
            if checked_get_profile {
                return Err(vec![NativeMirError::new(
                    "CheckedGet route receipt",
                    format!(
                        "{}: CheckedGet native route rejected: {message}",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                .diagnostic()]);
            }
        }
        // Preserve the primary single-program diagnostic when a caller hands
        // this generator a receipt for a different MIR graph.  The digest is
        // the graph identity; the route anchor is checked only after this
        // mismatch has been ruled out, so diagnostics cannot depend on which
        // provenance field happened to be tampered with first.
        if self
            .mir_native_compiled_digest
            .as_deref()
            .is_some_and(|compiled_digest| compiled_digest != program.canonical_digest())
        {
            return self.compile_mir_native(program);
        }
        if let Some(bound) = self.mir_native_route_receipt.as_ref() {
            if !bound.same_semantic_identity(receipt) {
                return Err(vec![NativeMirError::new(
                    "mir-program",
                    format!(
                        "{}: canonical route receipt disagrees with native program anchor",
                        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                .diagnostic()]);
            }
        }
        self.compile_mir_native_inner(program, true, Some(receipt), checker_authorized_route)?;
        if self.mir_native_route_receipt.is_none() {
            self.mir_native_route_receipt = Some(receipt.clone());
        }
        Ok(())
    }

    /// Crate-owned CLI route that couples shared-object preflight to the
    /// CheckedProgram used to materialize the MIR graph. Raw MIR entry points
    /// intentionally cannot authorize host-visible shared-object emission.
    pub fn compile_checked_mir_native_with_route_receipt(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        reject_unbound_owned_get_result_branch_v3(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        if receipt.profile == crate::core::mir::ORDINARY_MAP_NEW_RESULT_EXECUTION_PROFILE {
            return Err(vec![NativeMirError::new(
                "ordinary Map NewResult route receipt",
                "NewResult requires its dedicated Checker-bound native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile == crate::core::mir::ORDINARY_MAP_SET_RESULT_EXECUTION_PROFILE {
            return Err(vec![NativeMirError::new(
                "ordinary Map SetResult route receipt",
                "SetResult requires its dedicated Checker-bound native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile
            == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrintResult
                .as_str()
        {
            return Err(vec![NativeMirError::new(
                "ordinary Map nested Result DirectPrint route receipt",
                "nested Result DirectPrint requires its dedicated Checker-bound native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::ComputedKeyGet.profile() {
            return Err(vec![NativeMirError::new(
                "ComputedKeyGet route receipt",
                "ComputedKeyGet requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ComputedStringKeyOwnerChain.profile()
        {
            return Err(vec![NativeMirError::new(
                "ComputedStringKeyOwnerChain route receipt",
                "ComputedStringKeyOwnerChain requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::DynamicKeysGet.profile() {
            return Err(vec![NativeMirError::new(
                "DynamicKeysGet route receipt",
                "DynamicKeysGet requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller.profile() {
            return Err(vec![NativeMirError::new(
                "ValuesProducerCaller route receipt",
                "ValuesProducerCaller requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCaller.profile()
        {
            return Err(vec![NativeMirError::new(
                "ValuesCrosscallStringCaller route receipt",
                "ValuesCrosscallStringCaller requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCallerIndex1.profile()
        {
            return Err(vec![NativeMirError::new(
                "ValuesCrosscallStringCallerIndex1 route receipt",
                "ValuesCrosscallStringCallerIndex1 requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCaller.profile()
        {
            return Err(vec![NativeMirError::new(
                "ValuesResultCrosscallStringCallerV3 route receipt",
                "ValuesResultCrosscallStringCallerV3 requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCallerIndex1.profile()
        {
            return Err(vec![NativeMirError::new(
                "ValuesResultCrosscallStringCallerIndex1V4 route receipt",
                "ValuesResultCrosscallStringCallerIndex1V4 requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::MapRootProducerCaller.profile() {
            return Err(vec![NativeMirError::new(
                "MapRootProducerCaller route receipt",
                "MapRootProducerCaller requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32.profile() {
            return Err(vec![NativeMirError::new(
                "ValuesLocalIndexI32 route receipt",
                "ValuesLocalIndexI32 requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32ResultBranch.profile()
        {
            return Err(vec![NativeMirError::new(
                "ValuesLocalIndexI32ResultBranch route receipt",
                "ValuesLocalIndexI32ResultBranch requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::ValuesHeterogeneousStringIndex1.profile()
        {
            return Err(vec![NativeMirError::new(
                "ValuesHeterogeneousStringIndex1 route receipt",
                "ValuesHeterogeneousStringIndex1 requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::MapRootGetAnyCaller.profile() {
            return Err(vec![NativeMirError::new(
                "MapRootGetAnyCaller route receipt",
                "MapRootGetAnyCaller requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultI32.profile() {
            return Err(vec![NativeMirError::new(
                "GetOrDefaultI32 route receipt",
                "GetOrDefaultI32 requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultString.profile() {
            return Err(vec![NativeMirError::new(
                "GetOrDefaultString route receipt",
                "GetOrDefaultString requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::RemovedKeyCheckedMiss.profile() {
            return Err(vec![NativeMirError::new(
                "RemovedKeyCheckedMiss route receipt",
                "RemovedKeyCheckedMiss requires its dedicated CheckedProgram native route entry",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        let checker_authorized_route = if receipt.profile
            == CheckedOrdinaryMapOwnedAnyV2Route::CheckedGet.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::CheckedGet)
        } else if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultI32.profile() {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultI32)
        } else if receipt.profile == CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultString.profile()
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultString)
        } else if receipt.profile
            == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
        {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesFieldMove)
        } else {
            None
        };
        if let Some(authorized_route) = checker_authorized_route {
            if let Err(message) = receipt.validate_against_program(program) {
                return Err(vec![NativeMirError::new(
                    "mir-program",
                    format!(
                        "{}: canonical route receipt rejected: {message}",
                        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                .diagnostic()]);
            }
            if let Err(message) = authorized_route.validate(checked, program) {
                return Err(vec![NativeMirError::new(
                    format!("{} route receipt", authorized_route.label()),
                    format!(
                        "{}: {} native route rejected: {message}",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                        authorized_route.label()
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                .diagnostic()]);
            }
        }
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            // Only this CheckedProgram+canonical route entry can authorize
            // the checked owner receipt. Legacy/resolved emitters keep E0722.
            if receipt.validate_against_program(program).is_err()
                || checked_managed_any_unpack_coverage_for_route(
                    checked,
                    program,
                    checker_authorized_route,
                )
                .is_err()
            {
                return Err(original_error);
            }
        }
        let native_checker_authorized_route = match checker_authorized_route {
            Some(CheckedOrdinaryMapOwnedAnyV2Route::CheckedGet) => None,
            route => route,
        };
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            native_checker_authorized_route,
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact Checker-issued NewResult graph. This narrowly
    /// authorized native route does not change the production-readiness
    /// decision for the rest of ordinary Map/Any owner-v2.
    pub fn compile_checked_mir_native_with_ordinary_map_new_result_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_ordinary_map_new_result_route(checked, program, receipt)?;
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                if self
                    .mir_native_route_receipt
                    .as_ref()
                    .is_some_and(|bound| !bound.same_semantic_identity(receipt))
                {
                    return Err(vec![NativeMirError::new(
                        "ordinary Map NewResult route receipt",
                        format!(
                            "{}: NewResult route receipt disagrees with the native program anchor",
                            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                    .diagnostic()]);
                }
                return Ok(());
            }
            return self.compile_mir_native(program);
        }
        NativeMirEmitter::new(self, program, false)
            .compile_ordinary_map_new_result_l1()
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        self.mir_native_route_receipt = Some(receipt.clone());
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Compile the exact Checker-issued SetLineage graph. This has a
    /// dedicated authorization bit and cannot be opened by NewResult-only or
    /// broad owner-v2 route receipts.
    pub fn compile_checked_mir_native_with_ordinary_map_set_result_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_ordinary_map_set_result_route(checked, program, receipt)?;
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                if self
                    .mir_native_route_receipt
                    .as_ref()
                    .is_some_and(|bound| !bound.same_semantic_identity(receipt))
                {
                    return Err(vec![NativeMirError::new(
                        "ordinary Map SetResult route receipt",
                        format!(
                            "{}: SetResult route receipt disagrees with the native program anchor",
                            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                    .diagnostic()]);
                }
                return Ok(());
            }
            return self.compile_mir_native(program);
        }
        NativeMirEmitter::new(self, program, false)
            .compile_ordinary_map_set_result_l1()
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        self.mir_native_route_receipt = Some(receipt.clone());
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Compile fallible owned-get New/Set only when the exact ResultBranchV3
    /// Checker receipt, MIR projection and V1 execution route all agree.
    pub fn compile_checked_mir_native_with_ordinary_map_owned_get_result_branch_v3_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        super::validate::validate_ordinary_map_owned_get_result_branch_v3_native_route(
            checked, program, receipt,
        )
        .map_err(|error| vec![error.diagnostic()])?;
        validate_mir_native(program)?;
        self.preflight_native_dynamic_any_unpack(checked)?;

        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                if self
                    .mir_native_route_receipt
                    .as_ref()
                    .is_some_and(|bound| !bound.same_semantic_identity(receipt))
                {
                    return Err(vec![NativeMirError::new(
                        "ordinary Map owned-Get ResultBranchV3 route receipt",
                        "ResultBranchV3 route receipt disagrees with the native program anchor",
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                    .diagnostic()]);
                }
                return Ok(());
            }
            return self.compile_mir_native(program);
        }

        let mut emitter = NativeMirEmitter::new(self, program, false);
        emitter.ordinary_map_owned_get_result_branch_v3_authorized = true;
        emitter
            .compile()
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        self.mir_native_route_receipt = Some(receipt.clone());
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Compile the exact Checker-issued nested Result DirectPrint graph. This
    /// dedicated route cannot be authorized by the older flat DirectPrint
    /// receipt or by broad ordinary owner-v2 readiness.
    pub fn compile_checked_mir_native_with_ordinary_map_owned_any_v2_direct_print_result_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_ordinary_map_owned_any_v2_direct_print_result_route(
            checked, program, receipt,
        )?;
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                if self
                    .mir_native_route_receipt
                    .as_ref()
                    .is_some_and(|bound| !bound.same_semantic_identity(receipt))
                {
                    return Err(vec![NativeMirError::new(
                        "ordinary Map nested Result DirectPrint route receipt",
                        format!(
                            "{}: nested Result DirectPrint receipt disagrees with the native program anchor",
                            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                    .diagnostic()]);
                }
                return Ok(());
            }
            return self.compile_mir_native(program);
        }
        NativeMirEmitter::new(self, program, false)
            .compile_ordinary_map_direct_print_result_l1()
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        self.mir_native_route_receipt = Some(receipt.clone());
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Compile the exact Checker-issued New/Set/Get-i32 graph. This operation
    /// has a separate authorization bit and cannot be opened by NewResult,
    /// SetResult, GetAny, or V1 typed-Get receipts.
    pub fn compile_checked_mir_native_with_ordinary_map_set_get_i32_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_ordinary_map_set_get_i32_route(checked, program, receipt)?;
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                if self
                    .mir_native_route_receipt
                    .as_ref()
                    .is_some_and(|bound| !bound.same_semantic_identity(receipt))
                {
                    return Err(vec![NativeMirError::new(
                        "ordinary Map Set/Get-i32 route receipt",
                        format!(
                            "{}: Set/Get-i32 route receipt disagrees with the native program anchor",
                            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                    .diagnostic()]);
                }
                return Ok(());
            }
            return self.compile_mir_native(program);
        }
        NativeMirEmitter::new(self, program, false)
            .compile_ordinary_map_set_get_i32_l1()
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        self.mir_native_route_receipt = Some(receipt.clone());
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Compile the exact Checker-issued fallible New/Set/GetAny/unpack-i32
    /// graph. The dedicated emitter authorization is bound to the composite
    /// source receipt and rebuilt MIR graph; typed Get-i32, CheckedGet, and
    /// broad owner-v2 receipts cannot open this path.
    pub fn compile_checked_mir_native_with_ordinary_map_owned_any_v2_set_get_any_i32_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_ordinary_map_owned_any_v2_set_get_any_i32_route(
            checked, program, receipt,
        )?;
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                if self
                    .mir_native_route_receipt
                    .as_ref()
                    .is_some_and(|bound| !bound.same_semantic_identity(receipt))
                {
                    return Err(vec![NativeMirError::new(
                        "ordinary Map Set/GetAny-i32 route receipt",
                        format!(
                            "{}: Set/GetAny-i32 route receipt disagrees with the native program anchor",
                            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                    .diagnostic()]);
                }
                return Ok(());
            }
            return self.compile_mir_native(program);
        }
        NativeMirEmitter::new(self, program, false)
            .compile_ordinary_map_set_get_any_i32_l1()
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        self.mir_native_route_receipt = Some(receipt.clone());
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Compile the exact Checker-issued direct tuple Get L1 graph. This has
    /// its own authorization bit and consumes only the dedicated source/core
    /// receipt; it never opens the composite Set/GetAny-i32 profile.
    pub fn compile_checked_mir_native_with_ordinary_map_owned_any_v2_direct_tuple_get_unpack_l1_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_ordinary_map_owned_any_v2_direct_tuple_get_unpack_l1_route(
            checked, program, receipt,
        )?;
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest
                && self
                    .mir_native_route_receipt
                    .as_ref()
                    .is_some_and(|bound| bound.same_semantic_identity(receipt))
            {
                return Ok(());
            }
            return Err(vec![NativeMirError::new(
                "ordinary Map direct tuple Get L1 route receipt",
                "native generator already contains another MIR graph or route identity",
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        NativeMirEmitter::new(self, program, false)
            .compile_ordinary_map_direct_tuple_get_unpack_l1(checked, receipt)
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        self.mir_native_route_receipt = Some(receipt.clone());
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Compile the exact Checker-issued fallible New/Set/Result-valued
    /// GetAny graph. This authorization is bound to the independent
    /// MapGetAnyResult receipt and cannot fall back to generic MIR emission or
    /// the tuple-valued GetAny route.
    pub fn compile_checked_mir_native_with_ordinary_map_owned_any_v2_set_get_any_result_i32_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_ordinary_map_owned_any_v2_set_get_any_result_i32_route(
            checked, program, receipt,
        )?;
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                if self
                    .mir_native_route_receipt
                    .as_ref()
                    .is_none_or(|bound| !bound.same_semantic_identity(receipt))
                {
                    return Err(vec![NativeMirError::new(
                        "ordinary Map Set/GetAny-Result-i32 route receipt",
                        format!(
                            "{}: Set/GetAny-Result-i32 receipt differs from the already compiled native route",
                            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                    .diagnostic()]);
                }
                return Ok(());
            }
            return Err(vec![NativeMirError::new(
                "ordinary Map Set/GetAny-Result-i32 native route",
                format!(
                    "{}: another MIR graph is already compiled into this native module; generic fallback is not authorized",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        NativeMirEmitter::new(self, program, false)
            .compile_ordinary_map_set_get_any_result_i32_l1()
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        self.mir_native_route_receipt = Some(receipt.clone());
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Compile the exact Checker-issued New/Set<String>/Get<String> graph.
    /// This separate authorization binds the native adapter to the String
    /// lineage receipt and cannot be opened by the i32 or generic Set route.
    pub fn compile_checked_mir_native_with_ordinary_map_set_get_string_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_ordinary_map_set_get_string_route(checked, program, receipt)?;
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                if self
                    .mir_native_route_receipt
                    .as_ref()
                    .is_some_and(|bound| !bound.same_semantic_identity(receipt))
                {
                    return Err(vec![NativeMirError::new(
                        "ordinary Map Set/Get-String route receipt",
                        format!(
                            "{}: Set/Get-String route receipt disagrees with the native program anchor",
                            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                    .diagnostic()]);
                }
                return Ok(());
            }
            return self.compile_mir_native(program);
        }
        NativeMirEmitter::new(self, program, false)
            .compile_ordinary_map_set_get_string_l1()
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        self.mir_native_route_receipt = Some(receipt.clone());
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Compile the exact Checker/MIR computed-key GetAny profile. Computed
    /// keys have no static key digest, so only the dedicated Core validator
    /// over this CheckedProgram and MIR graph can authorize the native route.
    pub fn compile_checked_mir_native_with_computed_key_get_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::ComputedKeyGet;
        if receipt.profile != authorized_route.profile() {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{} CheckedProgram route requires profile '{}', got '{}'",
                    authorized_route.label(),
                    authorized_route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = authorized_route.validate(checked, program) {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    authorized_route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(
                checked,
                program,
                Some(authorized_route),
            )
            .is_err()
            {
                return Err(original_error);
            }
        }
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact Checker/MIR computed String-key owner chain. A
    /// generic route receipt cannot open owner-v2 native emission while the
    /// global readiness gate is closed.
    pub fn compile_checked_mir_native_with_computed_string_key_owner_chain_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::ComputedStringKeyOwnerChain;
        if receipt.profile != authorized_route.profile() {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{} CheckedProgram route requires profile '{}', got '{}'",
                    authorized_route.label(),
                    authorized_route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        receipt
            .validate_against_program(program)
            .map_err(|message| {
                vec![NativeMirError::new(
                    "mir-program",
                    format!(
                        "{}: canonical route receipt rejected: {message}",
                        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                .diagnostic()]
            })?;
        authorized_route
            .validate(checked, program)
            .map_err(|message| {
                vec![NativeMirError::new(
                    format!("{} route receipt", authorized_route.label()),
                    format!(
                        "{}: ComputedStringKeyOwnerChain native route rejected: {message}",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                .diagnostic()]
            })?;
        super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_native_contract(
            checked,
            program,
        )
        .map_err(|messages| {
            vec![NativeMirError::new(
                "ComputedStringKeyOwnerChain native contract",
                messages.join("; "),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
        receipt.manifest_round_trip().map_err(|message| {
            vec![NativeMirError::new(
                "ComputedStringKeyOwnerChain route receipt",
                format!(
                    "{}: ComputedStringKeyOwnerChain route manifest replay failed: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(
                checked,
                program,
                Some(authorized_route),
            )
            .is_err()
            {
                return Err(original_error);
            }
        }
        let mir_digest = program.canonical_digest();
        if self.mir_native_compiled_digest.as_deref() == Some(mir_digest.as_str()) {
            let Some(bound) = self.mir_native_route_receipt.as_ref() else {
                return Err(vec![NativeMirError::new(
                    "ComputedStringKeyOwnerChain route receipt",
                    format!(
                        "{}: native program digest has no route receipt anchor",
                        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                .diagnostic()]);
            };
            if !bound.same_semantic_identity(receipt) {
                return Err(vec![NativeMirError::new(
                    "ComputedStringKeyOwnerChain route receipt",
                    format!(
                        "{}: route receipt disagrees with the native program anchor",
                        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                .diagnostic()]);
            }
            return Ok(());
        }
        if self.mir_native_compiled_digest.is_some() {
            return Err(vec![NativeMirError::new(
                "ComputedStringKeyOwnerChain native emission",
                "this CodeGenerator already contains native MIR for a different canonical graph",
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        NativeMirEmitter::new(self, program, false)
            .compile_ordinary_map_computed_string_key_owner_chain_route(checked, receipt)
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        self.mir_native_route_receipt = Some(receipt.clone());
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Compile the exact CheckedProgram/MIR three-path MapSetDropControl
    /// result profile. Its native capability is independent from both the
    /// old no-result MapSetDropControl adapter and ComputedStringKeyOwnerChain.
    pub fn compile_checked_mir_native_with_map_set_drop_control_result_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        validate_mir_native_map_set_drop_control_result_route(checked, program, receipt)?;
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                let Some(bound) = self.mir_native_route_receipt.as_ref() else {
                    return Err(vec![NativeMirError::new(
                        "MapSetDropControlResult route receipt",
                        format!(
                            "{}: native program digest has no route receipt anchor",
                            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                    .diagnostic()]);
                };
                if !bound.same_semantic_identity(receipt) {
                    return Err(vec![NativeMirError::new(
                        "MapSetDropControlResult route receipt",
                        format!(
                            "{}: route receipt disagrees with the native program anchor",
                            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                    .diagnostic()]);
                }
                return Ok(());
            }
            return Err(vec![NativeMirError::new(
                "MapSetDropControlResult native emission",
                "this CodeGenerator already contains native MIR for a different canonical graph",
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        NativeMirEmitter::new(self, program, false)
            .compile_ordinary_map_set_drop_control_result_route(checked, receipt)
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        self.mir_native_route_receipt = Some(receipt.clone());
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Compile the exact Checker/MIR argv[0] → dynamic key → Keys/GetAny
    /// route. A raw or cross-profile route receipt cannot authorize its
    /// dedicated native opcode.
    pub fn compile_checked_mir_native_with_dynamic_keys_get_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::DynamicKeysGet;
        if receipt.profile != authorized_route.profile() {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{} CheckedProgram route requires profile '{}', got '{}'",
                    authorized_route.label(),
                    authorized_route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        receipt
            .validate_against_program(program)
            .map_err(|message| {
                vec![NativeMirError::new(
                    "mir-program",
                    format!(
                        "{}: canonical DynamicKeysGet route receipt rejected: {message}",
                        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                .diagnostic()]
            })?;
        authorized_route
            .validate(checked, program)
            .map_err(|message| {
                vec![NativeMirError::new(
                    format!("{} route receipt", authorized_route.label()),
                    format!(
                        "{}: DynamicKeysGet native route rejected: {message}",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                .diagnostic()]
            })?;
        super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_dynamic_keys_get_native_contract(
            program,
        )
        .map_err(|messages| {
            vec![NativeMirError::new(
                "DynamicKeysGet native contract",
                messages.join("; "),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(
                checked,
                program,
                Some(authorized_route),
            )
            .is_err()
            {
                return Err(original_error);
            }
        }
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact Checker/MIR Values snapshot-replacement profile.
    /// This has a separate checked authorization path so a generic receipt
    /// cannot mint its production-native exception.
    pub fn compile_checked_mir_native_with_values_snapshot_replacement_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::ValuesSnapshotReplacement;
        if receipt.profile != authorized_route.profile() {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{} CheckedProgram route requires profile '{}', got '{}'",
                    authorized_route.label(),
                    authorized_route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = authorized_route.validate(checked, program) {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    authorized_route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(
                checked,
                program,
                Some(authorized_route),
            )
            .is_err()
            {
                return Err(original_error);
            }
        }
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact immutable local `i32` index into one receipted
    /// AnyOwner-v2 Values snapshot. The Core route binds the Checker source
    /// initializer and local index to the MIR unpack; this native entry keeps
    /// that profile independent from the broader Values routes.
    pub fn compile_checked_mir_native_with_values_local_index_i32_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32;
        if receipt.profile != authorized_route.profile() {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{} CheckedProgram route requires profile '{}', got '{}'",
                    authorized_route.label(),
                    authorized_route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = authorized_route.validate(checked, program) {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    authorized_route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(messages) = super::ordinary_map_owned_any_v2::
            validate_ordinary_map_owned_any_v2_values_local_index_i32_native_contract(program)
        {
            return Err(vec![NativeMirError::new(
                "ValuesLocalIndexI32 native contract",
                messages.join("; "),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(
                checked,
                program,
                Some(authorized_route),
            )
            .is_err()
            {
                return Err(original_error);
            }
        }
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the separate nested Result Values profile. The Checker branch
    /// receipt, complete MIR path replay, and this native shape gate are each
    /// required; the straight-line Values authorization cannot be reused.
    pub fn compile_checked_mir_native_with_values_local_index_i32_result_branch_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32ResultBranch;
        if receipt.profile != authorized_route.profile() {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{} CheckedProgram route requires profile '{}', got '{}'",
                    authorized_route.label(),
                    authorized_route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        receipt
            .validate_against_program(program)
            .map_err(|message| {
                vec![NativeMirError::new(
                    "mir-program",
                    format!(
                        "{}: canonical route receipt rejected: {message}",
                        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                .diagnostic()]
            })?;
        authorized_route
            .validate(checked, program)
            .map_err(|message| {
                vec![NativeMirError::new(
                    format!("{} route receipt", authorized_route.label()),
                    format!(
                        "{}: {} native route rejected: {message}",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                        authorized_route.label()
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                .diagnostic()]
            })?;
        super::validate::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_l1_native_route(
            checked, program, receipt,
        )
        .map_err(|error| vec![error.diagnostic()])?;
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(
                checked,
                program,
                Some(authorized_route),
            )
            .is_err()
            {
                return Err(original_error);
            }
        }
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                let Some(bound) = self.mir_native_route_receipt.as_ref() else {
                    return Err(vec![NativeMirError::new(
                        "ValuesLocalIndexI32ResultBranch route receipt",
                        format!(
                            "{}: native program digest has no route receipt anchor",
                            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                    .diagnostic()]);
                };
                if !bound.same_semantic_identity(receipt) {
                    return Err(vec![NativeMirError::new(
                        "ValuesLocalIndexI32ResultBranch route receipt",
                        format!(
                            "{}: route receipt disagrees with the native program anchor",
                            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                    .diagnostic()]);
                }
                return Ok(());
            }
            return Err(vec![NativeMirError::new(
                "ValuesLocalIndexI32ResultBranch native emission",
                "this CodeGenerator already contains native MIR for a different canonical graph",
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        NativeMirEmitter::new(self, program, false)
            .compile_ordinary_map_values_local_index_i32_result_branch_route(checked, receipt)
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        self.mir_native_route_receipt = Some(receipt.clone());
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Compile only the exact two-entry heterogeneous Values snapshot whose
    /// UTF-8-sorted index one selects and transfers its String payload.
    pub fn compile_checked_mir_native_with_values_heterogeneous_string_index1_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::ValuesHeterogeneousStringIndex1;
        if receipt.profile != authorized_route.profile() {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{} CheckedProgram route requires profile '{}', got '{}'",
                    authorized_route.label(),
                    authorized_route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        receipt
            .validate_against_program(program)
            .map_err(|message| {
                vec![NativeMirError::new(
                    "mir-program",
                    format!(
                        "{}: canonical route receipt rejected: {message}",
                        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                .diagnostic()]
            })?;
        authorized_route
            .validate(checked, program)
            .map_err(|message| {
                vec![NativeMirError::new(
                    format!("{} route receipt", authorized_route.label()),
                    format!(
                        "{}: {} native route rejected: {message}",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                        authorized_route.label()
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                .diagnostic()]
            })?;
        super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_native_contract(
            program,
        )
        .map_err(|messages| {
            vec![NativeMirError::new(
                "ValuesHeterogeneousStringIndex1 native contract",
                messages.join("; "),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if let Err(coverage_error) = checked_managed_any_unpack_coverage_for_route(
                checked,
                program,
                Some(authorized_route),
            ) {
                return Err(original_error
                    .into_iter()
                    .map(|diagnostic| {
                        let span = diagnostic.span;
                        diagnostic.with_note(
                            format!(
                                "exact ValuesHeterogeneousStringIndex1 ManagedAny receipt coverage rejected the native route: {coverage_error}"
                            ),
                            span,
                        )
                    })
                    .collect());
            }
        }
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile only the exact Checker-owned sorted Keys snapshot and
    /// index-zero owned String copy. The CheckedProgram route receipt and
    /// independently rederived MIR witness must both agree before emission.
    pub fn compile_checked_mir_native_with_keys_snapshot_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::KeysSnapshot;
        if receipt.profile != authorized_route.profile() {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{} CheckedProgram route requires profile '{}', got '{}'",
                    authorized_route.label(),
                    authorized_route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = authorized_route.validate(checked, program) {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    authorized_route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(messages) = super::ordinary_map_owned_any_v2::
            validate_ordinary_map_owned_any_v2_keys_snapshot_native_contract(program)
        {
            return Err(vec![NativeMirError::new(
                "KeysSnapshot native contract",
                messages.join("; "),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact direct Values-producer/caller ownership transfer.
    /// The profile has a separately derived Checker-to-MIR route envelope;
    /// neither a matching profile string nor a raw MIR receipt can authorize
    /// the returned owner ABI.
    pub fn compile_checked_mir_native_with_values_producer_caller_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_values_producer_caller_route(checked, program, receipt)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller;
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(
                checked,
                program,
                Some(authorized_route),
            )
            .is_err()
            {
                return Err(original_error);
            }
        }
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact cross-call Values snapshot and String-unpack caller.
    /// The dedicated Checker/MIR receipt proves that the one reachable erased
    /// conversion is consumed by the native Values-copy adapter, with the
    /// caller's String and snapshot settled independently.
    pub fn compile_checked_mir_native_with_values_crosscall_string_caller_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_values_crosscall_string_caller_route(
            checked, program, receipt,
        )?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCaller;

        // The generic DynamicAny preflight rejects every erased unpack. This
        // exact route receipt is the narrowly scoped native proof that its
        // String unpack is copied and independently dropped, so this path
        // admits that operation without opening the generic preflight gate.
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the separately receipted two-entry Values snapshot and
    /// literal-index-one String caller. Its V2 Checker/MIR evidence and native
    /// contract are validated before the MIR emitter creates LLVM declarations.
    pub fn compile_checked_mir_native_with_values_crosscall_string_caller_index1_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_values_crosscall_string_caller_index1_route(
            checked, program, receipt,
        )?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCallerIndex1;

        // This V2 proof binds both sorted Values entries and the String owner
        // selected at index one. It admits only this route's erased unpack.
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact Result-branch Values producer and index-zero String
    /// caller profile V3. The independent Checker/MIR/native validation is
    /// completed before the emitter creates LLVM declarations.
    pub fn compile_checked_mir_native_with_values_result_crosscall_string_caller_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_values_result_crosscall_string_caller_route(
            checked, program, receipt,
        )?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCaller;
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the separately versioned Result-branch index-one String
    /// caller profile V4. V3 and straight-line caller receipts cannot
    /// authorize this profile.
    pub fn compile_checked_mir_native_with_values_result_crosscall_string_caller_index1_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_values_result_crosscall_string_caller_index1_route(
            checked, program, receipt,
        )?;
        let authorized_route =
            CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCallerIndex1;
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact persistent Map-root producer and its unique direct
    /// caller. The dedicated Checker/MIR receipt binds the returned owner ABI,
    /// caller observation, and final settlement before native emission.
    pub fn compile_checked_mir_native_with_map_root_producer_caller_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_map_root_producer_caller_route(checked, program, receipt)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::MapRootProducerCaller;
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact Map-root → GetAny → checked String transfer. This
    /// route has its own Checker/MIR witness for the returned root, GetAny
    /// token, unpacked String, and all three explicit Drops.
    pub fn compile_checked_mir_native_with_map_root_get_any_caller_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_map_root_get_any_caller_route(checked, program, receipt)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::MapRootGetAnyCaller;
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(
                checked,
                program,
                Some(authorized_route),
            )
            .is_err()
            {
                return Err(original_error);
            }
        }
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the one exact `std::maps.get_or_default::<i32>` callsite.
    /// Its complete Checker/MIR profile receipt authorizes the native
    /// GetAny, hit-only typed unpack, default selection, and pair settlement.
    pub fn compile_checked_mir_native_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_get_or_default_i32_route(checked, program, receipt)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultI32;
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the one exact `std::maps.get_or_default::<String>` callsite.
    /// Its dedicated CheckedProgram/MIR proof authorizes String payload
    /// copying, default ownership settlement, and miss-sentinel disposal.
    pub fn compile_checked_mir_native_with_ordinary_map_owned_any_v2_get_or_default_string_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        validate_checked_mir_native_get_or_default_string_route(checked, program, receipt)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultString;
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact Checker/MIR mixed Values snapshot profile.  The
    /// profile has its own source and MIR receipt and cannot borrow the broad
    /// owner-v2 readiness exception or another Values subprofile's authority.
    pub fn compile_checked_mir_native_with_values_mixed_snapshot_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshot;
        if receipt.profile != authorized_route.profile() {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{} CheckedProgram route requires profile '{}', got '{}'",
                    authorized_route.label(),
                    authorized_route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = authorized_route.validate(checked, program) {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    authorized_route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        // The checked route validator binds all three literal-index unpacks
        // to the immutable mixed snapshot.  Keep the native consumer's MIR
        // shape validation adjacent to that proof before reusing the common
        // ValuesUnpack emitter.
        if let Err(messages) = super::ordinary_map_owned_any_v2::
            validate_ordinary_map_owned_any_v2_values_mixed_snapshot_native_contract(program)
        {
            return Err(vec![NativeMirError::new(
                "ValuesMixedSnapshot native contract",
                messages.join("; "),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(
                checked,
                program,
                Some(authorized_route),
            )
            .is_err()
            {
                return Err(original_error);
            }
        }
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact Checker/MIR mixed Values snapshot with its separately
    /// receipted Remove operation. The dedicated profile carries its own
    /// Checker and MIR authority; neither raw receipts nor the mixed-snapshot
    /// profile alone can authorize the additional root update.
    pub fn compile_checked_mir_native_with_values_mixed_snapshot_remove_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let authorized_route = CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshotRemove;
        if receipt.profile != authorized_route.profile() {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{} CheckedProgram route requires profile '{}', got '{}'",
                    authorized_route.label(),
                    authorized_route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = authorized_route.validate(checked, program) {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", authorized_route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    authorized_route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(messages) = super::ordinary_map_owned_any_v2::
            validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_native_contract(
                program,
            )
        {
            return Err(vec![NativeMirError::new(
                "ValuesMixedSnapshotRemove native contract",
                messages.join("; "),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(
                checked,
                program,
                Some(authorized_route),
            )
            .is_err()
            {
                return Err(original_error);
            }
        }
        let result = self.compile_mir_native_with_route_receipt_inner(
            program,
            receipt,
            Some(authorized_route),
        );
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Only the exact source-bound profile can cross the broad readiness gate.
    pub fn compile_checked_mir_native_with_empty_root_root_queries_v1_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let route = CheckedOrdinaryMapOwnedAnyV2Route::EmptyRootRootQueriesV1;
        if receipt.profile != route.profile() {
            return Err(vec![NativeMirError::new(
                "MapEmptyRootRootQueriesV1",
                "exact dedicated profile required",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        route.validate(checked, program).map_err(|e| {
            vec![NativeMirError::new("MapEmptyRootRootQueriesV1", e)
                .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                .diagnostic()]
        })?;
        self.compile_mir_native_with_route_receipt_inner(program, receipt, Some(route))
    }

    /// Compile the exact Checker/MIR IsEmpty profile. Its Core validator
    /// binds the source evidence to this CheckedProgram and the route receipt
    /// binds native emission to the complete canonical MIR graph. The shared
    /// emitter lowers IsEmpty through its status/out runtime ABI.
    pub fn compile_checked_mir_native_with_is_empty_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let route = CheckedOrdinaryMapOwnedAnyV2Route::IsEmpty;
        if receipt.profile != route.profile() {
            return Err(vec![NativeMirError::new(
                route.label(),
                format!(
                    "exact dedicated profile '{}' required, got '{}'",
                    route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        route.validate(checked, program).map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
        self.preflight_native_dynamic_any_unpack(checked)?;

        // This consumes the complete MIR graph through NativeMirEmitter; the
        // IsEmpty instruction arm calls emit_ordinary_map_owned_any_v2_is_empty
        // and therefore uses the established status/out runtime ABI.
        let result =
            self.compile_mir_native_with_route_receipt_inner(program, receipt, Some(route));
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact mixed-payload RecordUnit profile through the native
    /// adapter. The Checker/MIR validator binds Label layout and aggregate
    /// glue before the ordinary native runtime path can be selected.
    pub fn compile_checked_mir_native_with_record_unit_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let route = CheckedOrdinaryMapOwnedAnyV2Route::RecordUnit;
        if receipt.profile != route.profile() {
            return Err(vec![NativeMirError::new(
                route.label(),
                format!(
                    "exact dedicated profile '{}' required, got '{}'",
                    route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        receipt
            .validate_against_program(program)
            .map_err(|message| {
                vec![NativeMirError::new(
                    "mir-program",
                    format!(
                        "{}: canonical route receipt rejected: {message}",
                        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                .diagnostic()]
            })?;
        route.validate(checked, program).map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
        self.preflight_native_dynamic_any_unpack(checked)?;
        let result =
            self.compile_mir_native_with_route_receipt_inner(program, receipt, Some(route));
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact Checker/MIR no-query control profile. Its private
    /// route token stays separate from IsEmpty even though both graphs carry
    /// the same selected payload TypeDesc.
    pub fn compile_checked_mir_native_with_map_set_drop_control_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let route = CheckedOrdinaryMapOwnedAnyV2Route::MapSetDropControl;
        if receipt.profile != route.profile() {
            return Err(vec![NativeMirError::new(
                route.label(),
                format!(
                    "exact dedicated profile '{}' required, got '{}'",
                    route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        route.validate(checked, program).map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
        self.compile_mir_native_with_route_receipt_inner(program, receipt, Some(route))
    }

    /// Compile the exact Checker/MIR branch-local persistent Set diamond.
    /// This route owns its arm-local CFG proof and two-entry concrete nominal
    /// payload-selection receipt; it cannot borrow MapSetDropControl's
    /// authority or the broad owner-v2 readiness flag.
    pub fn compile_checked_mir_native_with_branch_local_set_diamond_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let route = CheckedOrdinaryMapOwnedAnyV2Route::BranchLocalSetDiamond;
        if receipt.profile != route.profile() {
            return Err(vec![NativeMirError::new(
                route.label(),
                format!(
                    "exact dedicated profile '{}' required, got '{}'",
                    route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        route.validate(checked, program).map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
        self.compile_mir_native_with_route_receipt_inner(program, receipt, Some(route))
    }

    /// Compile the exact Checker/MIR CloneIsolation profile. Its Core
    /// validator binds the source evidence to this CheckedProgram and MIR
    /// graph; only its exact route receipt can authorize native emission.
    pub fn compile_checked_mir_native_with_clone_isolation_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let route = CheckedOrdinaryMapOwnedAnyV2Route::CloneIsolation;
        if receipt.profile != route.profile() {
            return Err(vec![NativeMirError::new(
                route.label(),
                format!(
                    "exact dedicated profile '{}' required, got '{}'",
                    route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        route.validate(checked, program).map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(checked, program, Some(route)).is_err()
            {
                return Err(original_error);
            }
        }

        // The route validator proves the cloned and source roots' ownership
        // separation; this call still consumes the complete, receipted MIR
        // through the shared native emitter.
        let result =
            self.compile_mir_native_with_route_receipt_inner(program, receipt, Some(route));
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact Checker-sealed PersistentRootAliasReplacement
    /// profile. Its route proof binds the persistent root alias, replacement
    /// root, and selected payload observations to this CheckedProgram and MIR
    /// graph; the broad owner-v2 readiness gate remains closed.
    pub fn compile_checked_mir_native_with_persistent_root_alias_replacement_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let route = CheckedOrdinaryMapOwnedAnyV2Route::PersistentRootAliasReplacement;
        if receipt.profile != route.profile() {
            return Err(vec![NativeMirError::new(
                route.label(),
                format!(
                    "exact dedicated profile '{}' required, got '{}'",
                    route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        route.validate(checked, program).map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
        let canonical_receipt = program.route_receipt(route.profile());
        if !canonical_receipt.same_semantic_identity(receipt) {
            return Err(vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                "the supplied native receipt differs from the exact Checker-bound profile receipt",
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(checked, program, Some(route)).is_err()
            {
                return Err(original_error);
            }
        }

        let result =
            self.compile_mir_native_with_route_receipt_inner(program, receipt, Some(route));
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact Checker/MIR ZeroMissI32 profile. This authorization
    /// is independent of broad V2 readiness and binds native emission to the
    /// dedicated receipt over the exact MIR graph, including the miss result's
    /// fresh owned canonical i32 zero.
    pub fn compile_checked_mir_native_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let route = CheckedOrdinaryMapOwnedAnyV2Route::ZeroMissI32;
        if receipt.profile != route.profile() {
            return Err(vec![NativeMirError::new(
                route.label(),
                format!(
                    "exact dedicated profile '{}' required, got '{}'",
                    route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        route.validate(checked, program).map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
        crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_mir_route(program)
            .map_err(|message| {
                vec![NativeMirError::new(
                    format!("{} MIR route receipt", route.label()),
                    format!(
                        "{}: {} native MIR route rejected: {message}",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                        route.label()
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                .diagnostic()]
            })?;
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(checked, program, Some(route)).is_err()
            {
                return Err(original_error);
            }
        }
        let result =
            self.compile_mir_native_with_route_receipt_inner(program, receipt, Some(route));
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile the exact Checker/MIR persistent-remove checked-miss profile.
    /// The dedicated receipt joins the queried root to Remove's successor and
    /// proves the canonical i32 miss before this route can bypass closed V2
    /// production readiness.
    pub fn compile_checked_mir_native_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        let route = CheckedOrdinaryMapOwnedAnyV2Route::RemovedKeyCheckedMiss;
        if receipt.profile != route.profile() {
            return Err(vec![NativeMirError::new(
                route.label(),
                format!(
                    "exact dedicated profile '{}' required, got '{}'",
                    route.profile(),
                    receipt.profile
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        route.validate(checked, program).map_err(|message| {
            vec![NativeMirError::new(
                format!("{} route receipt", route.label()),
                format!(
                    "{}: {} native route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
        crate::core::mir::validate_ordinary_map_owned_any_v2_removed_key_checked_miss_mir_route(
            program,
        )
        .map_err(|message| {
            vec![NativeMirError::new(
                format!("{} MIR route receipt", route.label()),
                format!(
                    "{}: {} native MIR route rejected: {message}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    route.label()
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]
        })?;
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            if checked_managed_any_unpack_coverage_for_route(checked, program, Some(route)).is_err()
            {
                return Err(original_error);
            }
        }
        let result =
            self.compile_mir_native_with_route_receipt_inner(program, receipt, Some(route));
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile canonical MIR after parsing and checking a CLI route manifest.
    /// Keep this manifest boundary beside the typed receipt entry point so
    /// native emission cannot accept an unvalidated field extension.
    pub fn compile_mir_native_with_route_manifest(
        &mut self,
        program: &MirProgram,
        manifest: &str,
    ) -> Result<(), Vec<Diagnostic>> {
        reject_verifier_only_program(program)?;
        let receipt =
            crate::core::mir::CanonicalMirRouteReceipt::from_manifest_round_trip(manifest)
                .map_err(|message| {
                    vec![NativeMirError::new(
                        "mir-program",
                        format!(
                            "{}: canonical route manifest rejected: {message}",
                            crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE)
                    .diagnostic()]
                })?;
        self.compile_mir_native_with_route_receipt(program, &receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        checker_route_authorization_mismatch, exactly_one_selected_type, has_only_field_named,
        has_single_equality_read_for_literal, CheckedOrdinaryMapOwnedAnyV2Route,
        CheckerRouteAuthorizationMismatch, NativeMirError,
    };
    use crate::core::mir::{
        MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_MANIFEST_ERROR_CODE,
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE,
    };

    #[test]
    fn direct_print_result_is_rejected_by_every_generic_native_mir_entry() {
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_direct_print_hit_i32.mimi"
        );
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("DirectPrintResult source lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("DirectPrintResult source parses");
        let checked =
            crate::core::check_program(&file).expect("DirectPrintResult source type-checks");
        let mir = crate::core::mir::reference::MirProgram::
            from_checked_program_ordinary_map_direct_print_result(&checked)
            .expect("DirectPrintResult source lowers to its exact MIR");
        let direct_profile =
            crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrintResult
                .as_str();
        let receipts = [
            mir.route_receipt(direct_profile),
            mir.route_receipt("run-canonical-v1"),
        ];

        let context = inkwell::context::Context::create();
        let mut without_receipt = crate::codegen::CodeGenerator::new(
            &context,
            "direct_print_result_generic_native_without_receipt",
        );
        let error = without_receipt
            .compile_mir_native(&mir)
            .expect_err("the raw generic native API cannot compile DirectPrintResult MIR");
        assert_eq!(
            error[0].code.as_deref(),
            Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
        assert!(error[0]
            .message
            .contains("dedicated Checker-bound native route"));
        assert!(without_receipt.mir_native_compiled_digest.is_none());

        for (index, receipt) in receipts.iter().enumerate() {
            let context = inkwell::context::Context::create();
            let mut generic = crate::codegen::CodeGenerator::new(
                &context,
                &format!("direct_print_result_generic_native_receipt_{index}"),
            );
            let error = generic
                .compile_mir_native_with_route_receipt(&mir, receipt)
                .expect_err("a generic or DirectPrintResult receipt cannot open the raw API");
            assert_eq!(
                error[0].code.as_deref(),
                Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
            );
            assert!(
                error[0]
                    .message
                    .contains("dedicated Checker-bound native route"),
                "{}",
                error[0].message
            );
            assert!(generic.mir_native_compiled_digest.is_none());
        }

        let ordinary_tokens = crate::lexer::Lexer::new("func main() -> i32 { 42 }")
            .tokenize()
            .expect("ordinary scalar source lexes");
        let ordinary_file = crate::parser::Parser::new(ordinary_tokens)
            .parse_file()
            .expect("ordinary scalar source parses");
        let ordinary_checked =
            crate::core::check_program(&ordinary_file).expect("ordinary scalar source type-checks");
        let ordinary_mir =
            crate::core::mir::reference::MirProgram::from_checked_program(&ordinary_checked)
                .expect("ordinary scalar source lowers to MIR");
        assert!(ordinary_mir.functions().values().all(|function| {
            function
                .ownership
                .ordinary_map_owned_any_v2_direct_print_result_receipt()
                .is_none()
        }));
        let forged_direct_print_receipt = ordinary_mir.route_receipt(direct_profile);
        let context = inkwell::context::Context::create();
        let mut profile_only = crate::codegen::CodeGenerator::new(
            &context,
            "direct_print_result_profile_without_source_signal",
        );
        let error = profile_only
            .compile_mir_native_with_route_receipt(&ordinary_mir, &forged_direct_print_receipt)
            .expect_err("the DirectPrintResult profile alone cannot authorize generic native MIR");
        assert_eq!(
            error[0].code.as_deref(),
            Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
        assert!(error[0]
            .message
            .contains("dedicated Checker-bound native route"));
        assert!(profile_only.mir_native_compiled_digest.is_none());
    }

    #[test]
    fn branch_local_set_selection_rejects_missing_or_ambiguous_type_metadata() {
        use crate::core::mir::OrdinaryMapOwnedAnyV2SelectedType;

        let missing = Vec::<OrdinaryMapOwnedAnyV2SelectedType>::new();
        assert_eq!(
            exactly_one_selected_type(&missing).unwrap_err(),
            "BranchLocalSetDiamond payload-selection receipt has no selected TypeDesc"
        );

        let payload_type = crate::core::ir::ResolvedTypeId::synthetic("test:Payload");
        let selected = OrdinaryMapOwnedAnyV2SelectedType {
            type_id: payload_type.clone(),
            type_desc_digest: "a".repeat(64),
        };
        assert_eq!(
            exactly_one_selected_type(std::slice::from_ref(&selected)).unwrap(),
            &payload_type
        );

        let ambiguous = vec![selected.clone(), selected];
        assert_eq!(
            exactly_one_selected_type(&ambiguous).unwrap_err(),
            "BranchLocalSetDiamond payload-selection receipt must select exactly one TypeDesc"
        );
    }

    #[test]
    fn branch_local_set_payload_shape_requires_one_named_text_field() {
        assert!(!has_only_field_named(&[], "text"));
        assert!(!has_only_field_named(
            &[
                ("id".into(), "i32".into()),
                ("text".into(), "string".into())
            ],
            "text"
        ));
        assert!(!has_only_field_named(
            &[("label".into(), "string".into())],
            "text"
        ));
        assert!(has_only_field_named(
            &[("text".into(), "string".into())],
            "text"
        ));
    }

    #[test]
    fn checker_route_authorization_rejects_missing_and_wrong_profiles() {
        let required = Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller);
        let authorized = Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller);
        assert_eq!(
            checker_route_authorization_mismatch(required, authorized),
            None
        );

        assert_eq!(
            checker_route_authorization_mismatch(required, None),
            Some(CheckerRouteAuthorizationMismatch::RequiredButMissing(
                CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller
            ))
        );
        assert_eq!(
            checker_route_authorization_mismatch(
                None,
                Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller)
            ),
            Some(
                CheckerRouteAuthorizationMismatch::AuthorizationForDifferentRoute(
                    CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller
                )
            )
        );
        assert_eq!(
            checker_route_authorization_mismatch(
                Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller),
                Some(CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshot)
            ),
            Some(CheckerRouteAuthorizationMismatch::RequiredButMissing(
                CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller
            ))
        );
    }

    #[test]
    fn values_string_read_requires_exactly_one_expected_equality_witness() {
        assert!(has_single_equality_read_for_literal(
            [(false, "text payload")].into_iter(),
            "text payload"
        ));
        assert!(has_single_equality_read_for_literal(
            [(false, "second value")].into_iter(),
            "second value"
        ));
        assert!(!has_single_equality_read_for_literal(
            std::iter::empty::<(bool, &str)>(),
            "text payload"
        ));
        assert!(!has_single_equality_read_for_literal(
            [(false, "text payload"), (false, "text payload")].into_iter(),
            "text payload"
        ));
        assert!(!has_single_equality_read_for_literal(
            [(true, "text payload")].into_iter(),
            "text payload"
        ));
        assert!(!has_single_equality_read_for_literal(
            [(false, "different literal")].into_iter(),
            "text payload"
        ));
    }

    #[test]
    fn map_set_drop_control_native_eligibility_is_exact_and_narrow() {
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_v2_map_set_drop_control.mimi"
        );
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("MapSetDropControl source lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("MapSetDropControl source parses");
        let checked =
            crate::core::check_program(&file).expect("MapSetDropControl source type-checks");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControl
        );
        let mir = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("MapSetDropControl lowers to MIR");
        let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE;
        let receipt = mir.route_receipt(profile);
        let context = inkwell::context::Context::create();
        let mut native =
            crate::codegen::CodeGenerator::new(&context, "map_set_drop_control_native_eligibility");
        native
            .compile_checked_mir_native_with_map_set_drop_control_route(&checked, &mir, &receipt)
            .expect("the exact Checker receipt admits this narrow native route");
        let expected_digest = mir.canonical_digest();
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(expected_digest.as_str())
        );

        let context = inkwell::context::Context::create();
        let mut broad =
            crate::codegen::CodeGenerator::new(&context, "map_set_drop_control_broad_gate");
        let error = broad
            .compile_mir_native_with_route_receipt(&mir, &mir.route_receipt("run-canonical-v1"))
            .expect_err("a generic route label cannot bypass closed broad readiness");
        assert_eq!(
            error[0].code.as_deref(),
            Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
        assert!(
            error[0]
                .message
                .contains("diagnostic-only while V2 production readiness is disabled"),
            "{}",
            error[0].message
        );

        let forged =
            mir.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE);
        let context = inkwell::context::Context::create();
        let mut wrong_profile =
            crate::codegen::CodeGenerator::new(&context, "map_set_drop_control_wrong_profile");
        let error = wrong_profile
            .compile_checked_mir_native_with_map_set_drop_control_route(&checked, &mir, &forged)
            .expect_err("the control adapter cannot borrow IsEmpty's profile identity");
        assert_eq!(
            error[0].code.as_deref(),
            Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
        assert!(wrong_profile.mir_native_compiled_digest.is_none());
    }

    #[test]
    fn map_set_drop_control_result_native_emitter_uses_its_exact_branch_receipt() {
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_v2_map_set_drop_control.mimi"
        );
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("MapSetDropControlResult source lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("MapSetDropControlResult source parses");
        let checked =
            crate::core::check_program(&file).expect("MapSetDropControlResult source type-checks");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControlResult
        );
        let mir = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("MapSetDropControlResult lowers to MIR");
        let profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_RESULT_BRANCH_PROFILE;
        let receipt = mir.route_receipt(profile);

        let context = inkwell::context::Context::create();
        let mut native = crate::codegen::CodeGenerator::new(
            &context,
            "map_set_drop_control_result_native_route",
        );
        native
            .compile_checked_mir_native_with_map_set_drop_control_result_route(
                &checked, &mir, &receipt,
            )
            .expect("the dedicated Checker branch receipt admits this native route");
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(mir.canonical_digest().as_str())
        );

        let mut forged_receipt = receipt.clone();
        forged_receipt.mir_digest = "0".repeat(64);
        let context = inkwell::context::Context::create();
        let mut forged = crate::codegen::CodeGenerator::new(
            &context,
            "map_set_drop_control_result_forged_receipt",
        );
        let error = forged
            .compile_checked_mir_native_with_map_set_drop_control_result_route(
                &checked,
                &mir,
                &forged_receipt,
            )
            .expect_err("a fabricated MIR digest cannot authorize the Result emitter");
        assert_eq!(
            error[0].code.as_deref(),
            Some(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
        );
        assert!(forged.mir_native_compiled_digest.is_none());

        let mut forged_anchor_mir = mir.clone();
        let owner = crate::core::NodeId("function:main".into());
        let forged_drop = forged_anchor_mir
            .function_mut_for_test(&owner)
            .expect("fixture main is mutable in this unit test")
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { .. }
                )
            })
            .expect("fixture has a root Drop anchor");
        let crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } =
            &mut forged_drop.kind
        else {
            unreachable!("the selected instruction is a root Drop")
        };
        *root = crate::core::mir::MirValueId::new("local:forged_map_root")
            .expect("test forged root ID is well formed");
        let forged_anchor_receipt = forged_anchor_mir.route_receipt(profile);
        let context = inkwell::context::Context::create();
        let mut forged_anchor = crate::codegen::CodeGenerator::new(
            &context,
            "map_set_drop_control_result_forged_anchor",
        );
        let error = forged_anchor
            .compile_checked_mir_native_with_map_set_drop_control_result_route(
                &checked,
                &forged_anchor_mir,
                &forged_anchor_receipt,
            )
            .expect_err("a fabricated Map root Drop operand cannot authorize this route");
        assert_eq!(
            error[0].code.as_deref(),
            Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
        assert!(forged_anchor.mir_native_compiled_digest.is_none());

        let wrong_profile = mir.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE,
        );
        let context = inkwell::context::Context::create();
        let mut wrong_profile_generator = crate::codegen::CodeGenerator::new(
            &context,
            "map_set_drop_control_result_wrong_profile",
        );
        let error = wrong_profile_generator
            .compile_checked_mir_native_with_map_set_drop_control_result_route(
                &checked,
                &mir,
                &wrong_profile,
            )
            .expect_err("the no-result MapSetDropControl receipt cannot authorize this emitter");
        assert_eq!(
            error[0].code.as_deref(),
            Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
        assert!(wrong_profile_generator.mir_native_compiled_digest.is_none());
    }

    #[test]
    fn record_unit_native_eligibility_requires_its_exact_checker_receipt() {
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source =
            include_str!("../../../tests/fixtures/mir_ordinary_map_any_owner_v2_record_unit.mimi");
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("RecordUnit source lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("RecordUnit source parses");
        let checked = crate::core::check_program(&file).expect("RecordUnit source type-checks");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::RecordUnit
        );
        let mir = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("RecordUnit lowers to MIR");
        let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE;
        let receipt = mir.route_receipt(profile);
        let context = inkwell::context::Context::create();
        let mut native = crate::codegen::CodeGenerator::new(&context, "record_unit_native_route");
        native
            .compile_checked_mir_native_with_record_unit_route(&checked, &mir, &receipt)
            .expect("the exact Checker receipt admits this narrow native route");
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(mir.canonical_digest().as_str())
        );

        let context = inkwell::context::Context::create();
        let mut broad = crate::codegen::CodeGenerator::new(&context, "record_unit_broad_gate");
        let error = broad
            .compile_mir_native_with_route_receipt(&mir, &mir.route_receipt("run-canonical-v1"))
            .expect_err("a generic route label cannot bypass closed broad readiness");
        assert_eq!(
            error[0].code.as_deref(),
            Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
        assert!(
            error[0]
                .message
                .contains("diagnostic-only while V2 production readiness is disabled"),
            "{}",
            error[0].message
        );

        let wrong_receipt =
            mir.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE);
        let context = inkwell::context::Context::create();
        let mut wrong_profile =
            crate::codegen::CodeGenerator::new(&context, "record_unit_wrong_profile");
        let error = wrong_profile
            .compile_checked_mir_native_with_record_unit_route(&checked, &mir, &wrong_receipt)
            .expect_err("IsEmpty cannot substitute for the exact mixed RecordUnit profile");
        assert_eq!(
            error[0].code.as_deref(),
            Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
        assert!(wrong_profile.mir_native_compiled_digest.is_none());
    }

    #[test]
    fn persistent_root_alias_replacement_native_route_is_exact_and_keeps_global_gate_closed() {
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source =
            include_str!("../../../tests/fixtures/mir_ordinary_map_owned_any_v2_clone.mimi");
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("PersistentRootAliasReplacement source lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("PersistentRootAliasReplacement source parses");
        let checked = crate::core::check_program(&file)
            .expect("PersistentRootAliasReplacement source type-checks");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::PersistentRootAliasReplacement
        );
        let mir = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("PersistentRootAliasReplacement lowers to MIR");
        let profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE;
        let receipt = mir.route_receipt(profile);

        let context = inkwell::context::Context::create();
        let mut native = crate::codegen::CodeGenerator::new(
            &context,
            "persistent_root_alias_replacement_native_route",
        );
        native
            .compile_checked_mir_native_with_persistent_root_alias_replacement_route(
                &checked, &mir, &receipt,
            )
            .expect("the exact Checker receipt admits this independent native route");
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(mir.canonical_digest().as_str())
        );
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };

        let wrong_receipt =
            mir.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE);
        let context = inkwell::context::Context::create();
        let mut wrong_profile =
            crate::codegen::CodeGenerator::new(&context, "persistent_root_alias_wrong_profile");
        let error = wrong_profile
            .compile_checked_mir_native_with_persistent_root_alias_replacement_route(
                &checked,
                &mir,
                &wrong_receipt,
            )
            .expect_err("CloneIsolation cannot authorize root alias replacement");
        assert_eq!(
            error[0].code.as_deref(),
            Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
        assert!(wrong_profile.mir_native_compiled_digest.is_none());

        let context = inkwell::context::Context::create();
        let mut generic_checked =
            crate::codegen::CodeGenerator::new(&context, "persistent_root_alias_generic_checked");
        let error = generic_checked
            .compile_checked_mir_native_with_route_receipt(&checked, &mir, &receipt)
            .expect_err(
                "the generic checked receipt entry cannot authorize this source-bound route",
            );
        assert_eq!(
            error[0].code.as_deref(),
            Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
        assert!(generic_checked.mir_native_compiled_digest.is_none());

        let context = inkwell::context::Context::create();
        let mut broad =
            crate::codegen::CodeGenerator::new(&context, "persistent_root_alias_broad_gate");
        let error = broad
            .compile_mir_native_with_route_receipt(&mir, &mir.route_receipt("run-canonical-v1"))
            .expect_err("broad route receipts cannot open the global V2 readiness gate");
        assert_eq!(
            error[0].code.as_deref(),
            Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
        assert!(broad.mir_native_compiled_digest.is_none());
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
    }

    #[test]
    fn route_errors_keep_structured_code_when_rendered() {
        let diagnostic = NativeMirError::new("mir-program", "future field")
            .with_code(MIR_ROUTE_MANIFEST_ERROR_CODE)
            .diagnostic();

        assert_eq!(
            diagnostic.code.as_deref(),
            Some(MIR_ROUTE_MANIFEST_ERROR_CODE)
        );
        assert_eq!(
            diagnostic.to_string(),
            format!(
                "[{}] canonical MIR native backend rejected mir-program: future field",
                MIR_ROUTE_MANIFEST_ERROR_CODE
            )
        );
        let origin = diagnostic.origin.expect("route error has provenance");
        assert_eq!(
            origin.kind,
            crate::diagnostic::DiagnosticOriginKind::RuntimeSystem
        );
        assert_eq!(origin.rule.as_deref(), Some("mir.route"));
    }

    #[test]
    fn values_mixed_snapshot_remove_native_adapter_rejects_unreceipted_stdout_before_emission() {
        const STDOUT_REASON: &str = "Values snapshot-Remove contains scalar stdout whose argument is not derived from a receipted V2 result";
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_snapshot_remove.mimi"
        );
        let near_misses = [
            (
                "literal stdout",
                source.replace("println(number_matches)", "println(true)"),
            ),
            (
                "transformed stdout",
                source.replace("println(number_matches)", "println(number_value + 1)"),
            ),
        ];
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let context = inkwell::context::Context::create();

        for (label, near_miss_source) in near_misses {
            let tokens = crate::lexer::Lexer::new(&near_miss_source)
                .tokenize()
                .unwrap_or_else(|error| panic!("{label} source lexes: {error}"));
            let file = crate::parser::Parser::new(tokens)
                .parse_file()
                .unwrap_or_else(|error| panic!("{label} source parses: {error}"));
            let checked = crate::core::check_program(&file)
                .unwrap_or_else(|errors| panic!("{label} source checks: {errors:?}"));
            assert_eq!(
                crate::core::mir::classify_canonical_mir_route_admission(&checked)
                    .ordinary_map_owned_any_v2,
                crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshotRemove,
                "{label} source keeps the exact owner admission"
            );

            let mir = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
                .unwrap_or_else(|errors| panic!("{label} source lowers to MIR: {errors:?}"));
            let fresh_receipt =
                mir.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE);
            fresh_receipt
                .validate_against_program(&mir)
                .expect("the mutation receives a fresh receipt bound to its MIR digest");

            let module_name = format!("values_snapshot_remove_unreceipted_{label}");
            let mut native = crate::codegen::CodeGenerator::new(&context, &module_name);
            let errors = native
                .compile_checked_mir_native_with_values_mixed_snapshot_remove_route(
                    &checked,
                    &mir,
                    &fresh_receipt,
                )
                .expect_err("the dedicated native adapter rejects unreceipted stdout");
            assert_eq!(errors.len(), 1, "{label} rejection is a single route error");
            assert_eq!(
                errors[0].code.as_deref(),
                Some(MIR_ROUTE_COVERAGE_ERROR_CODE),
                "{label} rejection is the shared route coverage error"
            );
            assert!(
                errors[0].message.contains(&format!(
                    "ValuesMixedSnapshotRemove native route rejected: {STDOUT_REASON}"
                )),
                "{label} rejection comes from the shared semantic validator: {}",
                errors[0].message
            );
            assert!(
                native.mir_native_compiled_digest.is_none(),
                "{label} rejection precedes native artifact emission"
            );
            assert!(
                native.module.get_function("main").is_none(),
                "{label} rejection precedes creation of the MIR main function"
            );
        }
    }
}

pub(super) fn instruction_kind(
    instruction: &crate::core::mir::MirInstruction,
) -> &MirInstructionKind {
    &instruction.kind
}

pub(super) fn mir_symbol(owner: &crate::core::NodeId) -> Result<String, String> {
    let Some(symbol) = owner.0.strip_prefix("function:") else {
        let transition = owner
            .0
            .strip_prefix("transition:")
            .ok_or_else(|| "callable identity is not a function or transition owner".to_string())?;
        if transition.trim().is_empty() {
            return Err("transition symbol must not be empty".into());
        }
        return Ok(format!(
            "__mimi_transition_{}",
            native_symbol_fragment(transition)
        ));
    };
    if symbol.trim().is_empty() {
        return Err("only simple function symbols are in the native MIR slice".into());
    }
    if symbol.starts_with("mimi_") {
        return Err("function symbol collides with reserved runtime namespace".into());
    }
    if symbol.contains("::") {
        // Protocol method owners are checker-canonical identities such as
        // `Read:for:Counter::read:<hash>`.  They are not surface names and
        // therefore need a deterministic LLVM-safe spelling, shared by the
        // declaration and call maps.  Keep ordinary function symbols
        // unchanged for compatibility with existing native callers.
        Ok(format!("__mimi_method_{}", native_symbol_fragment(symbol)))
    } else {
        Ok(symbol.to_owned())
    }
}

pub(super) fn native_symbol_fragment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

// The existing resolved/legacy E0722 gate remains unchanged. This scanner is
// local to the canonical entry point and checks all conversion positions in
// the CheckedProgram, including dormant bodies emitted by shared libraries.
#[derive(Default)]
struct CheckedManagedAnyUnpackScan {
    conversions: Vec<(crate::core::NodeId, crate::core::CheckedConversion)>,
}
impl CheckedManagedAnyUnpackScan {
    fn conversion(
        &mut self,
        conversion: &crate::core::CheckedConversion,
        origin: &crate::core::NodeId,
    ) {
        if conversion.kind == crate::core::CheckedConversionKind::DynamicAnyUnpack {
            self.conversions.push((origin.clone(), conversion.clone()));
        }
    }
    fn block(&mut self, block: &ResolvedBlock) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind { initializer, .. } => {
                    if let Some(initializer) = initializer {
                        self.expression(initializer);
                    }
                }
                ResolvedStmtKind::Assign {
                    value, conversion, ..
                } => {
                    self.conversion(conversion, &statement.node_id);
                    self.expression(value);
                }
                ResolvedStmtKind::Return { value, conversion } => {
                    if let Some(value) = value {
                        self.expression(value);
                    }
                    if let Some(conversion) = conversion {
                        self.conversion(conversion, &statement.node_id);
                    }
                }
                ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        self.expression(value);
                    }
                }
                ResolvedStmtKind::Expr(expression) => self.expression(expression),
                ResolvedStmtKind::While { condition, body } => {
                    self.expression(condition);
                    self.block(body);
                }
                ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                }
                | ResolvedStmtKind::For {
                    iterable: initializer,
                    body,
                    ..
                } => {
                    self.expression(initializer);
                    self.block(body);
                }
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    self.expression(initializer);
                    self.block(then_block);
                    if let Some(else_block) = else_block {
                        self.block(else_block);
                    }
                }
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    self.block(body)
                }
                ResolvedStmtKind::Contract { condition, .. } => self.expression(condition),
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        self.expression(expression);
                    }
                }
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    self.expression(value);
                    self.block(body);
                }
                ResolvedStmtKind::Continue
                | ResolvedStmtKind::Drop(_)
                | ResolvedStmtKind::NestedCallable(_) => {}
            }
        }
        if let Some(result) = &block.result {
            self.expression(result);
        }
    }

    fn expression(&mut self, expression: &ResolvedExpr) {
        match &expression.kind {
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let crate::core::ir::ResolvedFStringPart::Interpolation(value) = part {
                        self.expression(value);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                self.expression(value);
                if let crate::core::ir::ResolvedValueProjection::Index(index) = projection {
                    self.expression(index);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                self.expression(left);
                self.expression(right);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => self.expression(operand),
            ResolvedExprKind::Cast { value, conversion } => {
                self.conversion(conversion, &expression.node_id);
                self.expression(value);
            }
            ResolvedExprKind::Call(call) => {
                for argument in &call.arguments {
                    self.conversion(&argument.conversion, &argument.value.node_id);
                    self.expression(&argument.value);
                }
            }
            ResolvedExprKind::Tuple(elements)
            | ResolvedExprKind::List(elements)
            | ResolvedExprKind::Set(elements) => {
                for element in elements {
                    self.expression(element);
                }
            }
            ResolvedExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.expression(key);
                    self.expression(value);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.expression(value);
                self.expression(iterable);
                if let Some(guard) = guard {
                    self.expression(guard);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => self.expression(receiver),
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    self.conversion(&field.conversion, &field.value.node_id);
                    self.expression(&field.value);
                }
                if let Some(rest) = rest {
                    self.expression(rest);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.block(block),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.expression(condition);
                self.block(then_block);
                self.block(else_block);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.expression(scrutinee);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.expression(guard);
                    }
                    self.expression(&arm.body);
                }
            }
            ResolvedExprKind::Range { start, end } => {
                self.expression(start);
                self.expression(end);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.expression(target);
                if let Some(start) = start {
                    self.expression(start);
                }
                if let Some(end) = end {
                    self.expression(end);
                }
            }
            ResolvedExprKind::Lambda(lambda) => self.block(&lambda.body),
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
            ResolvedExprKind::Callable(_) => {}
        }
    }
}

pub(crate) fn checked_managed_any_unpack_coverage(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    checked_managed_any_unpack_coverage_for_route(checked, program, None)
}

fn checked_managed_any_unpack_coverage_for_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    authorized_route: Option<CheckedOrdinaryMapOwnedAnyV2Route>,
) -> Result<(), String> {
    let catalog = program.type_catalog();
    let mut expected = BTreeMap::new();
    let mut expected_v2_label = BTreeSet::new();
    let mut expected_v2_checked_get_bool = BTreeSet::new();
    let mut removed_key_checked_miss_i32_actual = BTreeSet::new();
    if authorized_route == Some(CheckedOrdinaryMapOwnedAnyV2Route::CheckedGet) {
        CheckedOrdinaryMapOwnedAnyV2Route::CheckedGet.validate(checked, program)?;
    }
    for (owner, callable) in checked.callables() {
        if checked
            .functions()
            .get(owner)
            .is_some_and(|f| f.is_comptime)
        {
            continue;
        }
        let mut scan = CheckedManagedAnyUnpackScan::default();
        for expression in callable.body.place_inputs.values() {
            scan.expression(expression);
        }
        for expression in callable.body.default_values.values() {
            scan.expression(expression);
        }
        scan.block(&callable.body.root);
        for (origin, conversion) in scan.conversions {
            if !matches!(checked.resolved_types().get(&conversion.from),Some(crate::core::ResolvedType::DynamicAny { capability }) if capability == "type.dynamic_value")
            {
                return Err(
                    "checked Any conversion source has no exact dynamic-value type capability"
                        .into(),
                );
            }
            let key = (owner.clone(), origin, conversion.from, conversion.to);
            let checked_get_bool_target = authorized_route
                == Some(CheckedOrdinaryMapOwnedAnyV2Route::CheckedGet)
                && matches!(
                    checked.resolved_types().get(&key.3),
                    Some(crate::core::ResolvedType::Primitive(
                        crate::core::PrimitiveType::Bool
                    ))
                )
                && catalog.validate_copy_scalar(&key.3).is_ok();
            let operation = match checked.resolved_types().get(&key.3) {
                Some(crate::core::ResolvedType::Primitive(crate::core::PrimitiveType::I32)) => {
                    Some(crate::core::mir::MirManagedAnyOperation::UnpackI32)
                }
                Some(crate::core::ResolvedType::Primitive(crate::core::PrimitiveType::String)) => {
                    Some(crate::core::mir::MirManagedAnyOperation::UnpackString)
                }
                _ if checked_get_bool_target => None,
                _ if checked_managed_any_v2_label_target_supported(checked, &key.3)
                    && super::ordinary_map_owned_any_v2::v2_values_is_label_type(catalog, &key.3) =>
                {
                    None
                }
                _ => {
                    return Err(
                        "checked Any conversion target is outside the v1 i32/String and receipted exact V2 Label descriptor contracts".into(),
                    )
                }
            };
            let inserted = if let Some(operation) = operation {
                expected.insert(key, operation).is_none()
            } else if checked_get_bool_target {
                expected_v2_checked_get_bool.insert(key)
            } else {
                expected_v2_label.insert(key)
            };
            if !inserted {
                return Err(
                    "a checked conversion origin carries duplicate Any unpack obligations".into(),
                );
            }
        }
    }
    let mut actual = BTreeSet::new();
    for function in program.functions().values() {
        catalog.validate_managed_any_function(function)?;
        for instruction in function.blocks.values().flat_map(|b| &b.instructions) {
            let MirInstructionKind::ManagedAny {
                result,
                source,
                operation,
                contract,
            } = &instruction.kind
            else {
                continue;
            };
            if !matches!(
                operation,
                crate::core::mir::MirManagedAnyOperation::UnpackI32
                    | crate::core::mir::MirManagedAnyOperation::UnpackString
            ) {
                continue;
            }
            catalog.validate_managed_any_contract(
                function,
                source,
                result.as_ref(),
                *operation,
                contract.as_ref(),
            )?;
            let contract = contract
                .as_ref()
                .ok_or_else(|| "ManagedAny unpack has no receipt".to_string())?;
            let target = contract
                .target_type
                .as_ref()
                .ok_or_else(|| "ManagedAny unpack receipt has no target".to_string())?;
            // Source IDs intentionally differ: CheckedProgram retains the
            // exact DynamicAny TypeDesc; canonical lowering substitutes its
            // independently sealed generation-bearing ManagedAny TypeDesc.
            let matches = expected
                .iter()
                .filter(|((owner, origin, _, to), op)| {
                    owner == &function.owner
                        && origin == &contract.origin
                        && to == target
                        && *op == operation
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            if matches.len() != 1
                || contract.source_type
                    != crate::core::mir::types::managed_any_i32_string_v1_type_id()
            {
                return Err("ManagedAny unpack has no unique exact checked (owner,origin,from,to) conversion".into());
            }
            let Some(matched) = matches.into_iter().next() else {
                return Err("ManagedAny unpack has no unique exact checked (owner,origin,from,to) conversion".into());
            };
            if !actual.insert(matched) {
                return Err(
                    "one checked Any conversion is claimed by multiple owner receipts".into(),
                );
            }
        }
    }
    let checked_get_v2_receipts = program.ordinary_map_owned_any_v2_unpack_result_receipts()?;
    let values_v2_receipts = program.ordinary_map_owned_any_v2_values_unpack_result_receipts()?;
    let values_route =
        authorized_route.unwrap_or(CheckedOrdinaryMapOwnedAnyV2Route::ValuesFieldMove);
    if !values_v2_receipts.is_empty() {
        values_route.validate(checked, program)?;
    }
    let mut checked_get_v2_actual = BTreeSet::new();
    let mut values_v2_label_actual = BTreeSet::new();
    let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let bool_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    if crate::core::mir::has_ordinary_map_set_get_any_result_i32_route_candidate(checked) {
        if authorized_route.is_some() {
            return Err(
                "Result-valued Set/GetAny-i32 cannot borrow another owner-v2 native route authorization"
                    .into(),
            );
        }
        let owner = crate::core::NodeId("function:main".into());
        let route = program.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_EXECUTION_PROFILE,
        );
        super::validate_checked_mir_native_ordinary_map_owned_any_v2_set_get_any_result_i32_route(
            checked, program, &route,
        )
        .map_err(|diagnostics| {
            format!("Result-valued Set/GetAny-i32 native receipt rejected: {diagnostics:?}")
        })?;
        let source = checked
            .callable(&owner)
            .and_then(|callable| {
                callable
                    .resources
                    .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt
                    .as_ref()
            })
            .ok_or_else(|| {
                "Result-valued Set/GetAny-i32 has no checked unpack lineage receipt".to_string()
            })?;
        let key = (
            owner,
            source.get_unpack_operation.clone(),
            source.get_unpack_source_type.clone(),
            source.get_unpack_target_type.clone(),
        );
        if expected.len() != 1
            || !expected_v2_label.is_empty()
            || !expected_v2_checked_get_bool.is_empty()
            || expected.get(&key) != Some(&crate::core::mir::MirManagedAnyOperation::UnpackI32)
            || source.get_unpack_conversion_kind
                != crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
            || source.get_unpack_target_type != i32_type
        {
            return Err(
                "Result-valued Set/GetAny-i32 does not own the only checked DynamicAny conversion, exactly as i32"
                    .into(),
            );
        }
        if !actual.insert(key) {
            return Err(
                "Result-valued Set/GetAny-i32 unpack is already claimed by another owner receipt"
                    .into(),
            );
        }
    }
    for receipt in checked_get_v2_receipts {
        receipt.validate_against_program(program)?;
        let key = (
            receipt.owner.clone(),
            receipt.cast.clone(),
            receipt.conversion_from.clone(),
            receipt.conversion_to.clone(),
        );
        let is_checked_get_v2_label = expected_v2_label.contains(&key)
            && super::ordinary_map_owned_any_v2::v2_values_is_label_type(
                catalog,
                &receipt.result_type,
            );
        let is_checked_get_v2_bool = authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::CheckedGet)
            && expected_v2_checked_get_bool.contains(&key)
            && receipt.result_type == bool_type
            && receipt.conversion_to == bool_type
            && receipt.conversion_kind == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
            && receipt.payload_source
                == crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::SelectedSetPayload;
        let is_removed_key_checked_miss_i32 = authorized_route
            == Some(CheckedOrdinaryMapOwnedAnyV2Route::RemovedKeyCheckedMiss)
            && expected.get(&key) == Some(&crate::core::mir::MirManagedAnyOperation::UnpackI32)
            && receipt.result_type == i32_type
            && receipt.conversion_to == i32_type
            && receipt.conversion_kind == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
            && receipt.payload_source
                == crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::CanonicalI32Miss;
        if !expected.contains_key(&key)
            && !is_checked_get_v2_label
            && !is_checked_get_v2_bool
            && !is_removed_key_checked_miss_i32
        {
            return Err(
                "CheckedGet GetAny receipt has no exact checked (owner,origin,from,to) conversion"
                    .into(),
            );
        }
        if !actual.insert(key.clone()) {
            return Err("one checked Any conversion is claimed by multiple owner receipts".into());
        }
        if (is_checked_get_v2_label || is_checked_get_v2_bool)
            && !checked_get_v2_actual.insert(key.clone())
        {
            return Err(
                "one checked V2 conversion is claimed by multiple CheckedGet receipts".into(),
            );
        }
        if is_removed_key_checked_miss_i32 && !removed_key_checked_miss_i32_actual.insert(key) {
            return Err(
                "one checked i32 miss conversion is claimed by multiple RemovedKeyCheckedMiss receipts"
                    .into(),
            );
        }
    }
    for receipt in values_v2_receipts {
        receipt.validate_against_program(program)?;
        let key = (
            receipt.owner.clone(),
            receipt.cast.clone(),
            receipt.conversion_from.clone(),
            receipt.conversion_to.clone(),
        );
        if values_route == CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32ResultBranch {
            let is_checked_branch_local_i32 = expected.get(&key)
                == Some(&crate::core::mir::MirManagedAnyOperation::UnpackI32)
                && receipt.index_type == i32_type
                && receipt.result_type == i32_type
                && receipt.conversion_to == i32_type
                && receipt.conversion_kind
                    == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                && receipt.index_disposition
                    == crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::SelectedPayload
                && receipt.result_owner_settlement
                    == crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::NoOwnedPayload
                && receipt.result_owner_settlement_point.is_none()
                && receipt.result_owner_settlement_index.is_none()
                && receipt.result_owner_settlement_instruction.is_none()
                && receipt.result_owner_whole_drop_unproved_uses.is_none()
                && receipt.result_owner_string_drop_source_uses.is_none()
                && receipt.result_owner_string_drop_statement_order.is_none()
                && receipt.result_owner_string_drop_reads.is_none()
                && receipt.result_owner_field_move.is_none();
            if !is_checked_branch_local_i32 {
                return Err(
                    "ValuesLocalIndexI32ResultBranch has no exact checked local-index i32 conversion and NoOwnedPayload receipt"
                        .into(),
                );
            }
            if !actual.insert(key) {
                return Err(
                    "one checked Any conversion is claimed by multiple owner receipts".into(),
                );
            }
            continue;
        }
        if values_route == CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32 {
            let is_checked_values_local_i32 = expected.get(&key)
                == Some(&crate::core::mir::MirManagedAnyOperation::UnpackI32)
                && receipt.index_type == i32_type
                && receipt.conversion_to == i32_type
                && receipt.result_type == i32_type
                && receipt.conversion_kind
                    == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                && receipt.result_owner_settlement
                    == crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::NoOwnedPayload
                && receipt.result_owner_settlement_point.is_none()
                && receipt.result_owner_settlement_index.is_none()
                && receipt.result_owner_settlement_instruction.is_none()
                && receipt.result_owner_whole_drop_unproved_uses.is_none()
                && receipt.result_owner_string_drop_source_uses.is_none()
                && receipt.result_owner_string_drop_statement_order.is_none()
                && receipt.result_owner_string_drop_reads.is_none()
                && receipt.result_owner_field_move.is_none();
            if !is_checked_values_local_i32 {
                return Err(
                    "ValuesLocalIndexI32 unpack has no exact checked local-index i32 conversion and NoOwnedPayload receipt"
                        .into(),
                );
            }
            if !actual.insert(key) {
                return Err(
                    "one checked Any conversion is claimed by multiple owner receipts".into(),
                );
            }
            continue;
        }
        if values_route == CheckedOrdinaryMapOwnedAnyV2Route::ValuesHeterogeneousStringIndex1 {
            let is_checked_values_string = expected.get(&key)
                == Some(&crate::core::mir::MirManagedAnyOperation::UnpackString)
                && receipt.index_type == i32_type
                && receipt.result_type == string_type
                && receipt.conversion_to == string_type
                && receipt.conversion_kind
                    == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                && receipt.result_owner_settlement
                    == crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::ExplicitStringDrop
                && receipt.result_owner_settlement_point.is_some()
                && receipt.result_owner_settlement_index.is_some()
                && receipt.result_owner_settlement_instruction.is_some()
                && receipt.result_owner_string_drop_source_uses.is_some()
                && receipt.result_owner_string_drop_statement_order.is_some()
                && receipt.result_owner_string_drop_reads.as_ref().is_some_and(|reads| {
                    has_single_equality_read_for_literal(
                        reads.iter().map(|read| {
                            (read.source.not_equal, read.source.peer_literal.as_str())
                        }),
                        "second value",
                    )
                })
                && receipt.result_owner_whole_drop_unproved_uses.is_none()
                && receipt.result_owner_field_move.is_none();
            if !is_checked_values_string {
                return Err("ValuesHeterogeneousStringIndex1 unpack lacks its exact checked String selection and owner-drop receipt".into());
            }
            if !actual.insert(key) {
                return Err(
                    "one checked Any conversion is claimed by multiple owner receipts".into(),
                );
            }
            continue;
        }
        if matches!(
            values_route,
            CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshot
                | CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshotRemove
        ) {
            let is_checked_values_i32 = expected.get(&key)
                == Some(&crate::core::mir::MirManagedAnyOperation::UnpackI32)
                && receipt.result_type == i32_type
                && receipt.result_owner_settlement
                    == crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::NoOwnedPayload
                && receipt.result_owner_whole_drop_unproved_uses.is_none()
                && receipt.result_owner_field_move.is_none();
            let is_checked_values_string = expected.get(&key)
                == Some(&crate::core::mir::MirManagedAnyOperation::UnpackString)
                && receipt.result_type == string_type
                && receipt.result_owner_settlement
                    == crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::ExplicitStringDrop
                && receipt.result_owner_settlement_point.is_some()
                && receipt.result_owner_settlement_index.is_some()
                && receipt.result_owner_string_drop_source_uses.is_some()
                && receipt.result_owner_string_drop_statement_order.is_some()
                && receipt
                    .result_owner_string_drop_reads
                    .as_ref()
                    .is_some_and(|reads| {
                        has_single_equality_read_for_literal(
                            reads.iter().map(|read| {
                                (read.source.not_equal, read.source.peer_literal.as_str())
                            }),
                            "text payload",
                        )
                    })
                && receipt.result_owner_whole_drop_unproved_uses.is_none()
                && receipt.result_owner_field_move.is_none();
            let is_checked_values_label = expected_v2_label.contains(&key)
                && checked_managed_any_v2_label_target_supported(checked, &receipt.conversion_to)
                && super::ordinary_map_owned_any_v2::v2_values_is_label_type(
                    catalog,
                    &receipt.result_type,
                )
                && receipt.result_type == receipt.conversion_to
                && receipt.result_owner_settlement
                    == crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::DirectStringFieldMove
                && receipt.result_owner_whole_drop_unproved_uses.is_none()
                && receipt.result_owner_field_move.as_ref().is_some_and(|field_move| {
                    field_move.source.field_name == "text"
                        && field_move.source.has_exact_source_use_sets()
                });
            if !is_checked_values_i32 && !is_checked_values_string && !is_checked_values_label {
                return Err(
                    "ValuesMixedSnapshot unpack has no exact checked i32/String/Label conversion and owner receipt"
                        .into(),
                );
            }
            if !actual.insert(key.clone()) {
                return Err(
                    "one checked Any conversion is claimed by multiple owner receipts".into(),
                );
            }
            if is_checked_values_label && !values_v2_label_actual.insert(key) {
                return Err(
                    "one checked Label conversion is claimed by multiple Values receipts".into(),
                );
            }
            continue;
        }
        let is_checked_values_label = expected_v2_label.contains(&key)
            && checked_managed_any_v2_label_target_supported(checked, &receipt.conversion_to)
            && super::ordinary_map_owned_any_v2::v2_values_is_label_type(
                catalog,
                &receipt.result_type,
            )
            && receipt.result_type == receipt.conversion_to;
        let Some(field_move) = receipt.result_owner_field_move.as_ref() else {
            return Err(
                "Values unpack receipt lacks the exact DirectStringFieldMove Checker/MIR proof"
                    .into(),
            );
        };
        let field_move_reads_match = match values_route {
            CheckedOrdinaryMapOwnedAnyV2Route::ValuesFieldMove => {
                field_move.source.text_read_uses.len() == 2
                    && field_move.text_read_uses.len() == 2
                    && field_move
                        .source
                        .text_read_uses
                        .iter()
                        .filter(|read| !read.not_equal)
                        .count()
                        == 1
                    && field_move
                        .source
                        .text_read_uses
                        .iter()
                        .filter(|read| read.not_equal)
                        .count()
                        == 1
            }
            // The snapshot-replacement receipt binds two independently
            // indexed Label unpacks, each with its own exact field-move
            // evidence. Its source profile does not require the one-unpack
            // Eq/Ne pair used by ValuesFieldMove.
            CheckedOrdinaryMapOwnedAnyV2Route::ValuesSnapshotReplacement => true,
            // MixedSnapshot's Checker/MIR route validator has already bound
            // the exact three-element index/key/type sequence and field
            // settlement. This branch is unreachable above, but keeps the
            // match exhaustive if the route flow is refactored.
            CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshot
            | CheckedOrdinaryMapOwnedAnyV2Route::ValuesMixedSnapshotRemove => true,
            // The exact local-index route has no owned result to project;
            // its i32 unpack is handled before the field-move census.
            CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32 => false,
            CheckedOrdinaryMapOwnedAnyV2Route::ValuesLocalIndexI32ResultBranch => false,
            CheckedOrdinaryMapOwnedAnyV2Route::ValuesHeterogeneousStringIndex1 => false,
            CheckedOrdinaryMapOwnedAnyV2Route::DynamicKeysGet => {
                return Err("DynamicKeysGet cannot satisfy a Values field-move receipt".into());
            }
            // ComputedKeyGet authorizes GetAny extraction, never a Values
            // field move. If a Values receipt reaches this shared validator,
            // fail closed rather than borrowing that unrelated route's proof.
            CheckedOrdinaryMapOwnedAnyV2Route::CheckedGet
            | CheckedOrdinaryMapOwnedAnyV2Route::ComputedKeyGet
            | CheckedOrdinaryMapOwnedAnyV2Route::ComputedStringKeyOwnerChain
            | CheckedOrdinaryMapOwnedAnyV2Route::EmptyRootRootQueriesV1
            | CheckedOrdinaryMapOwnedAnyV2Route::ZeroMissI32
            | CheckedOrdinaryMapOwnedAnyV2Route::RemovedKeyCheckedMiss
            | CheckedOrdinaryMapOwnedAnyV2Route::IsEmpty
            | CheckedOrdinaryMapOwnedAnyV2Route::RecordUnit
            | CheckedOrdinaryMapOwnedAnyV2Route::MapSetDropControl
            | CheckedOrdinaryMapOwnedAnyV2Route::MapSetDropControlResult
            | CheckedOrdinaryMapOwnedAnyV2Route::BranchLocalSetDiamond
            | CheckedOrdinaryMapOwnedAnyV2Route::CloneIsolation
            | CheckedOrdinaryMapOwnedAnyV2Route::PersistentRootAliasReplacement
            | CheckedOrdinaryMapOwnedAnyV2Route::KeysSnapshot
            | CheckedOrdinaryMapOwnedAnyV2Route::ValuesProducerCaller
            | CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCaller
            | CheckedOrdinaryMapOwnedAnyV2Route::ValuesCrosscallStringCallerIndex1
            | CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCaller
            | CheckedOrdinaryMapOwnedAnyV2Route::ValuesResultCrosscallStringCallerIndex1
            | CheckedOrdinaryMapOwnedAnyV2Route::MapRootProducerCaller
            | CheckedOrdinaryMapOwnedAnyV2Route::MapRootGetAnyCaller
            | CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultI32
            | CheckedOrdinaryMapOwnedAnyV2Route::GetOrDefaultString => false,
        };
        if receipt.result_owner_settlement
            != crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::DirectStringFieldMove
            || receipt.result_owner_whole_drop_unproved_uses.is_some()
            || field_move.source.field_name != "text"
            || !field_move.source.has_exact_source_use_sets()
            || !field_move_reads_match
        {
            return Err(
                "Values Label receipt lacks the exact text:string field move and equality-read source uses"
                    .into(),
            );
        }
        if !is_checked_values_label {
            return Err(
                "Values unpack receipt has no exact checked (owner,cast,from,to,result) Label conversion"
                    .into(),
            );
        }
        if !actual.insert(key.clone()) || !values_v2_label_actual.insert(key) {
            return Err(
                "one checked Label conversion is claimed by multiple owner receipts".into(),
            );
        }
    }
    if actual.len() != expected.len() + expected_v2_label.len() + expected_v2_checked_get_bool.len()
    {
        return Err(
            "checked Any conversions do not have complete canonical owner receipt coverage".into(),
        );
    }
    if checked_get_v2_actual.len() + values_v2_label_actual.len()
        != expected_v2_label.len() + expected_v2_checked_get_bool.len()
    {
        return Err(
            "checked V2 conversions do not have profile-specific GetAny or Values receipt coverage"
                .into(),
        );
    }
    let expected_removed_key_checked_miss_i32 = usize::from(
        authorized_route == Some(CheckedOrdinaryMapOwnedAnyV2Route::RemovedKeyCheckedMiss),
    );
    if removed_key_checked_miss_i32_actual.len() != expected_removed_key_checked_miss_i32 {
        return Err(
            "RemovedKeyCheckedMiss does not have exactly one canonical i32 miss receipt".into(),
        );
    }
    Ok(())
}

fn checked_managed_any_v2_label_target_supported(
    checked: &crate::core::CheckedProgram,
    ty: &crate::core::ResolvedTypeId,
) -> bool {
    use crate::core::ResolvedType as Type;

    let Some(Type::Nominal {
        item, arguments, ..
    }) = checked.resolved_types().get(ty)
    else {
        return false;
    };
    if item.as_str().strip_prefix("type:") != Some("Label") || !arguments.is_empty() {
        return false;
    }
    let Some(definition) = checked.type_def("Label") else {
        return false;
    };
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    definition.kind == crate::core::ResolvedTypeKind::Record
        && definition.generic_parameters.is_empty()
        && has_only_field_named(&definition.fields, "text")
        && definition.field_ids.len() == 1
        && definition
            .field_ids
            .get("text")
            .and_then(|field| checked.resolved_field_type(field))
            == Some(&string_type)
}
