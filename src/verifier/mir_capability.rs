//! Static capability gate for the verifier's Canonical MIR consumer.
//!
//! `verify_mir` is intentionally a contract engine: functions without an
//! obligation are not symbolically executed.  That is useful for keeping the
//! proof engine small, but it must not be mistaken for whole-program
//! capability.  The default route therefore runs this structural gate over
//! every function before admitting an island.  The gate consumes only the
//! canonical MIR program and its TypeDesc catalog; it never asks the frontend
//! to rediscover a type, ownership, or call fact.

use std::collections::{BTreeSet, HashSet};

use crate::core::ir::{
    ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedTypeId, ResolvedUnaryOp,
};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{
    MirAbiClass, MirGlueKind, MirGlueOperation, MirLayout, MirOwnership, MirTypeCatalog,
    MirTypeKind,
};
use crate::core::mir::{
    MirContractKind, MirFunction, MirGenericInstanceContract, MirInstance, MirInstructionKind,
    MirManagedAnyOperation, MirProjection, MirProofLoopEdgeKind, MirSwitchCase, MirTerminator,
    MirValueId,
};
use crate::verifier::checked_power::{CheckedPowerProfile, POWER_PROFILE_0_1_11_COMPAT};

/// Validate the subset of canonical MIR that the MIR verifier can safely
/// coexist with on the default route.
///
/// This is deliberately a capability result rather than a verification
/// result.  It answers whether every compiled MIR shape has a verifier
/// interpretation.  A function with no contracts is still scanned; otherwise
/// an unsupported body could silently enter a native/bytecode island merely
/// because the verifier had no obligation to print.
pub fn validate_mir_capabilities(program: &MirProgram) -> Result<(), Vec<String>> {
    if super::mir::mir_program_has_ordinary_map_owned_any_v2_direct_print_result_signal(program) {
        return Err(vec![format!(
            "{}: nested Result DirectPrint requires its exact CheckedProgram-bound L2 verifier capability",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )]);
    }
    if mir_program_has_ordinary_map_direct_tuple_get_unpack_l2_projection(program) {
        return Err(vec![format!(
            "{}: direct tuple Get L2 requires its exact CheckedProgram-aware semantic-projection capability",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )]);
    }
    if mir_program_has_ordinary_map_owned_get_any_tuple_l2_projection(program) {
        return Err(vec![format!(
            "{}: legacy tuple GetAny L2 requires its exact CheckedProgram-aware VerifierOnly capability",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )]);
    }
    if mir_program_has_ordinary_map_set_get_any_result_i32_signal(program) {
        return Err(vec![format!(
            "{}: ordinary Map GetAnyResult requires its exact CheckedProgram-aware L2 capability; generic MIR capability validation is closed",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )]);
    }
    if super::mir::mir_program_has_ordinary_map_set_get_any_i32_signal(program) {
        return Err(vec![format!(
            "{}: ordinary Map Set/GetAny-i32 requires its exact CheckedProgram-aware L2 capability; generic MIR capability validation is closed",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )]);
    }
    if super::mir::mir_program_has_ordinary_map_set_get_string_result_signal(program) {
        return Err(vec![
            format!(
                "{}: ordinary Map Set/Get<String> Result operations require the exact CheckedProgram-bound L2 capability; generic MIR capability validation is closed",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ),
        ]);
    }
    if super::mir::mir_program_has_ordinary_map_owned_any_v2_map_set_drop_control_result_signal(
        program,
    ) {
        return Err(vec![format!(
            "{}: MapSetDropControlResult requires its exact CheckedProgram-bound verifier capability; generic MIR capability validation is closed",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )]);
    }
    if mir_program_has_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_signal(
        program,
    ) {
        return Err(vec![format!(
            "{}: ValuesLocalIndexI32 Result-branch requires its exact CheckedProgram-bound verifier capability; generic MIR capability validation is closed",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )]);
    }
    validate_mir_capabilities_with_options(program, false, false, false, false, false, false, false)
}

fn mir_program_has_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_signal(
    program: &MirProgram,
) -> bool {
    program.functions().values().any(|function| {
        function
            .ownership
            .ordinary_map_owned_any_v2_cfg_receipt()
            .is_some_and(|receipt| {
                receipt
                    .values_local_index_i32_result_branch_receipt
                    .is_some()
            })
    })
}

fn mir_program_has_ordinary_map_direct_tuple_get_unpack_l2_projection(
    program: &MirProgram,
) -> bool {
    program.functions().values().any(|function| {
        function
            .ownership
            .ordinary_map_direct_tuple_get_unpack_l2_projection()
            .is_some()
            || function
                .ownership
                .ordinary_map_direct_tuple_get_unpack_l1_receipt()
                .is_some()
    })
}

/// Exact CheckedProgram-aware verifier capability for the direct tuple Get
/// semantic projection. It validates a non-executable proof object and
/// explicitly rejects contract obligations; this profile emits no ordinary
/// verification verdict and proves no runtime ownership theorem.
pub(crate) fn validate_mir_capabilities_with_ordinary_map_direct_tuple_get_unpack_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    let profile = crate::core::mir::ORDINARY_MAP_DIRECT_TUPLE_GET_UNPACK_L2_VERIFIER_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "direct tuple Get L2 capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "direct tuple Get L2 route receipt rejected: {message}"
            )]
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_direct_tuple_get_unpack_l2_verifier_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "direct tuple Get L2 Checker/MIR projection rejected: {message}"
            )]
        })?;
    if &expected != route_receipt {
        return Err(vec![
            "direct tuple Get L2 route receipt differs from the exact Checker/MIR projection"
                .into(),
        ]);
    }
    let owner = crate::core::NodeId("function:main".into());
    let callable = checked
        .callable(&owner)
        .ok_or_else(|| vec!["direct tuple Get L2 Checker callable disappeared".into()])?;
    if callable.has_proof_obligations() {
        return Err(vec![
            "direct tuple Get L2 validates source semantics only and cannot discharge contract obligations".into(),
        ]);
    }
    Ok(())
}

fn mir_program_has_ordinary_map_owned_get_any_tuple_l2_projection(program: &MirProgram) -> bool {
    program.functions().values().any(|function| {
        function
            .ownership
            .ordinary_map_owned_get_any_tuple_l2_projection()
            .is_some()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    instruction
                        .id
                        .as_str()
                        .starts_with("inst:ordinary_map_owned_get_any_tuple_l2:")
                })
            })
    })
}

/// Exact CheckedProgram-aware verifier capability for the fixed legacy tuple
/// GetAny L2 proof projection. It admits only the isolated Nop receipt graph;
/// any actual proof obligation is rejected because tuple ownership semantics
/// are not modeled by this profile.
pub(crate) fn validate_mir_capabilities_with_ordinary_map_owned_get_any_tuple_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_GET_ANY_TUPLE_L2_VERIFIER_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "legacy tuple GetAny L2 capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "legacy tuple GetAny L2 route receipt rejected: {message}"
            )]
        })?;
    let expected = crate::core::mir::validate_ordinary_map_owned_get_any_tuple_l2_verifier_route(
        checked, program,
    )
    .map_err(|message| {
        vec![format!(
            "legacy tuple GetAny L2 Checker/MIR receipt rejected: {message}"
        )]
    })?;
    if &expected != route_receipt {
        return Err(vec![
            "legacy tuple GetAny L2 route receipt differs from the exact Checker/MIR projection"
                .into(),
        ]);
    }
    let owner = crate::core::NodeId("function:main".into());
    let callable = checked
        .callable(&owner)
        .ok_or_else(|| vec!["legacy tuple GetAny L2 Checker callable disappeared".into()])?;
    if callable.has_proof_obligations() {
        return Err(vec![
            "legacy tuple GetAny L2 capability cannot prove contracts because the tuple value is receipt-only".into(),
        ]);
    }
    Ok(())
}

fn mir_program_has_ordinary_map_set_get_any_result_i32_signal(program: &MirProgram) -> bool {
    program.functions().values().any(|function| {
        function
            .ownership
            .ordinary_map_set_get_any_result_i32_lineage_receipt()
            .is_some()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        &instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedAnyV2GetAnyResult { .. }
                    )
                })
            })
    })
}

/// Validate the isolated Checker-aware L2 verifier capability for explicit
/// ordinary Map NewResult. The exact source receipt, MIR graph, TypeDesc
/// catalog, contract lowering, proof worklist, and complete checked-source
/// census are replayed by the Core route validator. This closed graph has no
/// generic Map operations outside the exact New/Result-switch/Size/Drop shape,
/// so it does not inherit broad owner-v2 readiness.
pub(crate) fn validate_mir_capabilities_with_ordinary_map_new_result_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    let profile = crate::core::mir::ORDINARY_MAP_NEW_RESULT_L2_VERIFIER_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "ordinary Map NewResult L2 capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    let expected =
        crate::core::mir::validate_ordinary_map_new_result_l2_verifier_route(checked, program)
            .map_err(|message| {
                vec![format!(
                    "ordinary Map NewResult L2 Checker/MIR route rejected: {message}"
                )]
            })?;
    if &expected != route_receipt {
        return Err(vec![
            "ordinary Map NewResult L2 route receipt differs from the exact Checker/MIR route"
                .into(),
        ]);
    }
    let owner = crate::core::NodeId("function:main".into());
    let function = program
        .functions()
        .get(&owner)
        .ok_or_else(|| vec!["ordinary Map NewResult L2 MIR is missing function:main".into()])?;
    function.validate().map_err(|errors| {
        vec![format!(
            "ordinary Map NewResult L2 MIR structure rejected: {errors:?}"
        )]
    })?;
    crate::core::mir::validate_ordinary_map_new_result_types(function, program.type_catalog())
        .map_err(|message| {
            vec![format!(
                "ordinary Map NewResult L2 TypeDesc rejected: {message}"
            )]
        })
}

/// Validate the nested New/Set Result → GetAny → stdout profile as a
/// Checker-bound, whole-program L2 capability. The route builder replays the
/// source CFG receipt and compares the complete expected MIR graph, including
/// Result tags, root identities, payload ABI, pair drop, branches, and glue.
/// This profile is neutral only when the checked program has no obligations;
/// it never authorizes symbolic proofs of the Result tag or printed output.
pub(crate) fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_direct_print_result_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;

    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_RESULT_L2_VERIFIER_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "nested Result DirectPrint capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "nested Result DirectPrint route receipt rejected: {message}"
            )]
        })?;

    let execution_receipt =
        crate::core::mir::validate_ordinary_map_owned_any_v2_direct_print_result_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "nested Result DirectPrint Checker/MIR route rejected: {message}"
            )]
        })?;
    let expected = program.route_receipt(profile);
    if execution_receipt.mir_digest != expected.mir_digest || &expected != route_receipt {
        return Err(vec![
            "nested Result DirectPrint L2 route receipt differs from the exact CheckedProgram/MIR graph identity"
                .into(),
        ]);
    }

    let owner = crate::core::NodeId("function:main".into());
    let callable = checked
        .callable(&owner)
        .ok_or_else(|| vec!["nested Result DirectPrint Checker callable disappeared".into()])?;
    let checker_receipt = callable
        .resources
        .ordinary_map_owned_any_v2_direct_print_result_receipt
        .as_ref()
        .ok_or_else(|| vec!["nested Result DirectPrint Checker receipt is missing".into()])?;
    let function = program
        .functions()
        .get(&owner)
        .ok_or_else(|| vec!["nested Result DirectPrint MIR is missing function:main".into()])?;
    if program.functions().len() != 1
        || function
            .ownership
            .ordinary_map_owned_any_v2_direct_print_result_receipt()
            != Some(checker_receipt)
    {
        return Err(vec![
            "nested Result DirectPrint MIR must contain only main and embed its exact Checker receipt"
                .into(),
        ]);
    }
    function.validate().map_err(|errors| {
        vec![format!(
            "nested Result DirectPrint MIR structure rejected: {errors:?}"
        )]
    })?;

    let expected_catalog =
        MirTypeCatalog::for_ordinary_map_owned_any_v2_direct_print_result(checked, checker_receipt)
            .map_err(|errors| {
                vec![format!(
                    "nested Result DirectPrint exact TypeDesc/glue catalog rejected: {errors:?}"
                )]
            })?;
    if program.type_catalog() != &expected_catalog {
        return Err(vec![
            "nested Result DirectPrint TypeDesc catalog differs from the exact Checker payload, tuple, root, and glue contract"
                .into(),
        ]);
    }

    let proof_bearing_owners = checked
        .callables()
        .iter()
        .filter(|(_, callable)| callable.has_proof_obligations())
        .map(|(owner, _)| owner.0.as_str())
        .collect::<Vec<_>>();
    if !proof_bearing_owners.is_empty() {
        return Err(vec![format!(
            "nested Result DirectPrint L2 route is neutral-only and refuses proof obligations on: {}",
            proof_bearing_owners.join(", ")
        )]);
    }
    Ok(())
}

/// Validate the exact CheckedProgram-bound MapSetDropControl Result-branch
/// capability. The route validators replay the three-way Checker receipt and
/// its complete expected MIR graph before the shared capability scanner is
/// given the narrow SetAnyResult allowance used by this route. Generic MIR
/// capability entry points remain closed to the Result carrier.
pub(crate) fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;

    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_RESULT_BRANCH_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "MapSetDropControlResult capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "MapSetDropControlResult route receipt rejected: {message}"
            )]
        })?;

    let checked_receipt = crate::core::mir::
        validate_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "MapSetDropControlResult Checker/MIR route rejected: {message}"
            )]
        })?;
    let mir_receipt = crate::core::mir::
        validate_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_mir_route(program)
            .map_err(|message| {
                vec![format!("MapSetDropControlResult MIR route rejected: {message}")]
            })?;
    if checked_receipt != mir_receipt || &checked_receipt != route_receipt {
        return Err(vec![
            "MapSetDropControlResult route receipt differs from the exact CheckedProgram/MIR graph identity"
                .into(),
        ]);
    }

    let owner = crate::core::NodeId("function:main".into());
    let function = program
        .functions()
        .get(&owner)
        .ok_or_else(|| vec!["MapSetDropControlResult MIR is missing function:main".into()])?;
    if program.functions().len() != 1 {
        return Err(vec![
            "MapSetDropControlResult MIR must contain only the exact main callable".into(),
        ]);
    }
    function.validate().map_err(|errors| {
        vec![format!(
            "MapSetDropControlResult MIR structure rejected: {errors:?}"
        )]
    })?;
    program.validate_proof_source_census().map_err(|error| {
        vec![format!(
            "MapSetDropControlResult proof source census rejected: {error:?}"
        )]
    })?;

    let proof_bearing_owners = checked
        .callables()
        .iter()
        .filter(|(_, callable)| callable.has_proof_obligations())
        .map(|(owner, _)| owner.0.as_str())
        .collect::<Vec<_>>();
    if !proof_bearing_owners.is_empty() {
        return Err(vec![format!(
            "MapSetDropControlResult is neutral-only and refuses proof obligations on: {}",
            proof_bearing_owners.join(", ")
        )]);
    }

    // The MIR scanner's SetAnyResult switch is private to this exact route:
    // the CheckedProgram receipt, MIR-only replay, and route identity above
    // have already proved the allowed source shape. No raw profile reaches
    // this call through the generic capability API.
    validate_mir_capabilities_with_options_and_keys_snapshot_and_computed_string_key_owner_chain(
        program, false, false, false, false, false, false, false, false, false, false, false,
        false, None, true, false,
    )
}

pub(crate) fn validate_mir_capabilities_with_ordinary_map_set_get_i32_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    let profile = crate::core::mir::ORDINARY_MAP_SET_GET_I32_L2_VERIFIER_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!("ordinary Map Set/Get-i32 L2 capability requires exact route profile '{profile}', found '{}'", route_receipt.profile)]);
    }
    let expected =
        crate::core::mir::validate_ordinary_map_set_get_i32_l2_verifier_route(checked, program)
            .map_err(|message| {
                vec![format!(
                    "ordinary Map Set/Get-i32 L2 Checker/MIR route rejected: {message}"
                )]
            })?;
    if &expected != route_receipt {
        return Err(vec![
            "ordinary Map Set/Get-i32 L2 route receipt differs from the exact Checker/MIR route"
                .into(),
        ]);
    }
    let owner = crate::core::NodeId("function:main".into());
    let function = program
        .functions()
        .get(&owner)
        .ok_or_else(|| vec!["ordinary Map Set/Get-i32 L2 MIR is missing function:main".into()])?;
    function.validate().map_err(|errors| {
        vec![format!(
            "ordinary Map Set/Get-i32 L2 MIR structure rejected: {errors:?}"
        )]
    })?;
    crate::core::mir::validate_ordinary_map_set_get_i32_result_types(
        function,
        program.type_catalog(),
    )
    .map_err(|message| {
        vec![format!(
            "ordinary Map Set/Get-i32 L2 TypeDesc rejected: {message}"
        )]
    })
}

/// Validate the dedicated fallible New/Set/generic-GetAny/unpack-i32 L2
/// projection. This is a capability preflight only: contract-free L1 source
/// is valid here, while the verifier entry separately requires and discharges
/// the exact Checker-bound Ensures obligation before returning a proof.
pub(crate) fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_set_get_any_i32_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;

    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_L2_VERIFIER_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "ordinary Map Set/GetAny-i32 L2 capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "ordinary Map Set/GetAny-i32 L2 route receipt rejected: {message}"
            )]
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_set_get_any_i32_l2_verifier_route(checked, program)
            .map_err(|message| {
                vec![format!(
                    "ordinary Map Set/GetAny-i32 L2 Checker/MIR route rejected: {message}"
                )]
            })?;
    if &expected != route_receipt {
        return Err(vec![
            "ordinary Map Set/GetAny-i32 L2 route receipt differs from the exact Checker/MIR route"
                .into(),
        ]);
    }

    let owner = crate::core::NodeId("function:main".into());
    let callable = checked.callable(&owner).ok_or_else(|| {
        vec!["ordinary Map Set/GetAny-i32 L2 has no checked function:main".into()]
    })?;
    let source_receipt = callable
        .resources
        .ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt
        .as_ref()
        .ok_or_else(|| vec!["CheckedProgram has no exact Set/GetAny-i32 source receipt".into()])?;
    crate::core::cfg::validate_ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt(
        source_receipt,
        &callable.cfg,
        &callable.body,
        &callable.signature,
        checked.resolved_types(),
        &callable
            .resources
            .ordinary_map_owned_any_v2_operation_attempts,
    )
    .map_err(|message| {
        vec![format!(
            "ordinary Map Set/GetAny-i32 Checker source receipt rejected: {message}"
        )]
    })?;

    let function = program.functions().get(&owner).ok_or_else(|| {
        vec!["ordinary Map Set/GetAny-i32 L2 MIR is missing function:main".into()]
    })?;
    function.validate().map_err(|errors| {
        vec![format!(
            "ordinary Map Set/GetAny-i32 L2 MIR structure rejected: {errors:?}"
        )]
    })?;
    let expected_catalog = crate::core::mir::types::MirTypeCatalog::for_ordinary_map_set_get_any_i32_result(
        checked,
        source_receipt,
    )
    .map_err(|errors| {
        vec![format!(
            "ordinary Map Set/GetAny-i32 L2 could not rebuild the Checker TypeDesc catalog: {errors:?}"
        )]
    })?;
    if program.type_catalog() != &expected_catalog {
        return Err(vec![
            "ordinary Map Set/GetAny-i32 L2 TypeDesc catalog differs from the exact CheckedProgram"
                .into(),
        ]);
    }
    crate::core::mir::validate_ordinary_map_set_get_any_i32_result_types(
        function,
        program.type_catalog(),
    )
    .map_err(|message| {
        vec![format!(
            "ordinary Map Set/GetAny-i32 L2 TypeDesc rejected: {message}"
        )]
    })?;

    program.validate_proof_source_census().map_err(|error| {
        vec![format!(
            "ordinary Map Set/GetAny-i32 L2 proof source census rejected: {error:?}"
        )]
    })?;
    let missing_verification_owners = program
        .proof_source_census()
        .missing_mir_verification_owners();
    if !missing_verification_owners.is_empty() {
        return Err(vec![format!(
            "ordinary Map Set/GetAny-i32 L2 projection omits checked callables with proof obligations: {}",
            missing_verification_owners
                .iter()
                .map(|owner| owner.0.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )]);
    }
    program
        .validate_owner_proof_worklist(&owner)
        .map(|_| ())
        .map_err(|error| {
            vec![format!(
                "ordinary Map Set/GetAny-i32 L2 proof worklist rejected: {error:?}"
            )]
        })
}

/// Validate the dedicated Checker-bound capability for Result-valued
/// `map_get_any`. This route uses an independent source receipt and MIR
/// profile from the legacy tuple-valued `map_get` route. Capability admission
/// validates projection only; the proof entry separately requires and
/// discharges the exact Ensures obligation.
pub(crate) fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_set_get_any_result_i32_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;

    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_L2_VERIFIER_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "ordinary Map GetAnyResult L2 capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    let admission = crate::core::mir::classify_canonical_mir_route_admission(checked);
    if admission.ordinary_map_owned_any_v2
        != crate::core::mir::OrdinaryMapOwnedAnyV2Admission::SetGetAnyResultI32
    {
        return Err(vec![
            "CheckedProgram does not admit the independent Result-valued GetAny-i32 route".into(),
        ]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "ordinary Map GetAnyResult L2 route receipt rejected: {message}"
            )]
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_set_get_any_result_i32_l2_verifier_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "ordinary Map GetAnyResult L2 Checker/MIR route rejected: {message}"
            )]
        })?;
    if &expected != route_receipt {
        return Err(vec![
            "ordinary Map GetAnyResult L2 route receipt differs from the exact CheckedProgram/MIR route"
                .into(),
        ]);
    }

    let owner = crate::core::NodeId("function:main".into());
    let callable = checked
        .callable(&owner)
        .ok_or_else(|| vec!["ordinary Map GetAnyResult L2 has no checked function:main".into()])?;
    let source_receipt = callable
        .resources
        .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt
        .as_ref()
        .ok_or_else(|| {
            vec!["CheckedProgram has no exact Result-valued GetAny-i32 source receipt".into()]
        })?;
    if checked.callables().values().any(|candidate| {
        candidate
            .resources
            .ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt
            .is_some()
    }) {
        return Err(vec![
            "Result-valued GetAny-i32 cannot use the legacy tuple-valued map_get receipt".into(),
        ]);
    }
    if source_receipt.owner != owner
        || source_receipt.profile_version
            != crate::core::OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageProfileVersion::V1
        || source_receipt.get_api != crate::core::OrdinaryMapOwnedAnyV2GetApi::MapGetAnyResult
    {
        return Err(vec![
            "CheckedProgram Result-valued GetAny-i32 source receipt has the wrong owner, version, or API identity"
                .into(),
        ]);
    }
    crate::core::cfg::validate_ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt(
        source_receipt,
        &callable.cfg,
        &callable.body,
        &callable.signature,
        checked.resolved_types(),
        &callable
            .resources
            .ordinary_map_owned_any_v2_operation_attempts,
    )
    .map_err(|message| {
        vec![format!(
            "ordinary Map GetAnyResult Checker source receipt rejected: {message}"
        )]
    })?;

    let function = program
        .functions()
        .get(&owner)
        .ok_or_else(|| vec!["ordinary Map GetAnyResult L2 MIR is missing function:main".into()])?;
    function.validate().map_err(|errors| {
        vec![format!(
            "ordinary Map GetAnyResult L2 MIR structure rejected: {errors:?}"
        )]
    })?;
    let expected_catalog =
        crate::core::mir::types::MirTypeCatalog::for_ordinary_map_set_get_any_result_i32(
            checked,
            source_receipt,
        )
        .map_err(|errors| {
            vec![format!(
                "ordinary Map GetAnyResult L2 could not rebuild the Checker TypeDesc catalog: {errors:?}"
            )]
        })?;
    if program.type_catalog() != &expected_catalog {
        return Err(vec![
            "ordinary Map GetAnyResult L2 TypeDesc catalog differs from the exact CheckedProgram"
                .into(),
        ]);
    }

    program.validate_proof_source_census().map_err(|error| {
        vec![format!(
            "ordinary Map GetAnyResult L2 proof source census rejected: {error:?}"
        )]
    })?;
    let missing_verification_owners = program
        .proof_source_census()
        .missing_mir_verification_owners();
    if !missing_verification_owners.is_empty() {
        return Err(vec![format!(
            "ordinary Map GetAnyResult L2 projection omits checked callables with proof obligations: {}",
            missing_verification_owners
                .iter()
                .map(|owner| owner.0.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )]);
    }
    if callable.has_proof_obligations() {
        program
            .validate_owner_proof_worklist(&owner)
            .map_err(|error| {
                vec![format!(
                    "ordinary Map GetAnyResult L2 proof worklist rejected: {error:?}"
                )]
            })?;
    }
    Ok(())
}

pub(crate) fn validate_mir_capabilities_with_ordinary_map_set_get_string_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    let profile = crate::core::mir::ORDINARY_MAP_SET_GET_STRING_L2_VERIFIER_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "ordinary Map Set/Get-String L2 capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    let expected =
        crate::core::mir::validate_ordinary_map_set_get_string_l2_verifier_route(checked, program)
            .map_err(|message| {
                vec![format!(
                    "ordinary Map Set/Get-String L2 Checker/MIR route rejected: {message}"
                )]
            })?;
    if &expected != route_receipt {
        return Err(vec![
            "ordinary Map Set/Get-String L2 route receipt differs from the exact Checker/MIR route"
                .into(),
        ]);
    }
    let owner = crate::core::NodeId("function:main".into());
    let function = program.functions().get(&owner).ok_or_else(|| {
        vec!["ordinary Map Set/Get-String L2 MIR is missing function:main".into()]
    })?;
    function.validate().map_err(|errors| {
        vec![format!(
            "ordinary Map Set/Get-String L2 MIR structure rejected: {errors:?}"
        )]
    })?;
    crate::core::mir::validate_ordinary_map_set_get_string_result_types(
        function,
        program.type_catalog(),
    )
    .map_err(|message| {
        vec![format!(
            "ordinary Map Set/Get-String L2 TypeDesc rejected: {message}"
        )]
    })
}

/// Validate the exact Checker-bound SetResult L2 verifier route. Core replays
/// the complete graph and source census; the symbolic verifier interprets the
/// Result status/tag values under that receipt.
pub(crate) fn validate_mir_capabilities_with_ordinary_map_set_result_l2_verifier_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    let profile = crate::core::mir::ORDINARY_MAP_SET_RESULT_L2_VERIFIER_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "ordinary Map SetResult L2 capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    let expected =
        crate::core::mir::validate_ordinary_map_set_result_l2_verifier_route(checked, program)
            .map_err(|message| {
                vec![format!(
                    "ordinary Map SetResult L2 Checker/MIR route rejected: {message}"
                )]
            })?;
    if &expected != route_receipt {
        return Err(vec![
            "ordinary Map SetResult L2 route receipt differs from the exact Checker/MIR route"
                .into(),
        ]);
    }
    let owner = crate::core::NodeId("function:main".into());
    let function = program
        .functions()
        .get(&owner)
        .ok_or_else(|| vec!["ordinary Map SetResult L2 MIR is missing function:main".into()])?;
    function.validate().map_err(|errors| {
        vec![format!(
            "ordinary Map SetResult L2 MIR structure rejected: {errors:?}"
        )]
    })?;
    crate::core::mir::validate_ordinary_map_set_result_types(function, program.type_catalog())
        .map_err(|message| {
            vec![format!(
                "ordinary Map SetResult L2 TypeDesc rejected: {message}"
            )]
        })
}

/// Validate DirectPrint only when its exact canonical route receipt and
/// Checker-derived Map/Any output proof are both present. Generic MIR
/// capability checks intentionally continue to reject this output operation.
pub fn validate_mir_capabilities_with_direct_print_route_receipt(
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    if route_receipt.profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE {
        return Err(vec![format!(
            "DirectPrint capability requires exact route profile '{}', found '{}'",
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE,
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| vec![format!("DirectPrint route receipt rejected: {message}")])?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_direct_print_mir_route(program)
        .map_err(|message| vec![format!("DirectPrint operation receipt rejected: {message}")])?;
    validate_mir_capabilities_with_options(program, false, true, false, false, false, false, false)
}

/// Validate the narrow owner-v2 IsEmpty route only when both the Checker
/// source seal and the exact MIR route receipt are present. Generic MIR
/// capability checks intentionally reject IsEmpty because a Boolean result
/// alone does not prove which checked owner operation produced it.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_is_empty_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
        })?;
    if route_receipt.profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE {
        return Err(vec![format!(
            "IsEmpty capability requires exact route profile '{}', found '{}'",
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE,
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| vec![format!("IsEmpty route receipt rejected: {message}")])?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_is_empty_route(checked, program)
        .map_err(|message| vec![format!("IsEmpty Checker/MIR receipt rejected: {message}")])?;
    validate_mir_capabilities_with_options(program, false, false, true, false, false, false, false)
}

/// Validate the exact mixed-payload RecordUnit profile after its Checker and
/// route receipts have been joined. The narrow route permits only the one
/// source-bound String print; Map/Any actions and ownership remain covered by
/// the normal structural capability scan.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_record_unit_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "RecordUnit capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| vec![format!("RecordUnit route receipt rejected: {message}")])?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_record_unit_route(checked, program)
        .map_err(|message| {
            vec![format!(
                "RecordUnit Checker/MIR receipt rejected: {message}"
            )]
        })?;
    validate_mir_capabilities_with_options(program, false, true, false, false, false, false, false)
}

/// Validate the narrow Checker-sealed branch-local persistent Set diamond.
/// Its source proof, exact route receipt, and MIR operation census are all
/// required before the ordinary structural capability gate runs. The gate
/// remains strict for every other AnyOwner-v2 profile.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    if route_receipt.profile
        != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE
    {
        return Err(vec![format!(
            "BranchLocalSetDiamond capability requires exact route profile '{}', found '{}'",
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE,
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "BranchLocalSetDiamond route receipt rejected: {message}"
            )]
        })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
        checked, program,
    )
    .map_err(|message| {
        vec![format!(
            "BranchLocalSetDiamond Checker/MIR receipt rejected: {message}"
        )]
    })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_mir_route(
        program,
    )
    .map_err(|message| {
        vec![format!(
            "BranchLocalSetDiamond MIR census rejected: {message}"
        )]
    })?;
    validate_mir_capabilities_with_options(program, false, false, false, false, false, false, false)
}

/// Validate the exact Checker-sealed i32 zero-miss route. General owner-v2
/// receipts also support canonical i32 misses; this narrower route proves
/// the exact stored-zero versus missing-zero observation profile.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    if route_receipt.profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE {
        return Err(vec![format!(
            "ZeroMissI32 capability requires exact route profile '{}', found '{}'",
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE,
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| vec![format!("ZeroMissI32 route receipt rejected: {message}")])?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_route(checked, program)
        .map_err(|message| {
            vec![format!(
                "ZeroMissI32 Checker/MIR receipt rejected: {message}"
            )]
        })?;
    crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_mir_route(program)
        .map_err(|message| vec![format!("ZeroMissI32 MIR shape rejected: {message}")])?;
    validate_ordinary_map_owned_any_v2_zero_miss_i32_census(program)
        .map_err(|message| vec![format!("ZeroMissI32 capability census rejected: {message}")])?;
    validate_mir_capabilities_with_options(program, false, false, false, true, false, false, false)
}

/// Validate the narrow cross-call GetOrDefault<i32> profile only after joining
/// the CheckedProgram's helper/callsite witness to the typed MIR receipt. The
/// generic capability API deliberately keeps this fused operation closed.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_I32_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "GetOrDefault<i32> capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "GetOrDefault<i32> route receipt rejected: {message}"
            )]
        })?;
    let expected_route =
        crate::core::mir::validate_ordinary_map_owned_any_v2_get_or_default_i32_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "GetOrDefault<i32> Checker/MIR route rejected: {message}"
            )]
        })?;
    if &expected_route != route_receipt {
        return Err(vec![
            "GetOrDefault<i32> route receipt differs from its exact Checker/MIR operation census"
                .into(),
        ]);
    }
    let mir_receipt =
        super::mir::validate_ordinary_map_owned_any_v2_get_or_default_i32_verifier_receipt(program)
            .map_err(|message| {
                vec![format!(
                    "GetOrDefault<i32> MIR/TypeDesc census rejected: {message}"
                )]
            })?;
    validate_ordinary_map_owned_any_v2_get_or_default_i32_checked_bridge(
        checked,
        program,
        &mir_receipt,
    )
    .map_err(|message| {
        vec![format!(
            "GetOrDefault<i32> CheckedProgram bridge rejected: {message}"
        )]
    })?;
    validate_mir_capabilities_with_options_and_keys_snapshot(
        program, false, // Typed FFI proofs.
        false, // Direct print.
        false, // V2 IsEmpty.
        false, // V2 zero-miss i32.
        false, // V2 Values producer/caller.
        false, // V2 Map-root producer/caller.
        false, // V2 Keys snapshot.
        false, // V2 removed-key checked miss.
        false, // V2 Values cross-call String caller.
        true,  // This exact GetOrDefault<i32> profile.
        false, // GetOrDefault<String> remains separately sealed.
        false, // DynamicKeysGet remains separately sealed.
        None,
    )
}

/// Validate the String GetOrDefault profile only after joining the exact
/// CheckedProgram helper receipt to its separate MIR operation and i32-miss
/// sentinel receipt. The generic capability gate keeps this operation closed.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_get_or_default_string_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "GetOrDefault<String> capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "GetOrDefault<String> route receipt rejected: {message}"
            )]
        })?;
    let expected_route =
        crate::core::mir::validate_ordinary_map_owned_any_v2_get_or_default_string_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "GetOrDefault<String> Checker/MIR route rejected: {message}"
            )]
        })?;
    if &expected_route != route_receipt {
        return Err(vec![
            "GetOrDefault<String> route receipt differs from its exact Checker/MIR operation census"
                .into(),
        ]);
    }
    let mir_receipt =
        super::mir::validate_ordinary_map_owned_any_v2_get_or_default_string_verifier_receipt(
            program,
        )
        .map_err(|message| {
            vec![format!(
                "GetOrDefault<String> MIR/TypeDesc census rejected: {message}"
            )]
        })?;
    validate_ordinary_map_owned_any_v2_get_or_default_string_checked_bridge(
        checked,
        program,
        &mir_receipt,
    )
    .map_err(|message| {
        vec![format!(
            "GetOrDefault<String> CheckedProgram bridge rejected: {message}"
        )]
    })?;
    validate_mir_capabilities_with_options_and_keys_snapshot(
        program, false, // Typed FFI proofs.
        false, // Direct print.
        false, // V2 IsEmpty.
        false, // V2 zero-miss i32.
        false, // V2 Values producer/caller.
        false, // V2 Map-root producer/caller.
        false, // V2 Keys snapshot.
        false, // V2 removed-key checked miss.
        false, // V2 Values cross-call String caller.
        false, // GetOrDefault<i32>.
        true,  // This exact GetOrDefault<String> profile.
        false, // DynamicKeysGet remains separately sealed.
        None,
    )
}

fn validate_ordinary_map_owned_any_v2_get_or_default_string_checked_bridge(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    mir_receipt: &crate::core::mir::OrdinaryMapOwnedAnyV2GetOrDefaultStringMirReceipt,
) -> Result<(), String> {
    let receipts = checked
        .resource_analyses()
        .values()
        .filter_map(|analysis| {
            analysis
                .ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()?
                .get_or_default_string_receipt
                .as_ref()
        })
        .collect::<Vec<_>>();
    let [checker] = receipts.as_slice() else {
        return Err(format!(
            "expected exactly one CheckedProgram GetOrDefault<String> bridge, found {}",
            receipts.len()
        ));
    };
    if checker.profile_version.canonical_text() != mir_receipt.profile
        || checker.caller_owner != mir_receipt.owner
        || checker.call_node != mir_receipt.callsite
        || checker.call_target != mir_receipt.call_target
        || checker.call_arguments.as_slice() != mir_receipt.argument_order.as_slice()
        || checker.call_arguments.first() != Some(&mir_receipt.caller_root_argument)
        || checker.call_argument_types.len() != 3
        || checker.call_argument_types[0] != checker.caller_root_type
        || &checker.caller_root_type != mir_receipt.checker_caller_root_type()
        || checker.call_type_argument != mir_receipt.type_argument
        || checker.call_result_type != mir_receipt.result_type
        || checker.caller_initial_root_local != mir_receipt.caller_initial_root_local
        || checker.caller_root_local != mir_receipt.caller_root_local
        || checker.caller_result_local != mir_receipt.caller_result_local
        || checker.caller_set_value_local != mir_receipt.caller_set_value_local
        || checker.caller_set_value_binding_point != mir_receipt.caller_set_value_binding_point
        || checker.caller_set_value_drop_point != mir_receipt.caller_set_value_drop_point
        || checker.caller_set_value_drop_place_index
            != mir_receipt.caller_set_value_drop_place_index
        || checker.caller_set_value_drop_point.is_some()
            != mir_receipt.caller_set_value_drop_instruction.is_some()
        || checker.caller_set_value_type != mir_receipt.key_type
        || checker.caller_default_local != mir_receipt.caller_default_local
        || checker.caller_default_binding_point != mir_receipt.caller_default_binding_point
        || checker.caller_call_binding_point != mir_receipt.caller_call_binding_point
        || checker.caller_initial_root_drop_point != mir_receipt.caller_initial_root_drop_point
        || checker.caller_root_drop_point != mir_receipt.caller_root_drop_point
        || checker.helper_map_parameter != mir_receipt.helper_map_parameter
        || checker.helper_key_parameter != mir_receipt.helper_key_parameter
        || checker.helper_default_parameter != mir_receipt.helper_default_parameter
        || checker.helper_map_permission != crate::core::Permission::View
        || checker.call_argument_types.get(1) != Some(&mir_receipt.key_type)
        || checker.call_argument_types.get(2) != Some(&mir_receipt.default_type)
        || checker.helper_pair_binding_point != mir_receipt.helper_pair_binding_point
        || checker.helper_found_condition != mir_receipt.helper_found_condition
        || checker.helper_found_arm_block != mir_receipt.helper_found_arm_block
        || checker.helper_default_arm_block != mir_receipt.helper_default_arm_block
        || checker.helper_payload_projection != mir_receipt.helper_payload_projection
        || checker.helper_unpack_cast != mir_receipt.helper_unpack_cast
        || checker.helper_selected_binding_point != mir_receipt.helper_selected_binding_point
        || checker.helper_selected_local != mir_receipt.helper_selected_local
        || checker.helper_default_load != mir_receipt.helper_default_load
        || checker.helper_pair_drop_point != mir_receipt.helper_pair_drop_point
        || checker.helper_return_node != mir_receipt.helper_return_node
        || checker.helper_default_pack != mir_receipt.helper_default_pack
        || checker.helper_default_pack_from != mir_receipt.helper_default_pack_from
        || checker.helper_default_pack_to != mir_receipt.helper_default_pack_to
        || checker.helper_default_pack_from != mir_receipt.type_argument
        || checker.miss_payload_type != mir_receipt.miss_payload_type_desc
        || checker.call_argument_types.get(1) != Some(&mir_receipt.key_type)
        || checker.call_argument_types.get(2) != Some(&mir_receipt.default_type)
    {
        return Err(
            "CheckedProgram helper/callsite/type/default-pack witness differs from its typed MIR receipt".into(),
        );
    }
    let function = program.functions().get(&mir_receipt.owner).ok_or_else(|| {
        "GetOrDefault<String> MIR receipt caller is absent from the program".to_string()
    })?;
    let has_literal = |value: &MirValueId, expected: &crate::core::ir::ResolvedLiteral| {
        function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Const { result, literal }
                        if result == value && literal == expected
                )
            })
    };
    let default_matches_checker = if let Some(local) = &checker.caller_default_local {
        let source =
            MirValueId::new(format!("local:{}", local.0 .0)).map_err(|error| error.to_string())?;
        let default_clones = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                MirInstructionKind::Clone { result, source } if result == &mir_receipt.default => {
                    Some(source)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        matches!(default_clones.as_slice(), [actual_source]
            if *actual_source == &source
                && function.values.get(actual_source).map(|value| &value.ty)
                    == Some(&mir_receipt.default_type)
                && function.values.get(&mir_receipt.default).map(|value| &value.ty)
                    == Some(&mir_receipt.default_type))
    } else {
        has_literal(
            &mir_receipt.default,
            &crate::core::ir::ResolvedLiteral::String(checker.caller_default_string.clone()),
        )
    };
    if !has_literal(
        &mir_receipt.key,
        &crate::core::ir::ResolvedLiteral::String(checker.caller_query_key_bytes.clone()),
    ) || !default_matches_checker
    {
        return Err(
            "CheckedProgram literal key/default differs from the exact typed MIR String operands"
                .into(),
        );
    }
    let initial_root = MirValueId::new(format!("local:{}", checker.caller_initial_root_local.0 .0))
        .map_err(|error| error.to_string())?;
    let set_root = MirValueId::new(format!("local:{}", checker.caller_set_root_local.0 .0))
        .map_err(|error| error.to_string())?;
    let mut set_matches = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
                result,
                source,
                key,
                value,
            } if result == &set_root && source == &initial_root => Some((key, value)),
            _ => None,
        });
    let Some((set_key, set_value)) = set_matches.next() else {
        return Err("CheckedProgram String Set is absent from the exact MIR root lineage".into());
    };
    let set_payload_matches_checker = if let Some(local) = &checker.caller_set_value_local {
        let source =
            MirValueId::new(format!("local:{}", local.0 .0)).map_err(|error| error.to_string())?;
        let payload_clones = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                MirInstructionKind::Clone { result, source } if result == set_value => Some(source),
                _ => None,
            })
            .collect::<Vec<_>>();
        matches!(payload_clones.as_slice(), [actual_source]
            if *actual_source == &source
                && function.values.get(actual_source).map(|value| &value.ty)
                    == Some(&mir_receipt.key_type)
                && function.values.get(set_value).map(|value| &value.ty)
                    == Some(&mir_receipt.key_type))
    } else {
        has_literal(
            set_value,
            &crate::core::ir::ResolvedLiteral::String(checker.caller_set_value_string.clone()),
        )
    };
    if set_matches.next().is_some()
        || !has_literal(
            set_key,
            &crate::core::ir::ResolvedLiteral::String(checker.caller_set_key_bytes.clone()),
        )
        || !set_payload_matches_checker
    {
        return Err(
            "CheckedProgram Set key/payload differs from the exact typed MIR String selection"
                .into(),
        );
    }
    let set_root_alias_clones = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Clone { result, source }
                    if result == &mir_receipt.root && source == &set_root
            )
        })
        .count();
    let expected_alias_clone_count = usize::from(mir_receipt.root != set_root);
    if set_root_alias_clones != expected_alias_clone_count {
        return Err(
            "CheckedProgram Set-result root and GetOrDefault receiver alias are not linked by the exact MIR Clone".into(),
        );
    }
    Ok(())
}

fn validate_ordinary_map_owned_any_v2_get_or_default_i32_checked_bridge(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    mir_receipt: &crate::core::mir::OrdinaryMapOwnedAnyV2GetOrDefaultI32MirReceipt,
) -> Result<(), String> {
    let receipts = checked
        .resource_analyses()
        .values()
        .filter_map(|analysis| {
            analysis
                .ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()?
                .get_or_default_i32_receipt
                .as_ref()
        })
        .collect::<Vec<_>>();
    let [checker] = receipts.as_slice() else {
        return Err(format!(
            "expected exactly one CheckedProgram GetOrDefault<i32> bridge, found {}",
            receipts.len()
        ));
    };
    if checker.profile_version.canonical_text()
        != "ordinary-map-owned-any-v2-get-or-default-i32-v1"
        || checker.caller_owner != mir_receipt.owner
        || checker.call_node != mir_receipt.callsite
        || checker.call_target != mir_receipt.call_target
        || checker.call_arguments.as_slice() != mir_receipt.argument_order.as_slice()
        || checker.call_arguments.first() != Some(&mir_receipt.caller_root_argument)
        || checker.call_argument_types.len() != 3
        // The Checker call carries the source-level Record nominal while the
        // MIR instruction deliberately uses the opaque V2 Map TypeDesc.
        // Bind the Checker side to its own source witness; MIR receipt
        // validation separately proves the opaque executable descriptor.
        || checker.call_argument_types[0] != checker.caller_root_type
        || &checker.caller_root_type != mir_receipt.checker_caller_root_type()
        || checker.call_type_argument != mir_receipt.type_argument
        || checker.call_result_type != mir_receipt.result_type
        || checker.caller_initial_root_local != mir_receipt.caller_initial_root_local
        || checker.caller_root_local != mir_receipt.caller_root_local
        || checker.caller_result_local != mir_receipt.caller_result_local
        || checker.caller_call_binding_point != mir_receipt.caller_call_binding_point
        || checker.caller_initial_root_drop_point != mir_receipt.caller_initial_root_drop_point
        || checker.caller_root_drop_point != mir_receipt.caller_root_drop_point
        || checker.helper_map_parameter != mir_receipt.helper_map_parameter
        || checker.helper_key_parameter != mir_receipt.helper_key_parameter
        || checker.helper_default_parameter != mir_receipt.helper_default_parameter
        || checker.helper_map_permission != crate::core::Permission::View
        || checker.call_argument_types.get(1) != Some(&mir_receipt.key_type)
        || checker.call_argument_types.get(2) != Some(&mir_receipt.default_type)
        || checker.helper_pair_binding_point != mir_receipt.helper_pair_binding_point
        || checker.helper_found_condition != mir_receipt.helper_found_condition
        || checker.helper_found_arm_block != mir_receipt.helper_found_arm_block
        || checker.helper_default_arm_block != mir_receipt.helper_default_arm_block
        || checker.helper_payload_projection != mir_receipt.helper_payload_projection
        || checker.helper_unpack_cast != mir_receipt.helper_unpack_cast
        || checker.helper_selected_binding_point != mir_receipt.helper_selected_binding_point
        || checker.helper_selected_local != mir_receipt.helper_selected_local
        || checker.helper_default_load != mir_receipt.helper_default_load
        || checker.helper_pair_drop_point != mir_receipt.helper_pair_drop_point
        || checker.helper_return_node != mir_receipt.helper_return_node
    {
        return Err(
            "CheckedProgram helper/callsite/type witness differs from its typed MIR receipt".into(),
        );
    }
    let function = program.functions().get(&mir_receipt.owner).ok_or_else(|| {
        "GetOrDefault<i32> MIR receipt caller is absent from the program".to_string()
    })?;
    let has_literal = |value: &MirValueId, expected: &crate::core::ir::ResolvedLiteral| {
        function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Const { result, literal }
                        if result == value && literal == expected
                )
            })
    };
    if !has_literal(
        &mir_receipt.key,
        &crate::core::ir::ResolvedLiteral::String(checker.caller_query_key_bytes.clone()),
    ) || !has_literal(
        &mir_receipt.default,
        &crate::core::ir::ResolvedLiteral::Int(checker.caller_default_i32),
    ) {
        return Err(
            "CheckedProgram literal key/default differs from the exact typed MIR call operands"
                .into(),
        );
    }
    let initial_root = MirValueId::new(format!("local:{}", checker.caller_initial_root_local.0 .0))
        .map_err(|error| error.to_string())?;
    let mut set_matches = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
                result,
                source,
                key,
                value,
            } if result == &mir_receipt.root && source == &initial_root => Some((key, value)),
            _ => None,
        });
    let Some((set_key, set_value)) = set_matches.next() else {
        return Err("CheckedProgram i32 Set is absent from the exact MIR root lineage".into());
    };
    if set_matches.next().is_some()
        || !has_literal(
            set_key,
            &crate::core::ir::ResolvedLiteral::String(checker.caller_set_key_bytes.clone()),
        )
        || !has_literal(
            set_value,
            &crate::core::ir::ResolvedLiteral::Int(checker.caller_set_value_i32),
        )
    {
        return Err(
            "CheckedProgram Set key/payload differs from the exact typed MIR i32 payload selection"
                .into(),
        );
    }
    Ok(())
}

/// Validate the exact Checker-sealed persistent-remove checked-miss route.
/// The independent verifier census closes the MIR operation set around one
/// New/Set/Remove/GetAny/unpack and all root/pair settlements before the
/// general capability gate admits execution.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    if route_receipt.profile
        != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE
    {
        return Err(vec![format!(
            "RemovedKeyCheckedMiss capability requires exact route profile '{}', found '{}'",
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE,
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "RemovedKeyCheckedMiss route receipt rejected: {message}"
            )]
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "RemovedKeyCheckedMiss Checker/MIR receipt rejected: {message}"
            )]
        })?;
    if &expected != route_receipt {
        return Err(vec![
            "RemovedKeyCheckedMiss route receipt differs from the Checker-bound route".into(),
        ]);
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_removed_key_checked_miss_mir_route(
        program,
    )
    .map_err(|message| {
        vec![format!(
            "RemovedKeyCheckedMiss MIR shape rejected: {message}"
        )]
    })?;
    validate_ordinary_map_owned_any_v2_removed_key_checked_miss_census(program).map_err(
        |message| {
            vec![format!(
                "RemovedKeyCheckedMiss capability census rejected: {message}"
            )]
        },
    )?;
    validate_mir_capabilities_with_options(program, false, false, false, false, false, false, true)
}

/// Validate the direct Values producer-to-caller transfer only when the
/// CheckedProgram and exact route receipt authorize both ends. The ordinary
/// capability API deliberately keeps rejecting this cross-function owner.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_producer_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|d| d.to_string())
                .collect::<Vec<String>>()
        })?;
    if route_receipt.profile
        != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_PRODUCER_CALLER_PROFILE
    {
        return Err(vec![format!(
            "Values producer/caller capability requires exact route profile '{}', found '{}'",
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_PRODUCER_CALLER_PROFILE,
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "Values producer/caller route receipt rejected: {message}"
            )]
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "Values producer/caller Checker/MIR route rejected: {message}"
            )]
        })?;
    if &expected != route_receipt {
        return Err(vec![
            "Values producer/caller receipt differs from the Checker-bound route".into(),
        ]);
    }
    let receipts =
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_mir_route(
            program,
        )
        .map_err(|message| {
            vec![format!(
                "Values producer/caller MIR census rejected: {message}"
            )]
        })?;
    if receipts.is_empty() || receipts.iter().any(|receipt| !receipt.unpacks.is_empty()) {
        return Err(vec![
            "Values producer/caller route must contain a receipted direct call and no cross-function unpack".into(),
        ]);
    }
    validate_mir_capabilities_with_options(program, false, false, false, false, true, false, false)
}

/// Validate cross-function Values ownership only when the exact Checker
/// route receipt authorizes its producer, caller, String unpack, and owner
/// settlements. The generic capability entry point remains closed.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "Values cross-call String caller capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "Values cross-call String caller route receipt rejected: {message}"
            )]
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "Values cross-call String caller Checker/MIR route rejected: {message}"
            )]
        })?;
    if &expected != route_receipt {
        return Err(vec![
            "Values cross-call String caller receipt differs from the Checker-bound route".into(),
        ]);
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_mir_route(
        program,
    )
    .map_err(|message| {
        vec![format!(
            "Values cross-call String caller MIR receipt preflight rejected: {message}"
        )]
    })?;
    let receipts = program
        .ordinary_map_owned_any_v2_values_crosscall_string_caller_receipts()
        .map_err(|message| {
            vec![format!(
                "Values cross-call String caller receipt inventory rejected: {message}"
            )]
        })?;
    if receipts.len() != 1 {
        return Err(vec![format!(
            "Values cross-call String caller requires one exact bridge receipt, found {}",
            receipts.len()
        )]);
    }
    for receipt in &receipts {
        super::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_type_owner_bridge(
            program, receipt,
        )
        .map_err(|message| {
            vec![format!("Values cross-call String type/owner bridge rejected: {message}")]
        })?;
    }
    validate_mir_capabilities_with_options_and_keys_snapshot(
        program, false, false, false, false, true, false, false, false, true, false, false, false,
        None,
    )
}

/// Validate the separately versioned Values cross-call String caller profile
/// whose producer returns two sorted String entries and whose caller selects
/// literal index one. The V1 capability above remains restricted to its
/// single-entry/index-zero profile.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "Values cross-call String caller index-one capability requires exact V2 route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "Values cross-call String caller index-one route receipt rejected: {message}"
            )]
        })?;
    let expected = crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
        checked, program,
    )
    .map_err(|message| {
        vec![format!(
            "Values cross-call String caller index-one Checker/MIR route rejected: {message}"
        )]
    })?;
    if &expected != route_receipt {
        return Err(vec![
            "Values cross-call String caller index-one route receipt differs from the Checker-bound V2 receipt".into(),
        ]);
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_mir_route(
        program,
    )
    .map_err(|message| {
        vec![format!(
            "Values cross-call String caller index-one MIR receipt preflight rejected: {message}"
        )]
    })?;

    let receipts = program
        .ordinary_map_owned_any_v2_values_crosscall_string_caller_receipts()
        .map_err(|message| {
            vec![format!(
                "Values cross-call String caller index-one receipt inventory rejected: {message}"
            )]
        })?;
    let [bridge] = receipts.as_slice() else {
        return Err(vec![format!(
            "Values cross-call String caller index-one requires one exact V2 aggregate receipt, found {}",
            receipts.len()
        )]);
    };
    let checker_v2 =
        crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion::V2;
    let expected_keys = ["a", "b"];
    let checker_keys = bridge
        .checker_caller
        .producer_sorted_keys
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let mir_keys = bridge
        .producer_set_entries
        .iter()
        .map(|entry| entry.key.as_str())
        .collect::<Vec<_>>();
    if bridge.schema
        != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_SCHEMA
        || bridge.profile != profile
        || bridge.checker_caller.profile_version != checker_v2
        || bridge.checker_caller.values_element_count != 2
        || bridge.checker_caller.index_value != 1
        || checker_keys != expected_keys
        || bridge.values_element_count != 2
        || bridge.caller_index_value != 1
        || mir_keys != expected_keys
        || bridge.producer_root_drop_instructions.len() != 3
        || bridge.producer_root_drop_place_indices != [0, 0, 0]
        || bridge.producer_set_entries.len() != 2
        || bridge.producer_payload_drop_instructions.len() != 2
        || bridge.producer_payload_drop_place_indices != [0, 0]
    {
        return Err(vec![
            "Values cross-call String caller index-one requires the exact sorted two-entry V2 Checker/MIR aggregate receipt and complete payload settlements".into(),
        ]);
    }
    super::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_type_owner_bridge(
        program, bridge,
    )
    .map_err(|message| {
        vec![format!(
            "Values cross-call String caller index-one type/owner bridge rejected: {message}"
        )]
    })?;

    // Only the exact V2 receipt above enables the existing Values ownership
    // checks. All other capability flags, including typed FFI proofs, stay
    // closed just as they do for the separately sealed V1 path.
    validate_mir_capabilities_with_options_and_keys_snapshot(
        program, false, false, false, false, true, false, false, false, true, false, false, false,
        None,
    )
}

/// Validate the additive Result-branch Values cross-call String caller
/// profiles. The route validator seals the exact V3/V4 Checker projection;
/// this gate independently binds its single MIR receipt and executable
/// TypeDesc bridge before enabling only the existing Values cross-call
/// capability. Straight-line V1/V2 receipts remain on their existing gates.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_profile(
        checked,
        program,
        route_receipt,
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_PROFILE,
        crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion::V3,
    )
}

fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_profile(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    profile: &str,
    expected_version: crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "Result-branch Values cross-call String caller capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "Result-branch Values cross-call route receipt rejected: {message}"
            )]
        })?;
    let expected = crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
        checked, program,
    )
    .map_err(|message| vec![format!("Result-branch Values cross-call Checker/MIR route rejected: {message}")])?;
    if &expected != route_receipt {
        return Err(vec![
            "Result-branch Values cross-call receipt differs from the exact Checker-bound route"
                .into(),
        ]);
    }
    if expected_version
        == crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion::V4
    {
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_mir_route(program)
    } else {
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_mir_route(program)
    }
    .map_err(|message| vec![format!("Result-branch Values cross-call MIR receipt preflight rejected: {message}")])?;

    let receipts = program
        .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_receipts()
        .map_err(|message| {
            vec![format!(
                "Result-branch Values cross-call receipt inventory rejected: {message}"
            )]
        })?;
    let [bridge] = receipts.as_slice() else {
        return Err(vec![format!(
            "Result-branch Values cross-call requires one exact producer/caller receipt, found {}",
            receipts.len()
        )]);
    };
    if bridge.profile != route_receipt.profile {
        return Err(vec![
            "Result-branch Values cross-call MIR receipt profile differs from its route receipt"
                .into(),
        ]);
    }
    if bridge.checker_caller.profile_version != expected_version {
        return Err(vec![format!(
            "Result-branch Values cross-call MIR receipt requires exact {:?} caller profile, found {:?}",
            expected_version, bridge.checker_caller.profile_version
        )]);
    }
    super::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_type_owner_bridge(
        program, bridge,
    )
    .map_err(|message| {
        vec![format!(
            "Result-branch Values cross-call type/owner bridge rejected: {message}"
        )]
    })?;
    super::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_verifier_receipts(
        program,
        route_receipt,
        profile,
    )
    .map_err(|message| {
        vec![format!(
            "Result-branch Values cross-call verifier receipt rejected: {message}"
        )]
    })?;
    let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
    let string_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let bool_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
    let producer = program.functions().get(&bridge.producer_owner);
    let caller = program.functions().get(&bridge.caller_owner);
    if bridge.values_type != values_ty
        || bridge.string_type != string_ty
        || bridge.bool_type != bool_ty
        || bridge.call_target != bridge.producer_owner
        || producer.is_none_or(|function| {
            function.result != values_ty
                || function
                    .values
                    .get(&bridge.producer_values_result)
                    .is_none_or(|v| v.ty != values_ty)
                || function
                    .values
                    .get(&bridge.producer_snapshot)
                    .is_none_or(|v| v.ty != values_ty)
        })
        || caller.is_none_or(|function| {
            function.result != bool_ty
                || function
                    .values
                    .get(&bridge.caller_call_result)
                    .is_none_or(|v| v.ty != values_ty)
                || function
                    .values
                    .get(&bridge.caller_snapshot)
                    .is_none_or(|v| v.ty != values_ty)
                || function
                    .values
                    .get(&bridge.caller_unpack_result)
                    .is_none_or(|v| v.ty != string_ty)
                || function
                    .values
                    .get(&bridge.caller_string)
                    .is_none_or(|v| v.ty != string_ty)
                || function
                    .values
                    .get(&bridge.caller_return_value)
                    .is_none_or(|v| v.ty != bool_ty)
        })
    {
        return Err(vec!["Result-branch Values cross-call TypeOwner bridge does not use the canonical Values/String/bool TypeDescs".into()]);
    }
    validate_mir_capabilities_with_options_and_keys_snapshot(
        program,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        Some(expected_version),
    )
}

/// Validate the separately versioned Result-branch Values String caller whose
/// caller selects literal index one. Its exact V4 receipt is independent of
/// the V3/index-zero admission above.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_INDEX1_PROFILE;
    validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_profile(
        checked,
        program,
        route_receipt,
        profile,
        crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion::V4,
    )
}

/// Validate the exact Checker-sealed Values local-index-i32 route. The narrow
/// Core receipt proves the immutable local's literal-zero initializer and its
/// sole use as the one indexed extraction; the ordinary whole-program gate
/// then validates the Values snapshot, result, and Map/snapshot settlements.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_local_index_i32_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    if route_receipt.profile
        != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE
    {
        return Err(vec![format!(
            "Values local-index-i32 capability requires exact route profile '{}', found '{}'",
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE,
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "Values local-index-i32 route receipt rejected: {message}"
            )]
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "Values local-index-i32 Checker/MIR route rejected: {message}"
            )]
        })?;
    if &expected != route_receipt {
        return Err(vec![
            "Values local-index-i32 receipt differs from the Checker-bound route".into(),
        ]);
    }
    let receipts = program
        .ordinary_map_owned_any_v2_values_local_index_i32_receipts()
        .map_err(|message| {
            vec![format!(
                "Values local-index-i32 MIR census rejected: {message}"
            )]
        })?;
    if receipts.len() != 1 {
        return Err(vec![
            "Values local-index-i32 route must contain exactly one immutable index and extraction"
                .into(),
        ]);
    }
    validate_mir_capabilities_with_options(program, false, false, false, false, false, false, false)
}

/// Validate the distinct fallible New/Set Values local-index profile. This
/// route requires the Result-branch source receipt and route profile, then
/// checks the exact seven-block/eight-edge MIR CFG before running the shared
/// whole-program capability scanner. It never borrows the straight-line
/// ValuesLocalIndexI32 route's authority.
pub(crate) fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;

    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_RESULT_BRANCH_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "ValuesLocalIndexI32 Result-branch capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "ValuesLocalIndexI32 Result-branch route receipt rejected: {message}"
            )]
        })?;

    let owner = crate::core::NodeId("function:main".into());
    let callable = checked.callable(&owner).ok_or_else(|| {
        vec!["ValuesLocalIndexI32 Result-branch has no checked function:main".into()]
    })?;
    let analysis = checked.resource_analysis(&owner).ok_or_else(|| {
        vec!["ValuesLocalIndexI32 Result-branch has no main ResourceAnalysis".into()]
    })?;
    let checker_cfg = analysis
        .ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .ok_or_else(|| {
            vec!["ValuesLocalIndexI32 Result-branch has no Checker Map/Any CFG receipt".into()]
        })?;
    let source_receipt = checker_cfg
        .values_local_index_i32_result_branch_receipt
        .as_ref()
        .ok_or_else(|| {
            vec![
                "ValuesLocalIndexI32 Result-branch has no dedicated three-path source receipt"
                    .into(),
            ]
        })?;
    crate::core::cfg::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_receipt(
        source_receipt,
        &callable.cfg,
        &callable.body,
        &callable.signature,
        checked.resolved_types(),
        checked.constants(),
        &checker_cfg.actions,
        &checker_cfg.attempts,
    )
    .map_err(|message| {
        vec![format!(
            "ValuesLocalIndexI32 Result-branch Checker source receipt rejected: {message}"
        )]
    })?;
    validate_values_local_index_i32_result_branch_mir_shape(program, source_receipt)
        .map_err(|message| vec![message])?;

    // This is intentionally the Result-branch validator. The straight-line
    // route cannot authorize this profile, even if its local-index witness is
    // otherwise identical.
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "ValuesLocalIndexI32 Result-branch Checker/MIR route rejected: {message}"
            )]
        })?;
    if &expected != route_receipt {
        return Err(vec![
            "ValuesLocalIndexI32 Result-branch route receipt differs from the exact CheckedProgram/MIR graph identity"
                .into(),
        ]);
    }

    validate_mir_capabilities_with_options_and_keys_snapshot_and_computed_string_key_owner_chain(
        program, false, false, false, false, false, false, false, false, false, false, false,
        false, None, false, true,
    )
}

fn validate_values_local_index_i32_result_branch_mir_shape(
    program: &MirProgram,
    source_receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchReceipt,
) -> Result<(), String> {
    use crate::core::mir::MirTerminator;
    use crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultPathKind as PathKind;

    let owner = crate::core::NodeId("function:main".into());
    let function = program.functions().get(&owner).ok_or_else(|| {
        "ValuesLocalIndexI32 Result-branch MIR is missing function:main".to_string()
    })?;
    if program.functions().len() != 1 {
        return Err(
            "ValuesLocalIndexI32 Result-branch MIR must contain only the exact main callable"
                .into(),
        );
    }
    // The source receipt embeds the complete CheckedProgram CFG, including
    // any exact direct-Ensures short-circuit prefix. The source replay above
    // validates that whole CFG and the New/Set business projection; the
    // seven-block limit below applies to MIR only.
    if source_receipt.owner != owner
        || source_receipt.cfg.owner != owner
        || source_receipt.paths.len() != 3
        || [PathKind::MapNewErr, PathKind::MapSetErr, PathKind::Success]
            .into_iter()
            .any(|kind| {
                source_receipt
                    .paths
                    .iter()
                    .filter(|path| path.kind == kind)
                    .count()
                    != 1
            })
    {
        return Err(
            format!(
                "ValuesLocalIndexI32 Result-branch Checker receipt does not contain exactly one of each New-Err, Set-Err, and Success path (owner_ok={}, cfg_owner_ok={}, full_cfg_blocks={}, full_cfg_edges={}, paths={}, path_kinds={:?})",
                source_receipt.owner == owner,
                source_receipt.cfg.owner == owner,
                source_receipt.cfg.reachable_blocks.len(),
                source_receipt.cfg.reachable_edges.len(),
                source_receipt.paths.len(),
                source_receipt
                    .paths
                    .iter()
                    .map(|path| path.kind)
                    .collect::<Vec<_>>(),
            ),
        );
    }
    if function.blocks.len() != 7 {
        return Err(format!(
            "ValuesLocalIndexI32 Result-branch MIR must contain exactly seven blocks, found {}",
            function.blocks.len()
        ));
    }
    function.validate().map_err(|errors| {
        format!("ValuesLocalIndexI32 Result-branch MIR structure rejected: {errors:?}")
    })?;

    let mut edge_ids = BTreeSet::new();
    let mut pending = vec![function.entry.clone()];
    let mut reachable = BTreeSet::new();
    let mut switch_move_blocks = 0usize;
    while let Some(block_id) = pending.pop() {
        if !reachable.insert(block_id.clone()) {
            continue;
        }
        let block = function.blocks.get(&block_id).ok_or_else(|| {
            format!(
                "ValuesLocalIndexI32 Result-branch MIR edge targets absent block '{}'",
                block_id.as_str()
            )
        })?;
        match &block.terminator {
            MirTerminator::Goto { edge, target, .. } => {
                if !edge_ids.insert(edge.clone()) {
                    return Err(
                        "ValuesLocalIndexI32 Result-branch MIR duplicates an edge identity".into(),
                    );
                }
                pending.push(target.clone());
            }
            MirTerminator::Branch {
                then_edge,
                then_target,
                else_edge,
                else_target,
                ..
            } => {
                if !edge_ids.insert(then_edge.clone()) || !edge_ids.insert(else_edge.clone()) {
                    return Err(
                        "ValuesLocalIndexI32 Result-branch MIR duplicates an edge identity".into(),
                    );
                }
                pending.push(then_target.clone());
                pending.push(else_target.clone());
            }
            MirTerminator::Switch { arms, .. } => {
                for arm in arms {
                    if !edge_ids.insert(arm.edge.clone()) {
                        return Err(
                            "ValuesLocalIndexI32 Result-branch MIR duplicates an edge identity"
                                .into(),
                        );
                    }
                    pending.push(arm.target.clone());
                }
            }
            MirTerminator::SwitchMove { arms, .. } => {
                switch_move_blocks += 1;
                if arms.len() != 2 {
                    return Err(
                        "ValuesLocalIndexI32 Result-branch MIR requires two exhaustive Result arms per consuming dispatch"
                            .into(),
                    );
                }
                for arm in arms {
                    if !edge_ids.insert(arm.edge.clone()) {
                        return Err(
                            "ValuesLocalIndexI32 Result-branch MIR duplicates an edge identity"
                                .into(),
                        );
                    }
                    pending.push(arm.target.clone());
                }
            }
            MirTerminator::Return { .. }
            | MirTerminator::Trap { .. }
            | MirTerminator::Fault { .. }
            | MirTerminator::Unreachable => {}
        }
    }
    if reachable.len() != 7
        || reachable.len() != function.blocks.len()
        || edge_ids.len() != 8
        || switch_move_blocks != 2
    {
        return Err(format!(
            "ValuesLocalIndexI32 Result-branch MIR must be one reachable 7-block/8-edge graph with two binary consuming Result dispatches (found {}/{}, {} edges, {switch_move_blocks} dispatches)",
            reachable.len(),
            function.blocks.len(),
            edge_ids.len()
        ));
    }
    Ok(())
}

/// Validate the exact Checker-sealed heterogeneous Values index-one String
/// profile. Core replays the selected sorted Set and owner receipt first;
/// this gate then reuses the closed whole-program Values capability checks.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    if route_receipt.profile
        != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE
    {
        return Err(vec![format!(
            "Values heterogeneous String index-one capability requires exact route profile '{}', found '{}'",
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE,
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "Values heterogeneous String index-one route receipt rejected: {message}"
            )]
        })?;
    let expected = crate::core::mir::validate_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
        checked,
        program,
    )
    .map_err(|message| {
        vec![format!(
            "Values heterogeneous String index-one Checker/MIR route rejected: {message}"
        )]
    })?;
    if &expected != route_receipt {
        return Err(vec![
            "Values heterogeneous String index-one receipt differs from the Checker-bound route"
                .into(),
        ]);
    }
    let receipts = program
        .ordinary_map_owned_any_v2_values_unpack_result_receipts()
        .map_err(|message| {
            vec![format!(
                "Values heterogeneous String index-one MIR unpack census rejected: {message}"
            )]
        })?;
    if receipts.len() != 1
        || receipts[0].conversion_to
            != crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String)
        || receipts[0].result_owner_settlement
            != crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::ExplicitStringDrop
    {
        return Err(vec![
            "Values heterogeneous String index-one route must contain exactly one owned String unpack".into(),
        ]);
    }
    validate_mir_capabilities_with_options(program, false, false, false, false, false, false, false)
}

/// Validate the exact Checker-sealed Keys snapshot route. Generic capability
/// validation stays closed to Keys because its result owns one independent
/// String per sorted key; only this source-bound receipt admits that shape.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_keys_snapshot_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    if route_receipt.profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE {
        return Err(vec![format!(
            "KeysSnapshot capability requires exact route profile '{}', found '{}'",
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE,
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| vec![format!("KeysSnapshot route receipt rejected: {message}")])?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_route(checked, program)
            .map_err(|message| {
                vec![format!(
                    "KeysSnapshot Checker/MIR route rejected: {message}"
                )]
            })?;
    if &expected != route_receipt {
        return Err(vec![
            "KeysSnapshot route receipt differs from the Checker-bound route".into(),
        ]);
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_mir_route(program)
        .map_err(|message| vec![format!("KeysSnapshot MIR census rejected: {message}")])?;
    validate_mir_capabilities_with_options_and_keys_snapshot(
        program, false, false, false, false, false, false, true, false, false, false, false, false,
        None,
    )
}

/// Validate the exact Checker-sealed DynamicKeysGet profile. Generic
/// capability validation stays closed to argv projection and dynamic Keys;
/// this entry joins the source route, immutable MIR receipt, and dedicated
/// symbolic operation census before enabling the narrow gate.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_dynamic_keys_get_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    let profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "DynamicKeysGet capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| vec![format!("DynamicKeysGet route receipt rejected: {message}")])?;
    let expected = crate::core::mir::validate_ordinary_map_owned_any_v2_dynamic_keys_get_route(
        checked, program,
    )
    .map_err(|message| {
        vec![format!(
            "DynamicKeysGet Checker/MIR route rejected: {message}"
        )]
    })?;
    if &expected != route_receipt {
        return Err(vec![
            "DynamicKeysGet route receipt differs from the Checker-bound route".into(),
        ]);
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_dynamic_keys_get_mir_route(program)
        .map_err(|message| vec![format!("DynamicKeysGet MIR census rejected: {message}")])?;
    validate_mir_capabilities_with_options_and_keys_snapshot(
        program, false, false, false, false, false, false, false, false, false, false, false, true,
        None,
    )
}

/// Validate the exact computed String-key owner-chain capability only after
/// joining the CheckedProgram's typed source receipt, immutable MIR route
/// receipt, and complete operation/ownership census. Generic MIR capability
/// validation remains closed to this source-qualified profile.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    let profile =
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE;
    if route_receipt.profile != profile {
        return Err(vec![format!(
            "ComputedStringKeyOwnerChain capability requires exact route profile '{profile}', found '{}'",
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "ComputedStringKeyOwnerChain route receipt rejected: {message}"
            )]
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "ComputedStringKeyOwnerChain Checker/MIR route rejected: {message}"
            )]
        })?;
    if &expected != route_receipt {
        return Err(vec![
            "ComputedStringKeyOwnerChain route receipt differs from the Checker-bound route".into(),
        ]);
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_mir_route(
        program,
    )
    .map_err(|message| {
        vec![format!(
            "ComputedStringKeyOwnerChain MIR census rejected: {message}"
        )]
    })?;
    validate_mir_capabilities_with_options_and_keys_snapshot_and_computed_string_key_owner_chain(
        program, false, false, false, false, false, false, false, false, false, false, false,
        false, None, true, false,
    )
}

/// Validate the exact Checker-sealed Map-root producer return and its unique
/// scalar-observing caller. The ordinary capability API keeps rejecting
/// cross-function Map-root returns because the opaque root can cross the call
/// boundary only under this whole-program receipt.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    if route_receipt.profile
        != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_PRODUCER_CALLER_PROFILE
    {
        return Err(vec![format!(
            "Map-root producer/caller capability requires exact route profile '{}', found '{}'",
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_PRODUCER_CALLER_PROFILE,
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "Map-root producer/caller route receipt rejected: {message}"
            )]
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "Map-root producer/caller Checker/MIR route rejected: {message}"
            )]
        })?;
    if &expected != route_receipt {
        return Err(vec![
            "Map-root producer/caller route receipt differs from the Checker-bound route".into(),
        ]);
    }
    let receipts =
        crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_mir_route(
            program,
        )
        .map_err(|message| {
            vec![format!(
                "Map-root producer/caller MIR census rejected: {message}"
            )]
        })?;
    if receipts.len() != 1 {
        return Err(vec![
            "Map-root producer/caller route must contain one exact producer/caller bridge".into(),
        ]);
    }
    validate_mir_capabilities_with_options(program, false, false, false, false, false, true, false)
}

/// Validate the exact Checker-sealed Map-root → GetAny → checked String
/// unpack transfer. The generic capability API keeps rejecting the returned
/// root crossing this direct call boundary; this route admits only the
/// uniquely derived producer/caller receipt and the exact three settlements.
pub fn validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
    route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), Vec<String>> {
    checked
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
        })?;
    if route_receipt.profile
        != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_GET_ANY_CALLER_PROFILE
    {
        return Err(vec![format!(
            "Map-root GetAny-caller capability requires exact route profile '{}', found '{}'",
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_GET_ANY_CALLER_PROFILE,
            route_receipt.profile
        )]);
    }
    route_receipt
        .validate_against_program(program)
        .map_err(|message| {
            vec![format!(
                "Map-root GetAny-caller route receipt rejected: {message}"
            )]
        })?;
    let expected =
        crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
            checked, program,
        )
        .map_err(|message| {
            vec![format!(
                "Map-root GetAny-caller Checker/MIR route rejected: {message}"
            )]
        })?;
    if &expected != route_receipt {
        return Err(vec![
            "Map-root GetAny-caller route receipt differs from the Checker-bound route".into(),
        ]);
    }
    let receipts =
        crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_mir_route(
            program,
        )
        .map_err(|message| {
            vec![format!(
                "Map-root GetAny-caller MIR census rejected: {message}"
            )]
        })?;
    if receipts.len() != 1 {
        return Err(vec![
            "Map-root GetAny-caller route must contain one exact producer/caller bridge".into(),
        ]);
    }
    validate_mir_capabilities_with_options(program, false, false, false, false, false, true, false)
}

/// Validate canonical MIR for an FFI proof request. This retains the complete
/// verifier capability gate but permits the verifier-only typed StringHandle
/// and scalar-leaf `repr(C)` record receipts understood by the MIR proof
/// engine. It is deliberately separate from `validate_mir_capabilities`,
/// which stays scalar-only for the ordinary verifier/execution boundary.
pub(crate) fn validate_ffi_verifier_mir_capabilities(
    program: &MirProgram,
) -> Result<(), Vec<String>> {
    validate_mir_capabilities_with_verifier_ffi_proofs(program, true)
}

fn validate_mir_capabilities_with_verifier_ffi_proofs(
    program: &MirProgram,
    allow_typed_ffi_verifier_proofs: bool,
) -> Result<(), Vec<String>> {
    validate_mir_capabilities_with_options(
        program,
        allow_typed_ffi_verifier_proofs,
        false,
        false,
        false,
        false,
        false,
        false,
    )
}

fn validate_mir_capabilities_with_options(
    program: &MirProgram,
    allow_typed_ffi_verifier_proofs: bool,
    allow_direct_print: bool,
    allow_ordinary_map_owned_any_v2_is_empty: bool,
    allow_ordinary_map_owned_any_v2_zero_miss_i32: bool,
    allow_ordinary_map_owned_any_v2_values_producer_caller: bool,
    allow_ordinary_map_owned_any_v2_map_root_producer_caller: bool,
    allow_ordinary_map_owned_any_v2_removed_key_checked_miss: bool,
) -> Result<(), Vec<String>> {
    validate_mir_capabilities_with_options_and_keys_snapshot(
        program,
        allow_typed_ffi_verifier_proofs,
        allow_direct_print,
        allow_ordinary_map_owned_any_v2_is_empty,
        allow_ordinary_map_owned_any_v2_zero_miss_i32,
        allow_ordinary_map_owned_any_v2_values_producer_caller,
        allow_ordinary_map_owned_any_v2_map_root_producer_caller,
        false,
        allow_ordinary_map_owned_any_v2_removed_key_checked_miss,
        false,
        false,
        false,
        false,
        None,
    )
}

fn validate_mir_capabilities_with_options_and_keys_snapshot(
    program: &MirProgram,
    allow_typed_ffi_verifier_proofs: bool,
    allow_direct_print: bool,
    allow_ordinary_map_owned_any_v2_is_empty: bool,
    allow_ordinary_map_owned_any_v2_zero_miss_i32: bool,
    allow_ordinary_map_owned_any_v2_values_producer_caller: bool,
    allow_ordinary_map_owned_any_v2_map_root_producer_caller: bool,
    allow_ordinary_map_owned_any_v2_keys_snapshot: bool,
    allow_ordinary_map_owned_any_v2_removed_key_checked_miss: bool,
    allow_values_crosscall_string_caller: bool,
    allow_ordinary_map_owned_any_v2_get_or_default_i32: bool,
    allow_ordinary_map_owned_any_v2_get_or_default_string: bool,
    allow_ordinary_map_owned_any_v2_dynamic_keys_get: bool,
    values_result_crosscall_profile: Option<
        crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
    >,
) -> Result<(), Vec<String>> {
    validate_mir_capabilities_with_options_and_keys_snapshot_and_computed_string_key_owner_chain(
        program,
        allow_typed_ffi_verifier_proofs,
        allow_direct_print,
        allow_ordinary_map_owned_any_v2_is_empty,
        allow_ordinary_map_owned_any_v2_zero_miss_i32,
        allow_ordinary_map_owned_any_v2_values_producer_caller,
        allow_ordinary_map_owned_any_v2_map_root_producer_caller,
        allow_ordinary_map_owned_any_v2_keys_snapshot,
        allow_ordinary_map_owned_any_v2_removed_key_checked_miss,
        allow_values_crosscall_string_caller,
        allow_ordinary_map_owned_any_v2_get_or_default_i32,
        allow_ordinary_map_owned_any_v2_get_or_default_string,
        allow_ordinary_map_owned_any_v2_dynamic_keys_get,
        values_result_crosscall_profile,
        false,
        false,
    )
}

fn validate_mir_capabilities_with_options_and_keys_snapshot_and_computed_string_key_owner_chain(
    program: &MirProgram,
    allow_typed_ffi_verifier_proofs: bool,
    allow_direct_print: bool,
    allow_ordinary_map_owned_any_v2_is_empty: bool,
    allow_ordinary_map_owned_any_v2_zero_miss_i32: bool,
    allow_ordinary_map_owned_any_v2_values_producer_caller: bool,
    allow_ordinary_map_owned_any_v2_map_root_producer_caller: bool,
    allow_ordinary_map_owned_any_v2_keys_snapshot: bool,
    allow_ordinary_map_owned_any_v2_removed_key_checked_miss: bool,
    allow_values_crosscall_string_caller: bool,
    allow_ordinary_map_owned_any_v2_get_or_default_i32: bool,
    allow_ordinary_map_owned_any_v2_get_or_default_string: bool,
    allow_ordinary_map_owned_any_v2_dynamic_keys_get: bool,
    values_result_crosscall_profile: Option<
        crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
    >,
    allow_computed_string_key_owner_chain: bool,
    allow_values_local_index_i32_result_branch: bool,
) -> Result<(), Vec<String>> {
    let set_any_result_count = program
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2SetAnyResult { .. }
            )
        })
        .count();
    if set_any_result_count != 0
        && !allow_computed_string_key_owner_chain
        && !allow_values_local_index_i32_result_branch
    {
        return Err(vec![format!(
            "{}: ordinary Map/Any SetAnyResult requires its exact CheckedProgram-bound ComputedStringKeyOwnerChain L2 capability",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )]);
    }
    if (allow_computed_string_key_owner_chain || allow_values_local_index_i32_result_branch)
        && set_any_result_count != 1
    {
        return Err(vec![
            "ComputedStringKeyOwnerChain capability requires exactly one SetAnyResult operation"
                .into(),
        ]);
    }
    if mir_program_has_ordinary_map_set_get_any_result_i32_signal(program) {
        return Err(vec![format!(
            "{}: ordinary Map GetAnyResult requires its dedicated CheckedProgram-bound L2 capability",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )]);
    }
    if super::mir::mir_program_has_ordinary_map_set_get_any_i32_signal(program) {
        return Err(vec![format!(
            "{}: ordinary Map Set/GetAny-i32 requires its dedicated Checker-bound L2 capability",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )]);
    }
    // The source census and canonical function shape are mandatory inputs to
    // capability decisions. Run these before the Actor fast path, DFS cycle
    // cuts, solver facts, or any per-function capability effects.
    program.validate_proof_source_census().map_err(|error| {
        vec![format!(
            "canonical MIR proof source census rejected: {error:?}"
        )]
    })?;
    let mut structural_errors = Vec::new();
    for (owner, function) in program.functions() {
        if let Err(errors) = function.validate() {
            structural_errors.extend(errors.into_iter().map(|error| {
                format!(
                    "canonical MIR structural preflight rejected for '{}': {error}",
                    owner.0
                )
            }));
        }
    }
    if !structural_errors.is_empty() {
        let mut errors =
            validate_ffi_capability_preflight(program, allow_typed_ffi_verifier_proofs);
        if !errors.is_empty() {
            errors.extend(structural_errors);
            return Err(errors);
        }
        return Err(structural_errors);
    }
    let has_unmodeled_set_get_i32_result = program.functions().values().any(|function| {
        function
            .ownership
            .ordinary_map_set_get_i32_lineage_receipt()
            .is_some()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        &instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { .. }
                    )
                })
            })
    });
    if has_unmodeled_set_get_i32_result {
        return Err(vec![
            "ordinary Map Set/Get<i32> Result status/tag semantics are not modeled by the MIR verifier; exact CheckedProgram-aware L2 semantics are required".into(),
        ]);
    }
    let has_get_or_default_i32 = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. }
                )
            })
        })
    });
    if has_get_or_default_i32 && !allow_ordinary_map_owned_any_v2_get_or_default_i32 {
        return Err(vec![
            "ordinary Map/Any owner-v2 GetOrDefault<i32> requires exact CheckedProgram/MIR route validation".into(),
        ]);
    }
    if allow_ordinary_map_owned_any_v2_get_or_default_i32 {
        super::mir::validate_ordinary_map_owned_any_v2_get_or_default_i32_verifier_receipt(program)
            .map_err(|message| {
                vec![format!(
                    "GetOrDefault<i32> MIR/TypeDesc census rejected: {message}"
                )]
            })?;
    }
    let has_get_or_default_string = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultString { .. }
                )
            })
        })
    });
    if has_get_or_default_string && !allow_ordinary_map_owned_any_v2_get_or_default_string {
        return Err(vec![
            "ordinary Map/Any owner-v2 GetOrDefault<String> requires exact CheckedProgram/MIR route validation".into(),
        ]);
    }
    if allow_ordinary_map_owned_any_v2_get_or_default_string {
        super::mir::validate_ordinary_map_owned_any_v2_get_or_default_string_verifier_receipt(
            program,
        )
        .map_err(|message| {
            vec![format!(
                "GetOrDefault<String> MIR/TypeDesc census rejected: {message}"
            )]
        })?;
    }
    let has_owner_v2_is_empty = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { .. }
                )
            })
        })
    });
    if has_owner_v2_is_empty && !allow_ordinary_map_owned_any_v2_is_empty {
        return Err(vec![
            "ordinary Map/Any owner-v2 IsEmpty requires exact Checker/MIR route validation".into(),
        ]);
    }
    let has_keys_snapshot = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { .. }
                        | MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack { .. }
                )
            })
        })
    });
    let has_dynamic_keys_get = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2DynamicArg0 { .. }
                )
            })
        })
    });
    if has_dynamic_keys_get && !allow_ordinary_map_owned_any_v2_dynamic_keys_get {
        return Err(vec![
            "ordinary Map/Any owner-v2 DynamicKeysGet argv[0] requires exact CheckedProgram/MIR route validation".into(),
        ]);
    }
    if allow_ordinary_map_owned_any_v2_dynamic_keys_get && !has_dynamic_keys_get {
        return Err(vec![
            "DynamicKeysGet capability was requested for MIR without its argv[0] operation".into(),
        ]);
    }
    if has_keys_snapshot
        && !allow_ordinary_map_owned_any_v2_keys_snapshot
        && !allow_ordinary_map_owned_any_v2_dynamic_keys_get
    {
        return Err(vec![
            "ordinary Map/Any owner-v2 KeysSnapshot requires exact Checker/MIR route validation"
                .into(),
        ]);
    }
    if allow_ordinary_map_owned_any_v2_keys_snapshot {
        crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_mir_route(program)
            .map_err(|message| {
                vec![format!(
                    "KeysSnapshot MIR receipt preflight rejected: {message}"
                )]
            })?;
    }
    if allow_ordinary_map_owned_any_v2_dynamic_keys_get {
        crate::core::mir::validate_ordinary_map_owned_any_v2_dynamic_keys_get_mir_route(program)
            .map_err(|message| {
                vec![format!(
                    "DynamicKeysGet MIR receipt preflight rejected: {message}"
                )]
            })?;
    }
    let matches_exact_zero_miss_i32_route =
        crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_mir_route(program)
            .is_ok();
    if matches_exact_zero_miss_i32_route && !allow_ordinary_map_owned_any_v2_zero_miss_i32 {
        return Err(vec![
            "ordinary Map/Any owner-v2 canonical i32 miss unpack requires exact Checker/MIR route validation".into(),
        ]);
    }
    let matches_exact_removed_key_checked_miss_route =
        crate::core::mir::validate_ordinary_map_owned_any_v2_removed_key_checked_miss_mir_route(
            program,
        )
        .is_ok();
    if matches_exact_removed_key_checked_miss_route
        && !allow_ordinary_map_owned_any_v2_removed_key_checked_miss
    {
        return Err(vec![
            "ordinary Map/Any owner-v2 RemovedKeyCheckedMiss requires exact Checker/MIR route validation".into(),
        ]);
    }
    let has_values_producer_caller_signal = program.functions().values().any(|function| {
        function.result == crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    let MirInstructionKind::Call { result, callee, .. } = &instruction.kind else {
                        return false;
                    };
                    let values_ty =
                        crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
                    result.as_ref().is_some_and(|result| {
                        function
                            .values
                            .get(result)
                            .is_some_and(|value| value.ty == values_ty)
                    }) || crate::core::mir::canonical_protocol_call_target(callee)
                        .and_then(|target| program.functions().get(&target))
                        .is_some_and(|target| target.result == values_ty)
                })
            })
    });
    if has_values_producer_caller_signal
        && !allow_ordinary_map_owned_any_v2_values_producer_caller
        && values_result_crosscall_profile.is_none()
    {
        return Err(vec![
            "ordinary Map/Any owner-v2 Values producer/caller requires exact Checker/MIR route validation".into(),
        ]);
    }
    let has_map_root_producer_caller_signal = map_root_producer_caller_signal(program);
    if has_map_root_producer_caller_signal
        && !allow_ordinary_map_owned_any_v2_map_root_producer_caller
    {
        return Err(vec![
            "Map-root producer/caller requires exact Checker/MIR route validation".into(),
        ]);
    }
    if allow_ordinary_map_owned_any_v2_map_root_producer_caller {
        let producer_caller =
            crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_mir_route(
                program,
            );
        let get_any_caller =
            crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_mir_route(
                program,
            );
        if let (Err(producer_err), Err(get_any_err)) = (producer_caller, get_any_caller) {
            return Err(vec![format!(
                "Map-root producer/caller MIR receipt preflight rejected: {}; GetAny-caller receipt preflight rejected: {}",
                producer_err,
                get_any_err
            )]);
        }
    }
    for (owner, function) in program.functions() {
        let Some(worklist) = function.proof_worklist.as_ref() else {
            continue;
        };
        if worklist_has_mutable_loop(worklist) || worklist_has_ffi_call_path_loop(worklist) {
            // A mutable-loop certificate authorizes CFG cycles. Recompute its
            // owner coverage, typed receipt, and exact MIR edge correspondence
            // from the independent source census before the DFS can cut them.
            // The exact immutable FFI call-path profile is also validated
            // here, even when a source loop is otherwise classified
            // stationary: its call block and every permitted backedge must be
            // bound to the same sealed source/MIR receipt.
            program
                .validate_owner_proof_worklist(owner)
                .map_err(|error| {
                    vec![format!("canonical MIR loop preflight rejected: {error:?}")]
                })?;
        }
    }
    // ClosureBind is metadata only; ClosureInvoke is admitted only by the
    // checked source seal and direct-call receipt.
    program.validate_closure_callable_receipts()?;
    // Nested direct-call lowering may carry declaration-time snapshots as
    // hidden parameters. Verify that owner/capture/call relation before the
    // verifier admits the graph, including graphs that do not otherwise need
    // symbolic execution.
    program.validate_nested_callable_scope_receipts()?;
    program.validate_actor_contracts()?;
    program.validate_managed_any_trap_cleanup_instances()?;
    validate_ordinary_map_owned_any_v2_payload_selection(program).map_err(|message| {
        vec![format!(
            "canonical MIR ordinary Map/Any owner-v2 payload selection preflight rejected: {message}"
        )]
    })?;
    validate_ordinary_map_owned_any_v2_values_receipts_with_producer_caller(
        program,
        allow_ordinary_map_owned_any_v2_values_producer_caller,
        allow_values_crosscall_string_caller,
        values_result_crosscall_profile,
    )
    .map_err(|message| {
        vec![format!(
            "canonical MIR ordinary Map/Any owner-v2 Values receipt preflight rejected: {message}"
        )]
    })?;
    program
        .ordinary_map_owned_any_v2_values_unpack_result_receipts()
        .map_err(|message| {
            vec![format!(
                "canonical MIR ordinary Map/Any owner-v2 Values unpack receipt preflight rejected: {message}"
            )]
        })?;
    if program
        .functions()
        .values()
        .any(|f| f.ownership.has_map_empty_root_root_queries_v1_receipt())
    {
        crate::core::mir::validate_map_empty_root_root_queries_v1_mir_route(program).map_err(
            |e| {
                vec![format!(
                    "MIR-COVERAGE-001: MapEmptyRootRootQueriesV1 capability: {e}"
                )]
            },
        )?;
    }
    let has_ordinary_map_owner_observer_v1_signal = program.functions().values().any(|function| {
        function
            .ownership
            .has_checker_ordinary_map_owner_observer_v1_receipts()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        &instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson { .. }
                            | MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln { .. }
                    )
                })
            })
    });
    if has_ordinary_map_owner_observer_v1_signal {
        crate::core::mir::validate_ordinary_map_owner_lifecycle_observer_v1_mir_route(program)
            .map_err(|message| {
                vec![format!(
                    "canonical MIR ordinary Map lifecycle observer-v1 receipt preflight rejected: {message}"
                )]
            })?;
    }
    validate_ordinary_map_owned_any_v2_get_any_receipts(program).map_err(|message| {
        vec![format!(
            "canonical MIR ordinary Map/Any owner-v2 GetAny receipt preflight rejected: {message}"
        )]
    })?;
    validate_ordinary_map_owned_any_v2_unpack_receipts(program).map_err(|message| {
        vec![format!(
            "canonical MIR ordinary Map/Any owner-v2 unpack receipt preflight rejected: {message}"
        )]
    })?;
    if program.actor_execution_plan().is_some() {
        // The finite Actor receipt validates executable structure and lifetime
        // boundaries. It does not introduce a symbolic mailbox effect model.
        if program
            .functions()
            .values()
            .any(|function| !function.contracts.is_empty())
        {
            return Err(vec![
                "canonical Actor mailbox effects have no trusted contract proof model".into(),
            ]);
        }
        // The Actor fast path does not enter the ordinary per-function gate,
        // so run the recursive Any boundary preflight before returning its
        // neutral capability result. Keep non-Actor diagnostic aggregation
        // in CapabilityGate below unchanged.
        program.validate_managed_any_identity_instances()?;
        return Ok(());
    }
    let mut gate = CapabilityGate::new(
        program,
        allow_typed_ffi_verifier_proofs,
        allow_direct_print,
        allow_ordinary_map_owned_any_v2_is_empty,
        allow_ordinary_map_owned_any_v2_values_producer_caller,
        allow_ordinary_map_owned_any_v2_map_root_producer_caller,
        allow_ordinary_map_owned_any_v2_keys_snapshot,
        allow_values_crosscall_string_caller,
        allow_ordinary_map_owned_any_v2_get_or_default_i32,
        allow_ordinary_map_owned_any_v2_get_or_default_string,
        allow_ordinary_map_owned_any_v2_dynamic_keys_get,
        values_result_crosscall_profile,
        allow_computed_string_key_owner_chain,
        allow_values_local_index_i32_result_branch,
    );
    if let Err(errors) = program.validate_managed_any_identity_instances() {
        for message in errors {
            gate.error(message);
        }
    }
    for message in
        crate::core::mir::validate_ffi_receipt_table(program.functions(), program.ffi_calls())
    {
        gate.error(message);
    }
    for message in crate::core::mir::validate_ffi_symbol_declaration_shapes(program.ffi_calls()) {
        gate.error(message);
    }
    gate.validate();
    if gate.errors.is_empty() {
        Ok(())
    } else {
        Err(gate.errors.into_iter().collect())
    }
}

/// Detect only aggregate MapRoot returns crossing a direct call boundary.
/// Local MapRoot lifecycle routes retain their existing capability and do not
/// acquire this producer/caller exception merely because they use the same
/// TypeDesc.
fn map_root_producer_caller_signal(program: &MirProgram) -> bool {
    let root_ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
    program.functions().values().any(|function| {
        function.result == root_ty
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    let MirInstructionKind::Call { result, callee, .. } = &instruction.kind else {
                        return false;
                    };
                    result.as_ref().is_some_and(|result| {
                        function
                            .values
                            .get(result)
                            .is_some_and(|value| value.ty == root_ty)
                    }) || crate::core::mir::canonical_protocol_call_target(callee)
                        .and_then(|target| program.functions().get(&target))
                        .is_some_and(|target| target.result == root_ty)
                })
            })
    })
}

fn map_root_producer_caller_bridge_for_caller(
    program: &MirProgram,
    function: &MirFunction,
) -> Option<crate::core::mir::OrdinaryMapOwnedAnyV2MapRootProducerCallerMirReceipt> {
    let receipts = program
        .ordinary_map_owned_any_v2_map_root_producer_caller_receipts()
        .ok()?;
    let matching = receipts
        .iter()
        .filter(|receipt| receipt.caller_owner == function.owner)
        .collect::<Vec<_>>();
    let [receipt] = matching.as_slice() else {
        return None;
    };
    receipt.validate_against_program(program).ok()?;
    Some((*receipt).clone())
}

fn ordinary_map_owned_any_v2_map_root_producer_caller_bridge_for_producer(
    program: &MirProgram,
    function: &MirFunction,
) -> Option<crate::core::mir::OrdinaryMapOwnedAnyV2MapRootProducerCallerMirReceipt> {
    let receipts = program
        .ordinary_map_owned_any_v2_map_root_producer_caller_receipts()
        .ok()?;
    let matching = receipts
        .iter()
        .filter(|receipt| receipt.producer_owner == function.owner)
        .collect::<Vec<_>>();
    let [receipt] = matching.as_slice() else {
        return None;
    };
    receipt.validate_against_program(program).ok()?;
    Some((*receipt).clone())
}

fn map_root_get_any_caller_bridge_for_caller(
    program: &MirProgram,
    function: &MirFunction,
) -> Option<crate::core::mir::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerMirReceipt> {
    let receipts = program
        .ordinary_map_owned_any_v2_map_root_get_any_caller_receipts()
        .ok()?;
    let matching = receipts
        .iter()
        .filter(|receipt| receipt.caller_owner == function.owner)
        .collect::<Vec<_>>();
    let [receipt] = matching.as_slice() else {
        return None;
    };
    receipt.validate_against_program(program).ok()?;
    Some((*receipt).clone())
}

fn map_root_get_any_caller_bridge_for_producer(
    program: &MirProgram,
    function: &MirFunction,
) -> Option<crate::core::mir::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerMirReceipt> {
    let receipts = program
        .ordinary_map_owned_any_v2_map_root_get_any_caller_receipts()
        .ok()?;
    let matching = receipts
        .iter()
        .filter(|receipt| receipt.producer_owner == function.owner)
        .collect::<Vec<_>>();
    let [receipt] = matching.as_slice() else {
        return None;
    };
    receipt.validate_against_program(program).ok()?;
    Some((*receipt).clone())
}

fn map_root_get_any_caller_string_drop_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    instruction_id: &str,
    value: &MirValueId,
) -> bool {
    map_root_get_any_caller_bridge_for_caller(program, function).is_some_and(|receipt| {
        receipt.caller_string_drop_instruction.as_str() == instruction_id
            && receipt.caller_string == *value
            && function
                .values
                .get(value)
                .is_some_and(|value| value.ty == receipt.string_type)
    })
}

fn ordinary_map_owned_any_v2_map_root_producer_caller_move_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    result: &MirValueId,
    source: &MirValueId,
) -> bool {
    ordinary_map_owned_any_v2_map_root_producer_caller_bridge_for_producer(program, function)
        .is_some_and(|receipt| {
            receipt.producer_return_value == *result && receipt.final_root == *source
        })
        || map_root_producer_caller_bridge_for_caller(program, function).is_some_and(|receipt| {
            receipt.caller_root == *result && receipt.caller_call_result == *source
        })
        || map_root_get_any_caller_bridge_for_producer(program, function).is_some_and(|receipt| {
            receipt.producer_return_value == *result && receipt.final_root == *source
        })
        || map_root_get_any_caller_bridge_for_caller(program, function).is_some_and(|receipt| {
            receipt.caller_root == *result && receipt.caller_call_result == *source
        })
}

fn ordinary_map_owned_any_v2_map_root_producer_caller_call_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    instruction_id: &crate::core::mir::MirInstructionId,
    instruction: &MirInstructionKind,
) -> bool {
    let MirInstructionKind::Call {
        result: Some(result),
        callee: ResolvedCallee::Function(target),
        ..
    } = instruction
    else {
        return false;
    };
    map_root_producer_caller_bridge_for_caller(program, function).is_some_and(|receipt| {
        receipt.caller_call_instruction == *instruction_id
            && receipt.call_target == *target
            && receipt.caller_call_result == *result
    }) || map_root_get_any_caller_bridge_for_caller(program, function).is_some_and(|receipt| {
        receipt.caller_call_instruction == *instruction_id
            && receipt.call_target == *target
            && receipt.caller_call_result == *result
    })
}

/// Consume Core's payload selection before the verifier admits any AnyOwner-v2
/// graph. The Core accessor derives the exact Set-entry mapping from its
/// Checker action ledger and validates the corresponding MIR values and
/// TypeDescs; this preflight only decides whether that receipt is mandatory
/// for the graph being checked.
fn validate_ordinary_map_owned_any_v2_payload_selection(
    program: &MirProgram,
) -> Result<(), String> {
    let owner_ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
    let payload_ty = crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id();
    let get_result_ty = crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
    let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
    let values_element_ty =
        crate::core::mir::types::ordinary_map_owned_any_v2_values_element_type_id();
    let has_type_signal = program.type_catalog().iter().any(|(id, descriptor)| {
        id == &owner_ty
            || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2
            || descriptor.layout == MirLayout::OrdinaryMapOwnedAnyV2
            || id == &payload_ty
            || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2Payload
            || descriptor.layout == MirLayout::OrdinaryMapOwnedAnyV2Payload
            || id == &get_result_ty
            || id == &values_ty
            || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2Values
            || id == &values_element_ty
            || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2ValuesElement
    });
    let has_value_signal = program.functions().values().any(|function| {
        function.result == owner_ty
            || function.result == payload_ty
            || function.result == get_result_ty
            || function.result == values_ty
            || function.values.values().any(|value| {
                value.ty == owner_ty
                    || value.ty == payload_ty
                    || value.ty == get_result_ty
                    || value.ty == values_ty
            })
    });
    let has_action_signal = program.functions().values().any(|function| {
        function
            .ownership
            .has_checker_ordinary_map_owned_any_v2_actions()
    });
    let has_instruction_signal = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                instruction_touches_ordinary_map_owned_any_v2(&instruction.kind)
                    || instruction_touches_ordinary_map_owned_any_v2_type(
                        function,
                        &instruction.kind,
                    )
            })
        })
    });
    let has_signal =
        has_type_signal || has_value_signal || has_action_signal || has_instruction_signal;

    // Call even for an unmarked graph so a future accessor cannot silently
    // introduce a receipt that the graph did not signal.
    match program.ordinary_map_owned_any_v2_payload_selection_receipt() {
        Ok(Some(receipt)) if has_signal => receipt
            .validate_against_program(program)
            .map_err(|message| format!("Core payload selection receipt failed: {message}")),
        Ok(Some(_)) => {
            Err("Core returned an AnyOwner-v2 payload selection receipt without a V2 signal".into())
        }
        Ok(None) if has_signal => {
            Err("V2 signal has no Core-owned Checker payload selection receipt".into())
        }
        Ok(None) => Ok(()),
        Err(message) => Err(format!("Core payload selection receipt failed: {message}")),
    }
}

/// Independently census every V2 Values instruction against the exact Core
/// witness. The receipt binds source/root/result/instruction/binding/Drop and
/// the complete payload-selection digest before symbolic execution.
#[cfg(test)]
fn validate_ordinary_map_owned_any_v2_values_receipts(program: &MirProgram) -> Result<(), String> {
    validate_ordinary_map_owned_any_v2_values_receipts_with_producer_caller(
        program, false, false, None,
    )
}

fn ordinary_map_owned_any_v2_values_result_receipts_for_gate(
    program: &MirProgram,
    allow_values_producer_caller: bool,
    values_result_crosscall_profile: Option<
        crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
    >,
) -> Result<Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesResultReceipt>, String> {
    if values_result_crosscall_profile.is_some() {
        // Result-branch producer Values operations have a distinct producer
        // receipt and no local ExplicitDrop result witness.
        return Ok(Vec::new());
    }
    if !allow_values_producer_caller {
        return program.ordinary_map_owned_any_v2_values_result_receipts();
    }
    let bridges = program.ordinary_map_owned_any_v2_values_producer_caller_receipts()?;
    program.ordinary_map_owned_any_v2_values_result_receipts_excluding_producer_caller(&bridges)
}

fn validate_ordinary_map_owned_any_v2_values_receipts_with_producer_caller(
    program: &MirProgram,
    allow_values_producer_caller: bool,
    allow_values_crosscall_string_caller: bool,
    values_result_crosscall_profile: Option<
        crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
    >,
) -> Result<(), String> {
    use crate::core::mir::MirInstructionKind as Instruction;

    let owner_ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
    let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
    let mut expected = BTreeSet::new();
    let mut observed = BTreeSet::new();
    let has_values_action =
        validate_ordinary_map_owned_any_v2_values_type_catalog(program, program.type_catalog())?;
    let mut has_signal = has_values_action;

    for (owner, function) in program.functions() {
        has_signal |= function.result == values_ty
            || function.values.values().any(|value| value.ty == values_ty)
            || function
                .ownership
                .has_checker_ordinary_map_owned_any_v2_values_actions();
        for instruction in function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
        {
            if matches!(
                instruction.kind,
                Instruction::OrdinaryMapOwnedAnyV2Values { .. }
            ) {
                has_signal = true;
                if !expected.insert((owner.clone(), instruction.id.clone())) {
                    return Err(format!(
                        "duplicate AnyOwner-v2 Values instruction '{}' in '{}'",
                        instruction.id, owner.0
                    ));
                }
            }
        }
    }
    if !has_signal {
        return Ok(());
    }

    let selection = program
        .ordinary_map_owned_any_v2_payload_selection_receipt()?
        .ok_or_else(|| "AnyOwner-v2 Values has no Checker payload-selection receipt".to_string())?;
    selection.validate_against_program(program)?;
    let producer_caller_receipts = if allow_values_producer_caller {
        program.ordinary_map_owned_any_v2_values_producer_caller_receipts()?
    } else {
        Vec::new()
    };
    // Result-branch callers have a separate Checker/MIR receipt and no
    // ordinary ExplicitDrop settlement receipt. Keep their producer Values
    // operation out of the local result census; the exact Result receipt is
    // added to `observed` below and must cover the entire Values inventory.
    let result_crosscall_receipts = if let Some(expected_version) = values_result_crosscall_profile
    {
        let receipts =
            program.ordinary_map_owned_any_v2_values_result_crosscall_string_caller_receipts()?;
        if receipts.len() != 1 {
            return Err(format!(
                "Result-branch Values capability requires one exact cross-call receipt, found {}",
                receipts.len()
            ));
        }
        let expected_profile = match expected_version {
            crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion::V3 => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_PROFILE
            }
            crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion::V4 => {
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_CROSSCALL_STRING_CALLER_INDEX1_PROFILE
            }
            crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion::V1
            | crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion::V2 => {
                return Err("straight-line Values caller profile cannot enter the Result capability gate".into())
            }
        };
        if receipts[0].profile != expected_profile
            || receipts[0].checker_caller.profile_version != expected_version
        {
            return Err(format!(
                "Result-branch Values capability requires exact {:?} receipt '{expected_profile}'",
                expected_version
            ));
        }
        super::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_type_owner_bridge(
            program,
            &receipts[0],
        )?;
        receipts
    } else {
        Vec::new()
    };
    let receipts = if values_result_crosscall_profile.is_some() {
        Vec::new()
    } else {
        ordinary_map_owned_any_v2_values_result_receipts_for_gate(
            program,
            allow_values_producer_caller,
            None,
        )?
    };
    let catalog = program.type_catalog();
    let dynamic_any_types = catalog
        .iter()
        .filter_map(|(id, descriptor)| {
            (descriptor.kind == MirTypeKind::DynamicAny).then_some(id.clone())
        })
        .collect::<Vec<_>>();
    if dynamic_any_types.len() != 1 {
        return Err(format!(
            "AnyOwner-v2 Values requires one canonical Checker DynamicAny TypeDesc, found {}",
            dynamic_any_types.len()
        ));
    }
    let dynamic_any_ty = &dynamic_any_types[0];
    let mut seen_checker_results = BTreeSet::new();

    for receipt in &receipts {
        if allow_values_producer_caller {
            // The filtered Core census already recomputed these local
            // ExplicitDrop witnesses while excluding only the exact producer
            // Return sites. The legacy all-results getter would reject that
            // same graph before this route-specific check could inspect it.
            receipt.validate()?;
        } else {
            receipt.validate_against_program(program)?;
        }
        let identity = (receipt.owner.clone(), receipt.instruction.clone());
        if !observed.insert(identity.clone()) {
            return Err(format!(
                "duplicate AnyOwner-v2 Values receipt for '{}:{}'",
                receipt.owner.0, receipt.instruction
            ));
        }
        if !expected.contains(&identity) {
            return Err(format!(
                "orphan AnyOwner-v2 Values receipt for '{}:{}'",
                receipt.owner.0, receipt.instruction
            ));
        }
        if !seen_checker_results.insert((receipt.owner.clone(), receipt.operation.clone())) {
            return Err(format!(
                "duplicate AnyOwner-v2 Values source/result witness for '{}:{}'",
                receipt.owner.0, receipt.operation.0
            ));
        }
        if receipt.payload_selection_digest != selection.selection_digest {
            return Err(format!(
                "AnyOwner-v2 Values receipt '{}' uses a different payload-selection witness",
                receipt.instruction
            ));
        }
        if receipt.mir_result_type != values_ty
            || receipt.checker_result_type == values_ty
            || receipt.root.as_str() != format!("local:{}", receipt.source_local.0 .0)
            || receipt.binding.as_str() != format!("local:{}", receipt.binding_local.0 .0)
        {
            return Err(format!(
                "AnyOwner-v2 Values receipt '{}' has a mismatched source/root/result/binding identity",
                receipt.instruction
            ));
        }
        let checker_result = catalog.get(&receipt.checker_result_type).ok_or_else(|| {
            format!(
                "AnyOwner-v2 Values receipt '{}' Checker List<DynamicAny> TypeDesc is absent",
                receipt.instruction
            )
        })?;
        if checker_result.kind != MirTypeKind::List
            || !matches!(
                &checker_result.layout,
                MirLayout::List { element } if element == dynamic_any_ty
            )
        {
            return Err(format!(
                "AnyOwner-v2 Values receipt '{}' does not preserve source shape List<DynamicAny>",
                receipt.instruction
            ));
        }
        let function = program
            .functions()
            .get(&receipt.owner)
            .ok_or_else(|| "AnyOwner-v2 Values receipt owner is absent".to_string())?;
        if function.values.get(&receipt.root).map(|value| &value.ty) != Some(&owner_ty)
            || function.values.get(&receipt.result).map(|value| &value.ty) != Some(&values_ty)
            || function.values.get(&receipt.binding).map(|value| &value.ty) != Some(&values_ty)
        {
            return Err(format!(
                "AnyOwner-v2 Values receipt '{}' has a mismatched MIR root/result/binding TypeDesc",
                receipt.instruction
            ));
        }
        let instruction = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| instruction.id == receipt.instruction)
            .collect::<Vec<_>>();
        if !matches!(
            instruction.as_slice(),
            [instruction]
                if matches!(
                    &instruction.kind,
                    Instruction::OrdinaryMapOwnedAnyV2Values { result, root }
                        if result == &receipt.result && root == &receipt.root
                )
        ) {
            return Err(format!(
                "AnyOwner-v2 Values receipt '{}' operands disagree with the MIR instruction",
                receipt.instruction
            ));
        }
        let direct_moves = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    &instruction.kind,
                    Instruction::Move { result, source }
                        if result == &receipt.binding && source == &receipt.result
                )
            })
            .count();
        let settlement_drops = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                instruction.id == receipt.settlement_instruction
                    && matches!(&instruction.kind, Instruction::Drop { value } if value == &receipt.binding)
            })
            .count();
        if direct_moves != 1 || settlement_drops != 1 {
            return Err(format!(
                "AnyOwner-v2 Values receipt '{}' does not identify one direct binding Move and its exact Drop/return settlement",
                receipt.instruction
            ));
        }
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            catalog
                .validate_list_glue(&values_ty, operation)
                .map_err(|message| format!("AnyOwner-v2 Values list TypeDesc: {message}"))?;
        }
    }
    for bridge in &producer_caller_receipts {
        super::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_type_bridge(
            program, bridge,
        )
        .map_err(|message| {
            format!("AnyOwner-v2 Values producer/caller type bridge rejected: {message}")
        })?;
        if !bridge.unpacks.is_empty() {
            return Err("cross-function Values indexed unpack has no Checker source receipt and is not admitted".into());
        }
        let identity = (
            bridge.producer_owner.clone(),
            bridge.producer_values_instruction.clone(),
        );
        let producer = program
            .functions()
            .get(&bridge.producer_owner)
            .ok_or_else(|| "AnyOwner-v2 Values producer function is absent".to_string())?;
        let exact_operation = producer.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                instruction.id == bridge.producer_values_instruction
                    && matches!(
                        &instruction.kind,
                        Instruction::OrdinaryMapOwnedAnyV2Values { result, .. }
                            if result == &bridge.producer_values_result
                    )
            })
        });
        if !expected.contains(&identity)
            || !exact_operation
            || producer.result != values_ty
            || bridge.producer_result_type != values_ty
            || bridge.owner_type != values_ty
        {
            return Err("AnyOwner-v2 Values producer/caller receipt does not cover one exact producer Values result".into());
        }
        if !observed.insert(identity) {
            return Err("duplicate AnyOwner-v2 Values producer/caller receipt coverage".into());
        }
    }
    let crosscall_receipts = if allow_values_crosscall_string_caller {
        program.ordinary_map_owned_any_v2_values_crosscall_string_caller_receipts()?
    } else {
        Vec::new()
    };
    for bridge in &crosscall_receipts {
        super::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_type_owner_bridge(
            program, bridge,
        )
        .map_err(|message| {
            format!("AnyOwner-v2 Values cross-call String type/owner bridge rejected: {message}")
        })?;
        let identity = (
            bridge.producer_owner.clone(),
            bridge.producer_values_instruction.clone(),
        );
        let producer = program
            .functions()
            .get(&bridge.producer_owner)
            .ok_or_else(|| "AnyOwner-v2 Values cross-call String producer is absent".to_string())?;
        let exact_operation = producer.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                instruction.id == bridge.producer_values_instruction
                    && matches!(
                        &instruction.kind,
                        Instruction::OrdinaryMapOwnedAnyV2Values { result, .. }
                            if result == &bridge.producer_values_result
                    )
            })
        });
        if !expected.contains(&identity)
            || !exact_operation
            || producer.result != values_ty
            || bridge.values_type != values_ty
            || !observed.insert(identity)
        {
            return Err("AnyOwner-v2 Values cross-call String receipt does not cover one unique exact producer Values result".into());
        }
    }
    for bridge in &result_crosscall_receipts {
        let identity = (
            bridge.producer_owner.clone(),
            bridge.producer_values_instruction.clone(),
        );
        let producer = program
            .functions()
            .get(&bridge.producer_owner)
            .ok_or_else(|| "Result-branch Values producer function is absent".to_string())?;
        let exact_operation = producer.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                instruction.id == bridge.producer_values_instruction
                    && matches!(
                        &instruction.kind,
                        Instruction::OrdinaryMapOwnedAnyV2Values { result, .. }
                            if result == &bridge.producer_values_result
                    )
            })
        });
        if !expected.contains(&identity)
            || !exact_operation
            || producer.result != values_ty
            || bridge.values_type != values_ty
            || !observed.insert(identity)
        {
            return Err("Result-branch Values cross-call receipt does not cover one unique exact producer Values result".into());
        }
    }
    if observed != expected {
        return Err(format!(
            "AnyOwner-v2 Values receipt census does not exactly cover MIR sites ({} receipts, {} operations)",
            observed.len(),
            expected.len()
        ));
    }
    Ok(())
}

/// Keep the Values TypeDesc pair tied to the exact Checker action. GetAny is
/// an independent V2 slice and must not inherit these List descriptors merely
/// because the graph has another V2 operation.
fn validate_ordinary_map_owned_any_v2_values_type_catalog(
    program: &MirProgram,
    catalog: &crate::core::mir::types::MirTypeCatalog,
) -> Result<bool, String> {
    let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
    let element_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_element_type_id();
    let has_values_action = program.functions().values().any(|function| {
        function
            .ownership
            .has_checker_ordinary_map_owned_any_v2_values_actions()
    });
    let has_values_descriptor = catalog.iter().any(|(id, descriptor)| {
        id == &values_ty
            || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2Values
            || descriptor.layout
                == (MirLayout::List {
                    element: element_ty.clone(),
                })
    });
    let has_element_descriptor = catalog.iter().any(|(id, descriptor)| {
        id == &element_ty
            || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2ValuesElement
            || descriptor.layout == MirLayout::OrdinaryMapOwnedAnyV2ValuesElement
    });

    if !has_values_action {
        if has_values_descriptor || has_element_descriptor {
            return Err(
                "orphan canonical AnyOwner-v2 Values/element TypeDesc without a Checker Values action"
                    .into(),
            );
        }
        return Ok(false);
    }

    let values = catalog.get(&values_ty).ok_or_else(|| {
        "AnyOwner-v2 Values action requires canonical Values and element TypeDescs".to_string()
    })?;
    let element = catalog.get(&element_ty).ok_or_else(|| {
        "AnyOwner-v2 Values action requires canonical Values and element TypeDescs".to_string()
    })?;
    if values.kind != MirTypeKind::OrdinaryMapOwnedAnyV2Values
        || values.layout
            != (MirLayout::List {
                element: element_ty.clone(),
            })
        || element.kind != MirTypeKind::OrdinaryMapOwnedAnyV2ValuesElement
        || element.layout != MirLayout::OrdinaryMapOwnedAnyV2ValuesElement
    {
        return Err(
            "AnyOwner-v2 Values action requires canonical Values and element TypeDescs".into(),
        );
    }
    for operation in [
        MirGlueOperation::MoveOut,
        MirGlueOperation::Clone,
        MirGlueOperation::Drop,
    ] {
        catalog
            .validate_list_glue(&values_ty, operation)
            .map_err(|message| format!("AnyOwner-v2 Values list TypeDesc: {message}"))?;
        catalog
            .validate_glue(&element_ty, operation)
            .map_err(|message| format!("AnyOwner-v2 Values element TypeDesc: {message}"))?;
    }
    Ok(true)
}

/// Independently census every V2 GetAny operation against Core's exact
/// Checker-derived result witness. The witness binds source/root/key/result,
/// direct binding Move, explicit Drop settlement and canonical miss i32; the
/// verifier never reconstructs that authority from V1 receipts or AST data.
fn validate_ordinary_map_owned_any_v2_get_any_receipts(program: &MirProgram) -> Result<(), String> {
    let owner_ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
    let result_ty = crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
    let payload_ty = crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id();
    let string_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let miss_i32_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let mut expected = BTreeSet::new();
    let mut observed = BTreeSet::new();
    let mut has_v2_signal = program.type_catalog().iter().any(|(id, descriptor)| {
        id == &owner_ty
            || id == &result_ty
            || id == &payload_ty
            || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2
            || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2Payload
    });

    for (owner, function) in program.functions() {
        has_v2_signal |= function
            .ownership
            .has_checker_ordinary_map_owned_any_v2_actions();
        for instruction in function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
        {
            if matches!(
                instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
            ) {
                has_v2_signal = true;
                if !expected.insert((owner.clone(), instruction.id.clone())) {
                    return Err(format!(
                        "duplicate AnyOwner-v2 GetAny instruction '{}' in '{}'",
                        instruction.id, owner.0
                    ));
                }
            }
        }
    }
    if !has_v2_signal {
        return Ok(());
    }
    let receipts = program.ordinary_map_owned_any_v2_get_any_result_receipts()?;

    for receipt in &receipts {
        receipt.validate_against_program(program)?;
        let identity = (receipt.owner.clone(), receipt.instruction.clone());
        if !observed.insert(identity.clone()) {
            return Err(format!(
                "duplicate AnyOwner-v2 GetAny result receipt for '{}:{}'",
                receipt.owner.0, receipt.instruction
            ));
        }
        if !expected.contains(&identity) {
            return Err(format!(
                "orphan AnyOwner-v2 GetAny result receipt for '{}:{}'",
                receipt.owner.0, receipt.instruction
            ));
        }
        let function = program
            .functions()
            .get(&receipt.owner)
            .ok_or_else(|| "AnyOwner-v2 GetAny receipt owner is absent".to_string())?;
        let instructions = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| instruction.id == receipt.instruction)
            .collect::<Vec<_>>();
        let [instruction] = instructions.as_slice() else {
            return Err(format!(
                "AnyOwner-v2 GetAny receipt '{}' does not identify one MIR instruction",
                receipt.instruction
            ));
        };
        match &instruction.kind {
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, root, key }
                if result == &receipt.result && root == &receipt.root && key == &receipt.key => {}
            _ => {
                return Err(format!(
                    "AnyOwner-v2 GetAny receipt '{}' operands disagree with its MIR instruction",
                    receipt.instruction
                ));
            }
        }
        if function.values.get(&receipt.root).map(|value| &value.ty) != Some(&owner_ty)
            || function.values.get(&receipt.key).map(|value| &value.ty) != Some(&string_ty)
            || function.values.get(&receipt.result).map(|value| &value.ty) != Some(&result_ty)
            || function.values.get(&receipt.binding).map(|value| &value.ty) != Some(&result_ty)
            || receipt.mir_result_type != result_ty
            || receipt.miss_payload_type != miss_i32_ty
        {
            return Err(format!(
                "AnyOwner-v2 GetAny receipt '{}' has a mismatched root/key/result/miss TypeDesc",
                receipt.instruction
            ));
        }
        let settlement = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| instruction.id == receipt.settlement_instruction)
            .collect::<Vec<_>>();
        if !matches!(
            settlement.as_slice(),
            [instruction] if matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == &receipt.binding)
        ) {
            return Err(format!(
                "AnyOwner-v2 GetAny result '{}' lacks its exact explicit settlement Drop",
                receipt.binding
            ));
        }
        let catalog = program.type_catalog();
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            catalog
                .validate_glue(&payload_ty, operation)
                .map_err(|message| format!("AnyOwner-v2 GetAny payload owner: {message}"))?;
            catalog
                .validate_glue(&result_ty, operation)
                .map_err(|message| format!("AnyOwner-v2 GetAny result: {message}"))?;
        }
        catalog
            .validate_copy_scalar(&miss_i32_ty)
            .map_err(|message| format!("AnyOwner-v2 GetAny miss sentinel: {message}"))?;
    }
    if observed != expected {
        return Err(format!(
            "AnyOwner-v2 GetAny result receipt census does not exactly cover MIR sites ({} receipts, {} operations)",
            observed.len(),
            expected.len()
        ));
    }
    Ok(())
}

/// Require each V2 checked unpack to be joined to its Checker-owned source
/// cast, directly-bound GetAny pair, exact settlement receipt, and selected
/// payload provenance. The result TypeDesc is the expected payload identity;
/// only an exact selected payload or the canonical i32 miss may authorize it.
fn validate_ordinary_map_owned_any_v2_unpack_receipts(program: &MirProgram) -> Result<(), String> {
    let mut expected = BTreeSet::new();
    for (owner, function) in program.functions() {
        for instruction in function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
        {
            if matches!(
                instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { .. }
            ) && !expected.insert((owner.clone(), instruction.id.clone()))
            {
                return Err(format!(
                    "duplicate AnyOwner-v2 unpack instruction '{}' in '{}'",
                    instruction.id, owner.0
                ));
            }
        }
    }

    // Source-only unpack receipts also matter: if lowering deleted the MIR
    // instruction, the Core derivation must still report the census mismatch.
    let has_get_any_source = program
        .ordinary_map_owned_any_v2_get_any_result_receipts()?
        .into_iter()
        .next()
        .is_some();
    if expected.is_empty() && !has_get_any_source {
        return Ok(());
    }

    let receipts = program.ordinary_map_owned_any_v2_unpack_result_receipts()?;
    let selection = program
        .ordinary_map_owned_any_v2_payload_selection_receipt()?
        .ok_or_else(|| "AnyOwner-v2 unpack has no payload-selection receipt".to_string())?;
    selection.validate_against_program(program)?;
    let get_any_receipts = program.ordinary_map_owned_any_v2_get_any_result_receipts()?;
    let catalog = program.type_catalog();
    for selected in &selection.selected_types {
        validate_ordinary_map_owned_any_v2_selected_payload_type(catalog, &selected.type_id)?;
    }
    let expected_pair_ty = crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
    let mut observed = BTreeSet::new();

    for receipt in &receipts {
        receipt.validate_against_program(program)?;
        let identity = (receipt.owner.clone(), receipt.instruction.clone());
        if !observed.insert(identity.clone()) {
            return Err(format!(
                "duplicate AnyOwner-v2 unpack result receipt for '{}:{}'",
                receipt.owner.0, receipt.instruction
            ));
        }
        if !expected.contains(&identity) {
            return Err(format!(
                "orphan AnyOwner-v2 unpack result receipt for '{}:{}'",
                receipt.owner.0, receipt.instruction
            ));
        }
        let function = program
            .functions()
            .get(&receipt.owner)
            .ok_or_else(|| "AnyOwner-v2 unpack receipt owner is absent".to_string())?;
        let instructions = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| instruction.id == receipt.instruction)
            .collect::<Vec<_>>();
        let [instruction] = instructions.as_slice() else {
            return Err(format!(
                "AnyOwner-v2 unpack receipt '{}' does not identify one MIR instruction",
                receipt.instruction
            ));
        };
        match &instruction.kind {
            MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack {
                result,
                pair,
                projection,
            } if result == &receipt.result
                && pair == &receipt.pair
                && projection == &receipt.projection => {}
            _ => {
                return Err(format!(
                    "AnyOwner-v2 unpack receipt '{}' operands disagree with its MIR instruction",
                    receipt.instruction
                ));
            }
        }
        let exact_pair_owners = get_any_receipts
            .iter()
            .filter(|get_any| {
                get_any.owner == receipt.owner
                    && get_any.operation == receipt.operation
                    && get_any.binding == receipt.pair
                    && get_any.binding_local == receipt.pair_binding_local
                    && get_any.binding_point == receipt.pair_binding_point
                    && get_any.settlement == receipt.settlement
                    && get_any.settlement_point == receipt.settlement_point
                    && get_any.settlement_instruction == receipt.settlement_instruction
            })
            .collect::<Vec<_>>();
        let [get_any] = exact_pair_owners.as_slice() else {
            return Err(format!(
                "AnyOwner-v2 unpack '{}' is not bound to one exact GetAny pair owner and settlement",
                receipt.instruction
            ));
        };
        if function.values.get(&receipt.pair).map(|value| &value.ty) != Some(&expected_pair_ty)
            || function.values.get(&receipt.result).map(|value| &value.ty)
                != Some(&receipt.result_type)
            || receipt.result_type != receipt.conversion_to
            || receipt.payload_selection_digest != selection.selection_digest
        {
            return Err(format!(
                "AnyOwner-v2 unpack '{}' pair/result TypeDesc or payload selection receipt is inconsistent",
                receipt.instruction
            ));
        }
        validate_ordinary_map_owned_any_v2_checked_get_unpack_type(catalog, &receipt.result_type)
            .map_err(|message| format!("{}: {message}", receipt.instruction))?;

        super::mir::validate_ordinary_map_owned_any_v2_unpack_payload_source(
            receipt, get_any, &selection,
        )?;
    }

    if observed != expected {
        return Err(format!(
            "AnyOwner-v2 unpack receipt census does not exactly cover MIR sites ({} receipts, {} operations)",
            observed.len(),
            expected.len()
        ));
    }
    Ok(())
}

/// Exact capability census for the ZeroMissI32 route. The source route
/// validator binds keys, payload selection, result observations, and source
/// settlements; this independent MIR scan makes sure the verifier admits no
/// neighboring Map operation or duplicated owner settlement alongside that
/// witness.
fn validate_ordinary_map_owned_any_v2_zero_miss_i32_census(
    program: &MirProgram,
) -> Result<(), String> {
    let pair_ty = crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
    let i32_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);

    let mut new_results = Vec::new();
    let mut set_ops = Vec::new();
    let mut get_any_ops = Vec::new();
    let mut unpack_ops = Vec::new();
    let mut pair_bindings = Vec::new();
    let mut pair_drops = Vec::new();
    let mut root_drops = Vec::new();
    let mut unexpected_map_ops = Vec::new();

    for (owner, function) in program.functions() {
        for instruction in function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
        {
            let subject = format!("{}:{}", owner.0, instruction.id);
            match &instruction.kind {
                MirInstructionKind::OrdinaryMapOwnedAnyV2New { result } => {
                    new_results.push((owner.clone(), result.clone()));
                }
                MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
                    result,
                    source,
                    key,
                    value,
                } => set_ops.push((
                    owner.clone(),
                    result.clone(),
                    source.clone(),
                    key.clone(),
                    value.clone(),
                )),
                MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, root, key } => {
                    get_any_ops.push((
                        owner.clone(),
                        instruction.id.clone(),
                        result.clone(),
                        root.clone(),
                        key.clone(),
                    ));
                }
                MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { result, pair, .. } => {
                    unpack_ops.push((
                        owner.clone(),
                        instruction.id.clone(),
                        result.clone(),
                        pair.clone(),
                    ));
                }
                MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } => {
                    root_drops.push((owner.clone(), root.clone()));
                }
                MirInstructionKind::Move { result, source }
                | MirInstructionKind::Copy { result, source }
                    if function.values.get(result).map(|value| &value.ty) == Some(&pair_ty)
                        && function.values.get(source).map(|value| &value.ty) == Some(&pair_ty) =>
                {
                    // A bound GetAny tuple has one explicit MIR transfer
                    // before its Checker-owned whole-pair Drop. Keep it out
                    // of the generic type-touch rejection, then prove below
                    // that the transfer is exactly the two receipted result
                    // bindings (no alias, extra copy, or unowned pair).
                    pair_bindings.push((owner.clone(), result.clone(), source.clone()));
                }
                MirInstructionKind::Drop { value }
                    if function.values.get(value).map(|value| &value.ty) == Some(&pair_ty) =>
                {
                    pair_drops.push((owner.clone(), value.clone()));
                }
                MirInstructionKind::MapRootNew { .. }
                | MirInstructionKind::MapRootSet { .. }
                | MirInstructionKind::MapRootRemove { .. }
                | MirInstructionKind::MapRootSize { .. }
                | MirInstructionKind::MapRootContains { .. }
                | MirInstructionKind::MapRootDrop { .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2Size { .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2Println { .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2Values { .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack { .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1NewResult { .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1SetResult { .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove { .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleNew { .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleSet { .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleSize { .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleContains { .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson { .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln { .. } => {
                    unexpected_map_ops.push(subject);
                }
                _ if instruction_touches_ordinary_map_owned_any_v2(&instruction.kind)
                    || instruction_touches_ordinary_map_owned_any_v2_type(
                        function,
                        &instruction.kind,
                    ) =>
                {
                    // Cloning, copying, moving, or otherwise passing a Map or
                    // GetAny pair through an unlisted instruction would add
                    // another owner operation to this closed slice.
                    unexpected_map_ops.push(subject);
                }
                _ => {}
            }
        }
    }

    if !unexpected_map_ops.is_empty() {
        return Err(format!(
            "unexpected Map operation(s): {}",
            unexpected_map_ops.join(", ")
        ));
    }
    if new_results.len() != 1
        || set_ops.len() != 1
        || get_any_ops.len() != 2
        || unpack_ops.len() != 2
        || pair_drops.len() != 2
        || root_drops.len() != 2
    {
        return Err(format!(
            "expected exactly New=1, Set=1, GetAny=2, DynamicAnyUnpack=2, pair Drop=2, root Drop=2; found New={}, Set={}, GetAny={}, DynamicAnyUnpack={}, pair Drop={}, root Drop={}",
            new_results.len(),
            set_ops.len(),
            get_any_ops.len(),
            unpack_ops.len(),
            pair_drops.len(),
            root_drops.len()
        ));
    }

    let [(new_owner, new_root)] = new_results.as_slice() else {
        unreachable!("the exact New count was checked above")
    };
    let [(set_owner, set_root, set_source, _, _)] = set_ops.as_slice() else {
        unreachable!("the exact Set count was checked above")
    };
    if new_owner != set_owner || set_source != new_root {
        return Err("the sole Set does not extend the sole New root".into());
    }
    let expected_roots = BTreeSet::from([new_root.clone(), set_root.clone()]);
    let observed_root_drops = root_drops
        .iter()
        .map(|(owner, root)| {
            if owner != new_owner {
                Err("a Map root Drop belongs to another callable".to_string())
            } else {
                Ok(root.clone())
            }
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if observed_root_drops.len() != 2 || observed_root_drops != expected_roots {
        return Err("both exact Map roots must be dropped once".into());
    }

    let get_any_receipts = program.ordinary_map_owned_any_v2_get_any_result_receipts()?;
    if get_any_receipts.len() != 2 {
        return Err(format!(
            "expected exactly two Checker-bound GetAny result owners, found {}",
            get_any_receipts.len()
        ));
    }
    let mut expected_pair_drops = BTreeSet::new();
    let mut expected_pair_bindings = BTreeSet::new();
    let mut expected_get_any_sites = BTreeSet::new();
    for receipt in &get_any_receipts {
        receipt.validate_against_program(program)?;
        if receipt.owner != *new_owner
            || receipt.root != *set_root
            || receipt.mir_result_type != pair_ty
            || receipt.miss_payload_type != i32_ty
            || !expected_pair_drops.insert((receipt.owner.clone(), receipt.binding.clone()))
            || !expected_pair_bindings.insert((
                receipt.owner.clone(),
                receipt.binding.clone(),
                receipt.result.clone(),
            ))
            || !expected_get_any_sites.insert((
                receipt.owner.clone(),
                receipt.instruction.clone(),
                receipt.result.clone(),
                receipt.root.clone(),
                receipt.key.clone(),
            ))
        {
            return Err("GetAny pair owner or canonical i32 miss TypeDesc is inconsistent".into());
        }
    }
    let observed_pair_bindings = pair_bindings.iter().cloned().collect::<BTreeSet<_>>();
    if pair_bindings.len() != 2
        || expected_pair_bindings.len() != 2
        || observed_pair_bindings != expected_pair_bindings
    {
        return Err(
            "GetAny tuple transfers are not exactly bound to both Checker result owners".into(),
        );
    }
    let observed_get_any_sites = get_any_ops.iter().cloned().collect::<BTreeSet<_>>();
    if observed_get_any_sites.len() != 2 || observed_get_any_sites != expected_get_any_sites {
        return Err("GetAny MIR sites are not exactly bound to both Checker result owners".into());
    }
    let observed_pair_drops = pair_drops.iter().cloned().collect::<BTreeSet<_>>();
    if observed_pair_drops.len() != 2 || observed_pair_drops != expected_pair_drops {
        return Err("each GetAny pair owner must have exactly one explicit Drop".into());
    }

    let unpack_receipts = program.ordinary_map_owned_any_v2_unpack_result_receipts()?;
    if unpack_receipts.len() != 2 {
        return Err(format!(
            "expected exactly two Checker-bound DynamicAnyUnpack owners, found {}",
            unpack_receipts.len()
        ));
    }
    let mut selected_payload_count = 0;
    let mut canonical_miss_count = 0;
    let expected_unpack_sites = unpack_receipts
        .iter()
        .map(|receipt| {
            receipt.validate_against_program(program)?;
            if receipt.owner != *new_owner
                || receipt.conversion_to != i32_ty
                || receipt.result_type != i32_ty
                || receipt.settlement
                    != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
            {
                return Err(format!(
                    "unpack '{}' does not produce the canonical i32 result under its exact owner receipt",
                    receipt.instruction
                ));
            }
            match receipt.payload_source {
                crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::SelectedSetPayload => {
                    selected_payload_count += 1;
                }
                crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::CanonicalI32Miss => {
                    canonical_miss_count += 1;
                }
            }
            Ok((
                receipt.owner.clone(),
                receipt.instruction.clone(),
                receipt.result.clone(),
                receipt.pair.clone(),
            ))
        })
        .collect::<Result<BTreeSet<_>, String>>()?;
    if selected_payload_count != 1 || canonical_miss_count != 1 || expected_unpack_sites.len() != 2
    {
        return Err(
            "the two i32 results must be one selected Set payload and one canonical miss".into(),
        );
    }
    if unpack_ops.iter().cloned().collect::<BTreeSet<_>>() != expected_unpack_sites {
        return Err(
            "DynamicAnyUnpack MIR sites are not exactly bound to both i32 owner receipts".into(),
        );
    }
    Ok(())
}

/// Independent operation census for the persistent-remove checked-miss
/// route. Core's route receipt proves key/root identity; this verifier-side
/// scan separately rejects extra Map operations, missing root settlements,
/// pair aliases, or a payload unpack whose provenance is not the canonical
/// i32 miss.
fn validate_ordinary_map_owned_any_v2_removed_key_checked_miss_census(
    program: &MirProgram,
) -> Result<(), String> {
    let pair_ty = crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
    let i32_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    if program.functions().len() != 1 {
        return Err("route must contain exactly one callable".into());
    }
    let mut counts = [0usize; 6];
    let mut pair_transfers = Vec::new();
    let mut pair_drops = Vec::new();
    for (owner, function) in program.functions() {
        for instruction in function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
        {
            match &instruction.kind {
                MirInstructionKind::OrdinaryMapOwnedAnyV2New { .. } => counts[0] += 1,
                MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. } => counts[1] += 1,
                MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { .. } => counts[2] += 1,
                MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. } => counts[3] += 1,
                MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { .. } => counts[4] += 1,
                MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { .. } => counts[5] += 1,
                MirInstructionKind::Move { result, source }
                | MirInstructionKind::Copy { result, source }
                    if function.values.get(result).map(|value| &value.ty) == Some(&pair_ty)
                        && function.values.get(source).map(|value| &value.ty) == Some(&pair_ty) =>
                {
                    pair_transfers.push((owner.clone(), result.clone(), source.clone()));
                }
                MirInstructionKind::Drop { value }
                    if function.values.get(value).map(|value| &value.ty) == Some(&pair_ty) =>
                {
                    pair_drops.push((owner.clone(), value.clone()));
                }
                _ if instruction_touches_ordinary_map_owned_any_v2(&instruction.kind)
                    || instruction_touches_ordinary_map_owned_any_v2_type(
                        function,
                        &instruction.kind,
                    ) =>
                {
                    return Err(format!(
                        "unexpected Map/Any owner-v2 instruction '{}:{}'",
                        owner.0, instruction.id
                    ));
                }
                _ => {}
            }
        }
    }
    if counts != [1, 1, 1, 1, 1, 3] {
        return Err(format!(
            "expected New/Set/Remove/GetAny/unpack/root-Drop counts 1/1/1/1/1/3, found {counts:?}"
        ));
    }
    if pair_transfers.len() != 1 || pair_drops.len() != 1 {
        return Err(format!(
            "expected one GetAny pair transfer and one explicit pair Drop; found {}/{}",
            pair_transfers.len(),
            pair_drops.len()
        ));
    }
    let get_any_receipts = program.ordinary_map_owned_any_v2_get_any_result_receipts()?;
    let [get_any] = get_any_receipts.as_slice() else {
        return Err(format!(
            "expected one exact GetAny result receipt, found {}",
            get_any_receipts.len()
        ));
    };
    get_any.validate_against_program(program)?;
    if get_any.miss_payload_type != i32_ty
        || get_any.settlement
            != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
    {
        return Err("GetAny result lacks canonical i32 miss or explicit pair settlement".into());
    }
    let unpack_receipts = program.ordinary_map_owned_any_v2_unpack_result_receipts()?;
    let [unpack] = unpack_receipts.as_slice() else {
        return Err(format!(
            "expected one exact Any unpack receipt, found {}",
            unpack_receipts.len()
        ));
    };
    unpack.validate_against_program(program)?;
    if unpack.owner != get_any.owner
        || unpack.operation != get_any.operation
        || unpack.pair != get_any.binding
        || unpack.conversion_to != i32_ty
        || unpack.result_type != i32_ty
        || unpack.payload_source
            != crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::CanonicalI32Miss
        || unpack.settlement != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
    {
        return Err(
            "unpack receipt is not the exact canonical i32 miss from the queried pair".into(),
        );
    }
    Ok(())
}

fn validate_ordinary_map_owned_any_v2_unpack_type(
    catalog: &MirTypeCatalog,
    ty: &ResolvedTypeId,
) -> Result<(), String> {
    let i32_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let bool_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
    if ty == &i32_ty {
        catalog.validate_copy_scalar(ty)?;
        for operation in [MirGlueOperation::Clone, MirGlueOperation::Drop] {
            catalog.validate_glue(ty, operation)?;
        }
        return Ok(());
    }
    if ty == &bool_ty {
        return Err("Bool is outside the owner-v2 Values unpack profile".into());
    }
    validate_ordinary_map_owned_any_v2_selected_payload_type(catalog, ty).map_err(|message| {
        format!(
            "ordinary Map/Any owner-v2 unpack TypeDesc '{}' is not a supported selected payload: {message}",
            ty.as_str()
        )
    })?;
    for operation in [
        MirGlueOperation::MoveOut,
        MirGlueOperation::Clone,
        MirGlueOperation::Drop,
    ] {
        catalog.validate_glue(ty, operation)?;
    }
    Ok(())
}

fn validate_ordinary_map_owned_any_v2_checked_get_unpack_type(
    catalog: &MirTypeCatalog,
    ty: &ResolvedTypeId,
) -> Result<(), String> {
    let bool_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
    if ty == &bool_ty {
        catalog.validate_copy_scalar(ty)?;
        for operation in [MirGlueOperation::Clone, MirGlueOperation::Drop] {
            catalog.validate_glue(ty, operation)?;
        }
        return Ok(());
    }
    validate_ordinary_map_owned_any_v2_unpack_type(catalog, ty)
}

fn validate_ordinary_map_owned_any_v2_selected_payload_type(
    catalog: &MirTypeCatalog,
    ty: &ResolvedTypeId,
) -> Result<(), String> {
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| format!("selected payload TypeDesc '{}' is absent", ty.as_str()))?;
    let i32_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let bool_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
    if ty == &i32_ty {
        return catalog.validate_copy_scalar(ty);
    }
    if ty == &bool_ty {
        return catalog.validate_copy_scalar(ty);
    }
    if descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String) {
        catalog.validate_owned_string(ty).map_err(|message| {
            format!(
                "selected payload TypeDesc '{}' is not canonical owned String: {message}",
                ty.as_str()
            )
        })?;
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            catalog.validate_glue(ty, operation)?;
        }
        return Ok(());
    }
    let MirLayout::Record { fields, .. } = &descriptor.layout else {
        return Err(format!(
            "selected payload TypeDesc '{}' is outside the i32/String/Label profile",
            ty.as_str()
        ));
    };
    let valid_label = descriptor.kind == MirTypeKind::Nominal
        && descriptor.abi == MirAbiClass::Aggregate
        && descriptor.ownership == MirOwnership::Move
        && descriptor.needs_drop_glue
        && descriptor.needs_clone_glue
        && descriptor.glue
            == (crate::core::mir::types::MirGlueContract {
                move_out: MirGlueKind::Aggregate,
                clone: MirGlueKind::Aggregate,
                drop: MirGlueKind::Aggregate,
            })
        && descriptor.drop_plan.is_some()
        && descriptor.variant_drop_plan.is_none()
        && fields.len() == 1
        && fields[0].name == "text"
        && catalog.get(&fields[0].ty).is_some_and(|field| {
            field.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String)
                && catalog.validate_owned_string(&field.id).is_ok()
        })
        && [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ]
        .into_iter()
        .all(|operation| catalog.validate_glue(ty, operation).is_ok());
    if !valid_label {
        return Err(format!(
            "selected payload TypeDesc '{}' is not canonical Label {{ text: string }}",
            ty.as_str()
        ));
    }
    Ok(())
}

fn worklist_has_mutable_loop(worklist: &crate::core::mir::MirProofWorklist) -> bool {
    fn block_has_mutable_loop(block: &crate::core::mir::MirProofBlock) -> bool {
        block.operations.iter().any(|operation| match operation {
            crate::core::mir::MirProofOperation::Loop {
                stationary: false, ..
            } => true,
            crate::core::mir::MirProofOperation::Branch {
                then_block,
                else_block,
                ..
            } => block_has_mutable_loop(then_block) || block_has_mutable_loop(else_block),
            crate::core::mir::MirProofOperation::Scope(inner) => block_has_mutable_loop(inner),
            crate::core::mir::MirProofOperation::Loop { body, .. } => block_has_mutable_loop(body),
            crate::core::mir::MirProofOperation::Bind { .. }
            | crate::core::mir::MirProofOperation::Call(_)
            | crate::core::mir::MirProofOperation::Eval { .. }
            | crate::core::mir::MirProofOperation::Assign { .. }
            | crate::core::mir::MirProofOperation::Math(_)
            | crate::core::mir::MirProofOperation::Break { .. }
            | crate::core::mir::MirProofOperation::Continue { .. }
            | crate::core::mir::MirProofOperation::Return { .. }
            | crate::core::mir::MirProofOperation::Exit => false,
        })
    }
    block_has_mutable_loop(&worklist.root)
}

fn worklist_has_ffi_call_path_loop(worklist: &crate::core::mir::MirProofWorklist) -> bool {
    fn block_has_profile(block: &crate::core::mir::MirProofBlock) -> bool {
        block.operations.iter().any(|operation| match operation {
            crate::core::mir::MirProofOperation::Loop {
                receipt: Some(receipt),
                body,
                ..
            } => receipt.typed.ffi_call_path.is_some() || block_has_profile(body),
            crate::core::mir::MirProofOperation::Branch {
                then_block,
                else_block,
                ..
            } => block_has_profile(then_block) || block_has_profile(else_block),
            crate::core::mir::MirProofOperation::Scope(inner) => block_has_profile(inner),
            crate::core::mir::MirProofOperation::Loop { body, .. } => block_has_profile(body),
            crate::core::mir::MirProofOperation::Bind { .. }
            | crate::core::mir::MirProofOperation::Call(_)
            | crate::core::mir::MirProofOperation::Eval { .. }
            | crate::core::mir::MirProofOperation::Assign { .. }
            | crate::core::mir::MirProofOperation::Math(_)
            | crate::core::mir::MirProofOperation::Break { .. }
            | crate::core::mir::MirProofOperation::Continue { .. }
            | crate::core::mir::MirProofOperation::Return { .. }
            | crate::core::mir::MirProofOperation::Exit => false,
        })
    }
    block_has_profile(&worklist.root)
}

/// Validate the minimum source/MIR binding required before an exact FFI loop
/// receipt may authorize any CFG backedge. The complete owner proof-worklist
/// validator runs before this gate; these checks keep the DFS authorization
/// tied to that same source-sealed call-path profile and its actual terminal
/// FFI block.
fn ffi_call_path_receipt_has_exact_terminal_call(
    function: &MirFunction,
    header: &crate::core::mir::MirBlockId,
    receipt: &crate::core::mir::MirProofLoopReceipt,
) -> bool {
    let Some(profile) = receipt.typed.ffi_call_path.as_ref() else {
        return false;
    };
    let Ok(expected_header) =
        crate::core::mir::MirBlockId::new(format!("bb:while.header:{}", profile.loop_node.0))
    else {
        return false;
    };
    let Ok(expected_exit) =
        crate::core::mir::MirBlockId::new(format!("bb:while.exit:{}", profile.loop_node.0))
    else {
        return false;
    };
    let Ok(expected_call) =
        crate::core::mir::MirInstructionId::new(format!("inst:call:{}", profile.call_node.0))
    else {
        return false;
    };
    let Ok(expected_break) =
        crate::core::mir::MirEdgeId::new(format!("edge:while.break:{}", profile.terminal_break.0))
    else {
        return false;
    };
    if profile.loop_node != receipt.typed.loop_node
        || &expected_header != header
        || receipt.typed.owner != function.owner
        || !receipt.typed.has_valid_source_witness()
        || !receipt.typed.writes.is_empty()
    {
        return false;
    }
    let Some(call_block) = receipt.ffi_call_block.as_ref() else {
        return false;
    };
    let call_locations = function
        .blocks
        .values()
        .flat_map(|block| {
            block
                .instructions
                .iter()
                .filter(|instruction| instruction.id == expected_call)
                .map(move |_| &block.id)
        })
        .collect::<Vec<_>>();
    let [actual_call_block] = call_locations.as_slice() else {
        return false;
    };
    if *actual_call_block != call_block
        || !function.blocks.get(call_block).is_some_and(|block| {
            block.instructions.iter().any(|instruction| {
                instruction.id == expected_call
                    && matches!(
                        &instruction.kind,
                        MirInstructionKind::Call {
                            callee: ResolvedCallee::Extern(_),
                            ..
                        }
                    )
            })
        })
    {
        return false;
    }
    let terminal_breaks = receipt
        .edges
        .iter()
        .filter(|edge| {
            edge.source_site == profile.terminal_break && edge.kind == MirProofLoopEdgeKind::Break
        })
        .collect::<Vec<_>>();
    let [terminal_break] = terminal_breaks.as_slice() else {
        return false;
    };
    terminal_break.from_block == *call_block
        && terminal_break.to_block.as_ref() == Some(&expected_exit)
        && terminal_break.mir_edge.as_ref() == Some(&expected_break)
        && function.blocks.get(call_block).is_some_and(|block| {
            matches!(
                &block.terminator,
                MirTerminator::Goto { edge, target, .. }
                    if edge == &expected_break && target == &expected_exit
            )
        })
}

/// Returns whether `program` references any Map/Any descriptor or ownership action.
pub(super) fn mir_program_touches_map_or_any(program: &MirProgram) -> bool {
    program.type_catalog().iter().any(|(_, descriptor)| {
        matches!(
            descriptor.kind,
            MirTypeKind::MapRoot
                | MirTypeKind::MapAnyOwnerV1
                | MirTypeKind::OrdinaryMapOwnerLifecycleI32StringV1
                | MirTypeKind::OrdinaryMapOwnedGetI32StringV1
                | MirTypeKind::OrdinaryMapOwnedAnyV2
                | MirTypeKind::OrdinaryMapOwnedAnyV2Payload
                | MirTypeKind::OrdinaryMapOwnedAnyV2Values
                | MirTypeKind::OrdinaryMapOwnedAnyV2ValuesElement
                | MirTypeKind::ManagedAnyI32StringV1
                | MirTypeKind::DynamicAny
        )
    }) || program.functions().values().any(|function| {
        function
            .ownership
            .has_checker_ordinary_map_owned_any_v2_actions()
            || function.ownership.has_checker_map_root_actions()
            || function.ownership.has_checker_map_any_owner_actions()
            || function
                .ownership
                .has_checker_ordinary_map_owner_lifecycle_v1_actions()
            || function
                .ownership
                .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
    })
}

struct CapabilityGate<'a> {
    program: &'a MirProgram,
    allow_typed_ffi_verifier_proofs: bool,
    allow_direct_print: bool,
    allow_ordinary_map_owned_any_v2_is_empty: bool,
    allow_ordinary_map_owned_any_v2_values_producer_caller: bool,
    allow_values_crosscall_string_caller: bool,
    /// Exact V3 or V4 Result-branch caller admission. This is disjoint from
    /// the straight-line V1/V2 bridge flag above.
    values_result_crosscall_profile:
        Option<crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion>,
    allow_ordinary_map_owned_any_v2_map_root_producer_caller: bool,
    allow_ordinary_map_owned_any_v2_keys_snapshot: bool,
    /// Exact Checker-bound GetOrDefault<i32> route only. The generic gate
    /// keeps this operation closed unless its dedicated profile preflight
    /// has already joined Checker and MIR receipts.
    allow_ordinary_map_owned_any_v2_get_or_default_i32: bool,
    /// Exact Checker-bound GetOrDefault<String> route. Kept disjoint from the
    /// i32 profile even though both use the same opaque V2 Map pair ABI.
    allow_ordinary_map_owned_any_v2_get_or_default_string: bool,
    /// Exact Checker-bound argv[0] → dynamic key → Keys/GetAny route only.
    /// The generic gate does not admit the argument projection or dynamic
    /// Keys semantics without this source-bound route validation.
    allow_ordinary_map_owned_any_v2_dynamic_keys_get: bool,
    /// Exact CheckedProgram-bound computed String-key owner-chain Result
    /// branch only. Generic capability validation leaves this closed.
    allow_computed_string_key_owner_chain: bool,
    /// Exact CheckedProgram-bound Values/New/Set Result branch. It admits
    /// its synthetic Result<MapRoot, i32> carrier only after the dedicated
    /// route replay has joined the complete Checker CFG to the fixed MIR
    /// business graph.
    allow_values_local_index_i32_result_branch: bool,
    errors: BTreeSet<String>,
    checked_types: HashSet<crate::core::ResolvedTypeId>,
    /// A move-owned Result constructor is admitted only when the same
    /// canonical graph contains the promoted generic Result projection
    /// receipt. This keeps the generic managed-payload extension local: an
    /// unrelated direct Result helper cannot silently widen the older mixed
    /// record/variant route before its own whole-program consumer matrix is
    /// closed.
    allow_result_move_variant: bool,
    allow_recoverable_flow_result: bool,
    /// A non-Copy tuple payload is admitted only when this canonical graph
    /// contains the explicit nested tuple projection receipt.  This keeps a
    /// bare `Option<(...)>` constructor/drop body outside the profile.
    allow_nested_option_tuple_variant: bool,
    /// Exact Result<string, i32> identities created by this profile's typed
    /// String getter. This managed Result shape is admitted only for these
    /// checked result values.
    ordinary_map_owned_get_result_string_types: HashSet<crate::core::ResolvedTypeId>,
    /// Checker result List<DynamicAny> identities retained only as source
    /// evidence for V2 Values receipts. Executable MIR rewrites these to the
    /// separate synthetic Values owner type.
    ordinary_map_owned_any_v2_checker_values_source_types: HashSet<crate::core::ResolvedTypeId>,
}

impl<'a> CapabilityGate<'a> {
    fn new(
        program: &'a MirProgram,
        allow_typed_ffi_verifier_proofs: bool,
        allow_direct_print: bool,
        allow_ordinary_map_owned_any_v2_is_empty: bool,
        allow_ordinary_map_owned_any_v2_values_producer_caller: bool,
        allow_ordinary_map_owned_any_v2_map_root_producer_caller: bool,
        allow_ordinary_map_owned_any_v2_keys_snapshot: bool,
        allow_values_crosscall_string_caller: bool,
        allow_ordinary_map_owned_any_v2_get_or_default_i32: bool,
        allow_ordinary_map_owned_any_v2_get_or_default_string: bool,
        allow_ordinary_map_owned_any_v2_dynamic_keys_get: bool,
        values_result_crosscall_profile: Option<
            crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
        >,
        allow_computed_string_key_owner_chain: bool,
        allow_values_local_index_i32_result_branch: bool,
    ) -> Self {
        let has_managed_result_call = program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    let (result, has_move_receipt) = match &instruction.kind {
                        MirInstructionKind::Call {
                            result: Some(result),
                            variant_call_contract: Some(receipt),
                            ..
                        } => (
                            result,
                            receipt.mode
                                == crate::core::mir::types::MirVariantCallAbiMode::MoveOwned,
                        ),
                        MirInstructionKind::ClosureInvoke { .. } => return false,
                        _ => return false,
                    };
                    has_move_receipt
                        && function.values.get(result).is_some_and(|value| {
                            program
                                .type_catalog()
                                .get(&value.ty)
                                .is_some_and(|descriptor| descriptor.kind == MirTypeKind::Result)
                        })
                })
            })
        });
        let checker_values_source_types =
            ordinary_map_owned_any_v2_values_result_receipts_for_gate(
                program,
                allow_ordinary_map_owned_any_v2_values_producer_caller,
                values_result_crosscall_profile,
            )
            .unwrap_or_default()
            .into_iter()
            .map(|receipt| receipt.checker_result_type)
            .chain(
                values_result_crosscall_profile
                    .and_then(|expected_version| {
                        program
                            .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_receipts()
                            .ok()
                            .and_then(|receipts| {
                                receipts
                                    .into_iter()
                                    .find(|receipt| {
                                        receipt.checker_caller.profile_version == expected_version
                                    })
                                    .map(|receipt| {
                                        [
                                            receipt.checker_producer_result.snapshot_type,
                                            receipt.checker_producer_result.surface_return_type,
                                        ]
                                    })
                            })
                    })
                    .into_iter()
                    .flatten(),
            )
            .collect();
        Self {
            program,
            allow_typed_ffi_verifier_proofs,
            allow_direct_print,
            allow_ordinary_map_owned_any_v2_is_empty,
            allow_ordinary_map_owned_any_v2_values_producer_caller,
            allow_values_crosscall_string_caller,
            values_result_crosscall_profile,
            allow_ordinary_map_owned_any_v2_map_root_producer_caller,
            allow_ordinary_map_owned_any_v2_keys_snapshot,
            allow_ordinary_map_owned_any_v2_get_or_default_i32,
            allow_ordinary_map_owned_any_v2_get_or_default_string,
            allow_ordinary_map_owned_any_v2_dynamic_keys_get,
            allow_computed_string_key_owner_chain,
            allow_values_local_index_i32_result_branch,
            errors: BTreeSet::new(),
            checked_types: HashSet::new(),
            allow_result_move_variant: program.instances().values().any(|instance| match &instance
                .contract
            {
                MirGenericInstanceContract::ScalarVariantProjection { contract } => {
                    contract.projection.nominal.as_str() == "builtin:type:Result"
                        && contract.projection.ownership == MirOwnership::Move
                        && matches!(
                            contract.projection.move_out_glue,
                            MirGlueKind::OwnedString | MirGlueKind::List
                        )
                }
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } => {
                    contract.projection.nominal.as_str() == "builtin:type:Result"
                        && contract.projection.ownership == MirOwnership::Move
                        && matches!(
                            contract.projection.move_out_glue,
                            MirGlueKind::OwnedString | MirGlueKind::List
                        )
                }
                _ => false,
            }) || has_managed_result_call,
            allow_recoverable_flow_result: program
                .transitions()
                .values()
                .any(|transition| transition.effect.is_recoverable()),
            allow_nested_option_tuple_variant: program.functions().values().any(|function| {
                function.blocks.values().any(|block| {
                    matches!(
                        &block.terminator,
                        MirTerminator::SwitchMove { arms, .. }
                            if arms.iter().any(|arm| arm.bindings.iter().any(|binding| binding.nested_tuple.is_some()))
                    )
                })
            }),
            ordinary_map_owned_get_result_string_types: program
                .functions()
                .values()
                .flat_map(|function| {
                    function.blocks.values().flat_map(move |block| {
                        block.instructions.iter().filter_map(move |instruction| {
                            let MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString {
                                result,
                                ..
                            } = &instruction.kind
                            else {
                                return None;
                            };
                            function.values.get(result).map(|value| value.ty.clone())
                        })
                    })
                })
                .collect(),
            // `validate_mir_capabilities` validates this receipt table before
            // constructing the gate. If derivation fails here, the earlier
            // preflight has already returned the same failure.
            ordinary_map_owned_any_v2_checker_values_source_types: checker_values_source_types,
        }
    }

    fn values_result_receipts(
        &self,
    ) -> Option<Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesResultReceipt>> {
        ordinary_map_owned_any_v2_values_result_receipts_for_gate(
            self.program,
            self.allow_ordinary_map_owned_any_v2_values_producer_caller,
            self.values_result_crosscall_profile,
        )
        .ok()
    }

    fn program_touches_map_or_any(&self) -> bool {
        mir_program_touches_map_or_any(self.program)
    }

    fn has_values_local_index_i32_result_branch_receipt(&self, function: &MirFunction) -> bool {
        if !self.allow_values_local_index_i32_result_branch
            || function.owner != crate::core::NodeId("function:main".into())
        {
            return false;
        }
        function
            .ownership
            .has_checker_values_local_index_i32_result_branch_receipt()
    }

    fn validate_values_local_index_i32_result_branch_result_type(
        &self,
        ty: &crate::core::ResolvedTypeId,
    ) -> Result<(), String> {
        if !self.allow_values_local_index_i32_result_branch
            || ty != &crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id()
        {
            return Err(
                "Result<MapRoot, i32> is outside the ValuesLocalIndexI32 Result-branch capability"
                    .into(),
            );
        }
        let checker_source =
            crate::core::mir::types::ordinary_map_owned_any_v2_source_result_type_id()?;
        self.program
            .type_catalog()
            .validate_ordinary_map_owned_any_v2_root_result(ty, &checker_source)
    }

    fn validate(&mut self) {
        let has_map_root_type = self
            .program
            .type_catalog()
            .iter()
            .any(|(_, descriptor)| descriptor.kind == MirTypeKind::MapRoot);
        let has_map_root_receipt = self
            .program
            .functions()
            .values()
            .any(|function| function.ownership.has_checker_map_root_actions());
        if has_map_root_type != has_map_root_receipt {
            self.error(
                "MapRoot TypeDesc presence must match Checker-owned MapRoot action receipts".into(),
            );
        }
        let map_any_owner_type = crate::core::mir::types::map_any_owner_v1_type_id();
        let has_map_any_owner_type = self.program.type_catalog().iter().any(|(id, descriptor)| {
            id == &map_any_owner_type
                || descriptor.kind == MirTypeKind::MapAnyOwnerV1
                || descriptor.layout == MirLayout::MapAnyOwnerV1
        });
        let has_map_any_owner_receipt = self
            .program
            .functions()
            .values()
            .any(|function| function.ownership.has_checker_map_any_owner_actions());
        if has_map_any_owner_type != has_map_any_owner_receipt {
            self.error(
                "MapAnyOwner v1 TypeDesc presence must match independent Checker owner receipts"
                    .into(),
            );
        }
        if has_map_any_owner_type {
            self.validate_map_any_owner_descriptor();
        }
        let has_managed_any_type = self.program.type_catalog().iter().any(|(_, descriptor)| {
            descriptor.kind == MirTypeKind::ManagedAnyI32StringV1
                || descriptor.layout == MirLayout::ManagedAnyI32StringV1
        });
        // The legacy typed GetAny/Values path materializes this shared
        // ManagedAny TypeDesc. V2 has its own payload, result, and Values
        // TypeDescs, validated against its exact Checker receipts below; its
        // operations must not be mistaken for legacy ManagedAny producers.
        let has_v1_get_any = self.program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
                            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
                    )
                })
            })
        });
        let has_v2_query = self.program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
                            | MirInstructionKind::OrdinaryMapOwnedAnyV2Values { .. }
                    )
                })
            })
        });
        let has_checked_i32_pack = self.program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ManagedAny {
                            operation: MirManagedAnyOperation::PackI32,
                            contract: Some(_),
                            ..
                        }
                    )
                })
            })
        });
        let has_managed_any_producer =
            has_v1_get_any || has_checked_i32_pack || (has_managed_any_type && has_v2_query);
        if has_managed_any_type != has_managed_any_producer {
            self.error("ManagedAny TypeDesc presence must match a separately Checker-receipted GetAny/Values or exact i32-pack producer".into());
        }
        let ordinary_map_owner_ty =
            crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
        let has_ordinary_map_owner_type = self.program.type_catalog().iter().any(|(id, desc)| {
            id == &ordinary_map_owner_ty
                || desc.kind == MirTypeKind::OrdinaryMapOwnerLifecycleI32StringV1
                || desc.layout == MirLayout::OrdinaryMapOwnerLifecycleI32StringV1
        });
        let has_ordinary_map_owner_receipt = self.program.functions().values().any(|function| {
            function
                .ownership
                .has_checker_ordinary_map_owner_lifecycle_v1_actions()
        });
        if has_ordinary_map_owner_type != has_ordinary_map_owner_receipt {
            self.error(
                "ordinary Map owner lifecycle v1 TypeDesc presence must match its independent Checker receipt".into(),
            );
        }
        if has_ordinary_map_owner_type {
            self.validate_ordinary_map_owner_descriptor();
        }
        let ordinary_map_owned_get_ty =
            crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
        let has_ordinary_map_owned_get_type =
            self.program.type_catalog().iter().any(|(id, descriptor)| {
                id == &ordinary_map_owned_get_ty
                    || descriptor.kind == MirTypeKind::OrdinaryMapOwnedGetI32StringV1
                    || descriptor.layout == MirLayout::OrdinaryMapOwnedGetI32StringV1
            });
        let has_ordinary_map_owned_get_receipt =
            self.program.functions().values().any(|function| {
                function
                    .ownership
                    .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            });
        if has_ordinary_map_owned_get_type != has_ordinary_map_owned_get_receipt {
            self.error(
                "ordinary Map owned-get v1 TypeDesc presence must match its independent Checker receipt".into(),
            );
        }
        if has_ordinary_map_owned_get_type {
            self.validate_ordinary_map_owned_get_descriptor();
        }
        let ordinary_map_owned_any_v2_ty =
            crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
        let ordinary_map_owned_any_v2_values_ty =
            crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        let ordinary_map_owned_any_v2_values_element_ty =
            crate::core::mir::types::ordinary_map_owned_any_v2_values_element_type_id();
        let has_ordinary_map_owned_any_v2_type =
            self.program.type_catalog().iter().any(|(id, descriptor)| {
                id == &ordinary_map_owned_any_v2_ty
                    || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2
                    || descriptor.layout == MirLayout::OrdinaryMapOwnedAnyV2
                    || id == &ordinary_map_owned_any_v2_values_ty
                    || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2Values
                    || id == &ordinary_map_owned_any_v2_values_element_ty
                    || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2ValuesElement
            });
        let has_ordinary_map_owned_any_v2_receipt =
            self.program.functions().values().any(|function| {
                function
                    .ownership
                    .has_checker_ordinary_map_owned_any_v2_actions()
            });
        if has_ordinary_map_owned_any_v2_type != has_ordinary_map_owned_any_v2_receipt {
            self.error(
                "ordinary Map/Any owner-v2 TypeDesc presence must match its independent Checker receipt".into(),
            );
        }
        if has_ordinary_map_owned_any_v2_type {
            self.validate_ordinary_map_owned_any_v2_descriptor();
            if self.program.functions().values().any(|function| {
                function
                    .ownership
                    .has_checker_ordinary_map_owned_any_v2_values_actions()
            }) {
                self.validate_ordinary_map_owned_any_v2_values_descriptor();
            }
        }
        if (has_map_root_type && has_map_any_owner_type)
            || (has_map_root_type && has_ordinary_map_owner_type)
            || (has_map_any_owner_type && has_ordinary_map_owner_type)
            || (has_ordinary_map_owned_get_type
                && (has_map_root_type || has_map_any_owner_type || has_ordinary_map_owner_type))
            || (has_ordinary_map_owned_any_v2_type
                && (has_map_root_type
                    || has_map_any_owner_type
                    || has_ordinary_map_owner_type
                    || has_ordinary_map_owned_get_type))
        {
            self.error(
                "MapRoot, MapAnyOwner v1, ordinary Map owner lifecycle v1, ordinary Map owned-get v1, and ordinary Map/Any owner-v2 descriptors are independent profiles and cannot share one MIR program".into(),
            );
        }
        for function in self.program.functions().values() {
            self.validate_function(function);
        }
        for instance in self.program.instances().values() {
            let Some(function) = self.program.functions().get(&instance.function) else {
                self.error(format!(
                    "instance '{}' executable '{}' is absent",
                    instance.id, instance.function.0
                ));
                continue;
            };
            match &instance.contract {
                MirGenericInstanceContract::ManagedAnyIdentity => {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_managed_any_identity_instance(instance, function)
                    {
                        self.error(format!(
                            "instance '{}' ManagedAny identity receipt rejected: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ManagedAnyTrapCleanup { .. } => {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_managed_any_trap_cleanup_instance(instance, function)
                    {
                        self.error(format!(
                            "instance '{}' ManagedAny trap-cleanup receipt rejected: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarIdentity
                | MirGenericInstanceContract::OwnedStringIdentity => {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_generic_identity_arguments(&instance.arguments)
                    {
                        self.error(format!(
                            "instance '{}' identity TypeDesc contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                    self.validate_identity_instance(function, instance.id.as_str());
                }
                MirGenericInstanceContract::ScalarArgumentProjection {
                    parameter_index,
                    parameter_count,
                } => {
                    let concrete = instance.arguments.first();
                    let argument_validation = self
                        .program
                        .type_catalog()
                        .validate_scalar_generic_arguments(&instance.arguments);
                    if let Err(message) = argument_validation {
                        self.error(format!(
                            "instance '{}' argument projection TypeDesc contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                    if let Some(concrete) = concrete {
                        if let Err(message) =
                            crate::core::mir::validate_generic_argument_projection_shape(
                                function,
                                concrete,
                                *parameter_index,
                                *parameter_count,
                            )
                        {
                            self.error(format!(
                                "instance '{}' argument projection receipt is invalid: {message}",
                                instance.id
                            ));
                        }
                    } else {
                        self.error(format!(
                            "instance '{}' argument projection has no concrete generic substitution",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::OwnedRecordUpdate { contract } => {
                    if let Some(concrete) = instance.arguments.first() {
                        if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_owned_record_update_generic_argument(concrete)
                        {
                            self.error(format!(
                                "instance '{}' owned record update argument is unsupported: {message}",
                                instance.id
                            ));
                        }
                    } else {
                        self.error(format!(
                            "instance '{}' owned record update has no concrete argument",
                            instance.id
                        ));
                    }
                    if let Err(message) = crate::core::mir::lower::validate_owned_record_update_mir(
                        function,
                        self.program.type_catalog(),
                        contract,
                    ) {
                        self.error(format!(
                            "instance '{}' owned record update contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarSetFacade { operation } => {
                    if let Err(message) = crate::core::mir::lower::validate_scalar_set_facade_mir(
                        function,
                        self.program.type_catalog(),
                        *operation,
                    ) {
                        self.error(format!(
                            "instance '{}' Set facade contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarListFacade { operation } => {
                    if let Err(message) = crate::core::mir::lower::validate_scalar_list_facade_mir(
                        function,
                        self.program.type_catalog(),
                        *operation,
                    ) {
                        self.error(format!(
                            "instance '{}' List facade contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarListConstruct { contract } => {
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_list_construct_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' List construction contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarListProjection {
                    contract,
                    index_value,
                } => {
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_list_projection_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                            *index_value,
                        )
                    {
                        self.error(format!(
                            "instance '{}' List projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarRecordProjection { contract } => {
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_record_projection_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' record projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarRecordUpdate { contract } => {
                    if let Err(message) = crate::core::mir::lower::validate_scalar_record_update_mir(
                        function,
                        self.program.type_catalog(),
                        contract,
                    ) {
                        self.error(format!(
                            "instance '{}' record update contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarTupleProjection { contract } => {
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_tuple_projection_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' tuple projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::OwnedRecordProjection { contract } => {
                    if let Some(concrete) = instance.arguments.first() {
                        if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_move_owned_record_payload(concrete)
                        {
                            self.error(format!(
                                "instance '{}' owned record projection argument is unsupported: {message}",
                                instance.id
                            ));
                        }
                    } else {
                        self.error(format!(
                            "instance '{}' owned record projection has no concrete argument",
                            instance.id
                        ));
                    }
                    if let Err(message) =
                        crate::core::mir::lower::validate_owned_record_projection_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' owned record projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::OwnedRecordProjectionDrop { contract } => {
                    if let Some(concrete) = instance.arguments.first() {
                        if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_move_owned_record_payload(concrete)
                        {
                            self.error(format!(
                                "instance '{}' owned record move/drop projection argument is unsupported: {message}",
                                instance.id
                            ));
                        }
                    } else {
                        self.error(format!(
                            "instance '{}' owned record move/drop projection has no concrete argument",
                            instance.id
                        ));
                    }
                    if let Err(message) =
                        crate::core::mir::lower::validate_owned_record_projection_drop_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' owned record move/drop projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarVariantPredicate { contract } => {
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_variant_predicate_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' Option predicate contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarVariantProjection { contract } => {
                    if contract.projection.ownership == MirOwnership::Move {
                        if !matches!(
                            contract.projection.nominal.as_str(),
                            "builtin:type:Option" | "builtin:type:Result"
                        ) {
                            self.error(format!(
                                "instance '{}' consuming generic variant projection is not the admitted managed Option/Result shape",
                                instance.id
                            ));
                        } else if instance.arguments.len() != 1 {
                            self.error(format!(
                                "instance '{}' owned generic variant projection requires one concrete argument",
                                instance.id
                            ));
                        } else if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_move_owned_payload(&instance.arguments[0])
                        {
                            self.error(format!(
                                "instance '{}' owned generic variant projection argument is unsupported: {message}",
                                instance.id
                            ));
                        }
                    }
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_variant_projection_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' generic variant projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } => {
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_variant_projection_fallback_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' Option fallback projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
            }
            if matches!(
                instance.contract,
                MirGenericInstanceContract::OwnedStringIdentity
            ) {
                if let Some(concrete) = instance.arguments.first() {
                    if let Err(message) =
                        self.program.type_catalog().validate_owned_string(concrete)
                    {
                        self.error(format!(
                            "instance '{}' owned String TypeDesc contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                    if let Err(message) =
                        crate::core::mir::validate_owned_string_identity_shape(function, concrete)
                    {
                        self.error(format!(
                            "instance '{}' owned String identity shape is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
            }
        }
    }

    fn validate_function(&mut self, function: &MirFunction) {
        let has_managed_any = crate::verifier::mir::direct_call_uses_managed_any(
            function,
            self.program.type_catalog(),
        );
        if has_managed_any
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction.kind, MirInstructionKind::ManagedAny { .. })
                })
            })
        {
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.error(format!(
                        "function '{}' ManagedAny sealed Checker receipt rejected: {error}",
                        function.owner.0
                    ));
                }
            }
            if function
                .ownership
                .ordinary_map_new_result_receipt()
                .is_none()
            {
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_managed_any_function(function)
                {
                    self.error(format!(
                        "function '{}' ManagedAny owner ledger rejected: {message}",
                        function.owner.0
                    ));
                }
            }
        }
        let map_any_owner_ty = crate::core::mir::types::map_any_owner_v1_type_id();
        let has_map_any_owner = function.result == map_any_owner_ty
            || function
                .values
                .values()
                .any(|value| value.ty == map_any_owner_ty)
            || function.ownership.has_checker_map_any_owner_actions()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    instruction_touches_map_any_owner(function, &instruction.kind)
                })
            });
        let has_map_root = function.result == crate::core::mir::types::map_root_type_id()
            || function
                .values
                .values()
                .any(|value| value.ty == crate::core::mir::types::map_root_type_id())
            || function.ownership.has_checker_map_root_actions()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    is_map_root_operation(&instruction.kind)
                        && !instruction_touches_map_any_owner(function, &instruction.kind)
                })
            });
        let ordinary_map_owner_ty =
            crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
        let has_ordinary_map_owner = function.result == ordinary_map_owner_ty
            || function
                .values
                .values()
                .any(|value| value.ty == ordinary_map_owner_ty)
            || function
                .ownership
                .has_checker_ordinary_map_owner_lifecycle_v1_actions()
            || function.blocks.values().any(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction_touches_ordinary_map_owner(&instruction.kind))
            });
        let ordinary_map_owned_get_ty =
            crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
        let has_ordinary_map_owned_get = function.result == ordinary_map_owned_get_ty
            || function
                .values
                .values()
                .any(|value| value.ty == ordinary_map_owned_get_ty)
            || function
                .ownership
                .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    instruction_touches_ordinary_map_owned_get(&instruction.kind)
                })
            });
        let ordinary_map_owned_any_v2_ty =
            crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
        let ordinary_map_owned_any_v2_values_ty =
            crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        let has_ordinary_map_owned_any_v2 = function.result == ordinary_map_owned_any_v2_ty
            || function.result == ordinary_map_owned_any_v2_values_ty
            || function.values.values().any(|value| {
                value.ty == ordinary_map_owned_any_v2_ty
                    || value.ty == ordinary_map_owned_any_v2_values_ty
            })
            || function
                .ownership
                .has_checker_ordinary_map_owned_any_v2_actions()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    instruction_touches_ordinary_map_owned_any_v2(&instruction.kind)
                        || instruction_touches_ordinary_map_owned_any_v2_type(
                            function,
                            &instruction.kind,
                        )
                })
            });
        if (has_map_any_owner && has_map_root)
            || (has_ordinary_map_owner && has_map_any_owner)
            || (has_ordinary_map_owner && has_map_root)
            || (has_ordinary_map_owned_get
                && (has_ordinary_map_owner || has_map_any_owner || has_map_root))
            || (has_ordinary_map_owned_any_v2
                && (has_map_root
                    || has_map_any_owner
                    || has_ordinary_map_owner
                    || has_ordinary_map_owned_get))
        {
            self.error(format!(
                "function '{}' mixes independent MapRoot, MapAnyOwner v1, ordinary Map owner lifecycle v1, ordinary Map owned-get v1, or ordinary Map/Any owner-v2 ownership profiles",
                function.owner.0
            ));
        }
        if has_map_any_owner {
            if !function.ownership.has_checker_map_any_owner_actions() {
                self.error(format!(
                    "function '{}' uses MapAnyOwner v1 without an independent Checker owner receipt",
                    function.owner.0
                ));
            }
            if function.ownership.has_checker_map_root_actions() {
                self.error(format!(
                    "function '{}' raw MapRoot receipt cannot authorize MapAnyOwner v1 operations",
                    function.owner.0
                ));
            }
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.error(format!(
                        "function '{}' MapAnyOwner v1 MIR validation failed: {error}",
                        function.owner.0
                    ));
                }
            }
        }
        if has_map_root {
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.error(format!(
                        "function '{}' MapRoot MIR validation failed: {error}",
                        function.owner.0
                    ));
                }
            }
        }
        if has_ordinary_map_owner {
            if !function
                .ownership
                .has_checker_ordinary_map_owner_lifecycle_v1_actions()
            {
                self.error(format!(
                    "function '{}' uses ordinary Map owner lifecycle v1 without its independent Checker receipt",
                    function.owner.0
                ));
            }
            if function.ownership.has_checker_map_root_actions()
                || function.ownership.has_checker_map_any_owner_actions()
            {
                self.error(format!(
                    "function '{}' ordinary Map owner lifecycle v1 cannot borrow another Map profile receipt",
                    function.owner.0
                ));
            }
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.error(format!(
                        "function '{}' ordinary Map owner lifecycle v1 MIR validation failed: {error}",
                        function.owner.0
                    ));
                }
            }
        }
        if has_ordinary_map_owned_get {
            if !function
                .ownership
                .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            {
                self.error(format!(
                    "function '{}' uses ordinary Map owned-get v1 without its independent Checker receipt",
                    function.owner.0
                ));
            }
            if function.ownership.has_checker_map_root_actions()
                || function.ownership.has_checker_map_any_owner_actions()
                || function
                    .ownership
                    .has_checker_ordinary_map_owner_lifecycle_v1_actions()
            {
                self.error(format!(
                    "function '{}' ordinary Map owned-get v1 cannot borrow another Map profile receipt",
                    function.owner.0
                ));
            }
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.error(format!(
                        "function '{}' ordinary Map owned-get v1 MIR validation failed: {error}",
                        function.owner.0
                    ));
                }
            }
        }
        if has_ordinary_map_owned_any_v2 {
            if !function
                .ownership
                .has_checker_ordinary_map_owned_any_v2_actions()
                && !(self.allow_ordinary_map_owned_any_v2_values_producer_caller
                    && ordinary_map_owned_any_v2_values_producer_caller_bridge_for_caller(
                        self.program,
                        function,
                    )
                    .is_some())
                && !(self.allow_values_crosscall_string_caller
                    && ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_caller(
                        self.program,
                        function,
                    )
                    .is_some())
                && !(self.values_result_crosscall_profile.is_some_and(|version| {
                    ordinary_map_owned_any_v2_values_result_crosscall_string_caller_bridge_for_owner(
                        self.program,
                        &function.owner,
                        false,
                        version,
                    )
                    .is_some()
                }))
                && !(self.allow_ordinary_map_owned_any_v2_map_root_producer_caller
                    && (ordinary_map_owned_any_v2_map_root_producer_caller_bridge_for_producer(
                        self.program,
                        function,
                    )
                    .is_some()
                        || map_root_producer_caller_bridge_for_caller(self.program, function)
                            .is_some()
                        || map_root_get_any_caller_bridge_for_producer(self.program, function)
                            .is_some()
                        || map_root_get_any_caller_bridge_for_caller(self.program, function)
                            .is_some()))
            {
                self.error(format!(
                    "function '{}' uses ordinary Map/Any owner-v2 without its independent Checker receipt",
                    function.owner.0
                ));
            }
            if function.ownership.has_checker_map_root_actions()
                || function.ownership.has_checker_map_any_owner_actions()
                || function
                    .ownership
                    .has_checker_ordinary_map_owner_lifecycle_v1_actions()
                || function
                    .ownership
                    .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            {
                self.error(format!(
                    "function '{}' ordinary Map/Any owner-v2 cannot borrow another Map profile receipt",
                    function.owner.0
                ));
            }
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.error(format!(
                        "function '{}' ordinary Map/Any owner-v2 MIR validation failed: {error}",
                        function.owner.0
                    ));
                }
            }
        }
        for value in function.values.values() {
            self.validate_type(&value.ty, &format!("function '{}' value", function.owner.0));
        }
        self.validate_type(
            &function.result,
            &format!("function '{}' result", function.owner.0),
        );
        if let Err(message) = crate::core::mir::validate_entry_invariant_shapes(function) {
            self.error(format!("function '{}' {message}", function.owner.0));
        }
        self.validate_acyclic_cfg(function);
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                self.validate_instruction(
                    function,
                    &instruction.id,
                    &instruction.kind,
                    instruction.id.as_str(),
                );
            }
            self.validate_terminator(function, &block.terminator, block.id.as_str());
        }
        for event in &function.ownership.events {
            let unsupported =
                if event.kind == crate::core::mir::MirOwnershipEventKind::TransferSession {
                    !function.blocks.values().any(|block| {
                        block.instructions.iter().any(|instruction| {
                            let point = instruction
                                .id
                                .as_str()
                                .split_once(':')
                                .and_then(|(_, rest)| rest.split_once(':'))
                                .map(|(_, point)| point);
                            matches!(
                                &instruction.kind,
                                MirInstructionKind::SessionCall {
                                    endpoint,
                                    contract: Some(contract),
                                    ..
                                } if point == Some(event.point.0.as_str())
                                    && event.value.as_ref() == Some(endpoint)
                                    && !contract.terminal
                            )
                        })
                    })
                } else {
                    matches!(
                        event.kind,
                        crate::core::mir::MirOwnershipEventKind::TransferChild
                            | crate::core::mir::MirOwnershipEventKind::BorrowMut
                    )
                };
            if unsupported {
                self.error(format!(
                    "function '{}' ownership effect '{}' is outside the MIR verifier capability",
                    function.owner.0,
                    event.kind.as_str()
                ));
            }
        }
        if has_map_any_owner {
            self.validate_map_any_owner_function(function);
        }
        if has_ordinary_map_owner {
            self.validate_ordinary_map_owner_function(function);
        }
        if has_ordinary_map_owned_get {
            self.validate_ordinary_map_owned_get_function(function);
        }
        if has_ordinary_map_owned_any_v2 {
            self.validate_ordinary_map_owned_any_v2_function(function);
        }
    }

    fn validate_map_root_type(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        role: &str,
    ) {
        if function
            .values
            .get(value)
            .is_none_or(|value| value.ty != crate::core::mir::types::map_root_type_id())
        {
            self.error(format!(
                "{subject} MapRoot {role} does not use the canonical MapRoot TypeDesc"
            ));
        }
    }

    fn validate_map_any_owner_descriptor(&mut self) {
        let ty = crate::core::mir::types::map_any_owner_v1_type_id();
        let catalog = self.program.type_catalog();
        for operation in [MirGlueOperation::MoveOut, MirGlueOperation::Drop] {
            if let Err(message) = catalog.validate_glue(&ty, operation) {
                self.error(format!("MapAnyOwner v1 TypeDesc rejected: {message}"));
            }
        }
        if catalog.validate_glue(&ty, MirGlueOperation::Clone).is_ok() {
            self.error("MapAnyOwner v1 TypeDesc unexpectedly exposes generic Clone glue".into());
        }
    }

    fn validate_ordinary_map_owner_descriptor(&mut self) {
        let ty = crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
        let catalog = self.program.type_catalog();
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            if let Err(message) = catalog.validate_glue(&ty, operation) {
                self.error(format!(
                    "ordinary Map owner lifecycle v1 TypeDesc rejected: {message}"
                ));
            }
        }
    }

    fn validate_ordinary_map_owned_get_descriptor(&mut self) {
        let ty = crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
        let catalog = self.program.type_catalog();
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            if let Err(message) = catalog.validate_glue(&ty, operation) {
                self.error(format!(
                    "ordinary Map owned-get v1 TypeDesc rejected: {message}"
                ));
            }
        }
    }

    fn validate_ordinary_map_owned_any_v2_descriptor(&mut self) {
        let ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
        let catalog = self.program.type_catalog();
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            if let Err(message) = catalog.validate_glue(&ty, operation) {
                self.error(format!(
                    "ordinary Map/Any owner-v2 TypeDesc rejected: {message}"
                ));
            }
        }
    }

    fn validate_ordinary_map_owned_any_v2_values_descriptor(&mut self) {
        let ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        let catalog = self.program.type_catalog();
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            if let Err(message) = catalog.validate_list_glue(&ty, operation) {
                self.error(format!(
                    "ordinary Map/Any owner-v2 Values TypeDesc rejected: {message}"
                ));
            }
        }
    }

    fn values_producer_return_is_receipted(&self, function: &MirFunction) -> bool {
        if !self.allow_ordinary_map_owned_any_v2_values_producer_caller
            && !self.allow_values_crosscall_string_caller
            && self.values_result_crosscall_profile.is_none()
        {
            return false;
        }
        let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        ordinary_map_owned_any_v2_values_producer_caller_bridge_for_producer(self.program, function)
            .is_some_and(|receipt| {
                receipt.producer_result_type == values_ty
                    && receipt.producer_return_value.as_str().starts_with("expr:")
                    && function.result == values_ty
            })
            || (self.allow_values_crosscall_string_caller
                && ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_producer(
                    self.program,
                    function,
                )
                .is_some_and(|receipt| {
                    receipt.values_type == values_ty
                        && receipt.producer_return_value.as_str().starts_with("expr:")
                        && function.result == values_ty
                }))
            || self.values_result_crosscall_profile.is_some_and(|version| {
                ordinary_map_owned_any_v2_values_result_crosscall_string_caller_bridge_for_owner(
                    self.program,
                    &function.owner,
                    true,
                    version,
                )
                .is_some_and(|receipt| {
                    receipt.values_type == values_ty
                        && function.result == values_ty
                        && receipt.producer_return_value.as_str().starts_with("expr:")
                })
            })
    }

    fn values_call_is_receipted(
        &self,
        function: &MirFunction,
        instruction: &crate::core::mir::MirInstructionId,
        target: &crate::core::NodeId,
        result: &MirValueId,
    ) -> bool {
        if !self.allow_ordinary_map_owned_any_v2_values_producer_caller
            && !self.allow_values_crosscall_string_caller
            && self.values_result_crosscall_profile.is_none()
        {
            return false;
        }
        let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        ordinary_map_owned_any_v2_values_producer_caller_bridge_for_caller(self.program, function)
            .is_some_and(|receipt| {
                receipt.call_instruction == *instruction
                    && receipt.producer_owner == *target
                    && receipt.call_result == *result
                    && receipt.call_result_type == values_ty
                    && function
                        .values
                        .get(result)
                        .is_some_and(|value| value.ty == values_ty)
            })
            || (self.allow_values_crosscall_string_caller
                && ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_caller(
                    self.program,
                    function,
                )
                .is_some_and(|receipt| {
                    receipt.caller_call_instruction == *instruction
                        && receipt.call_target == *target
                        && receipt.caller_call_result == *result
                        && receipt.values_type == values_ty
                        && function
                            .values
                            .get(result)
                            .is_some_and(|value| value.ty == values_ty)
                }))
            || self.values_result_crosscall_profile.is_some_and(|version| {
                ordinary_map_owned_any_v2_values_result_crosscall_string_caller_bridge_for_owner(
                    self.program,
                    &function.owner,
                    false,
                    version,
                )
                .is_some_and(|receipt| {
                    receipt.caller_call_instruction == *instruction
                        && receipt.call_target == *target
                        && receipt.caller_call_result == *result
                        && receipt.values_type == values_ty
                        && function
                            .values
                            .get(result)
                            .is_some_and(|value| value.ty == values_ty)
                })
            })
    }

    fn validate_ordinary_map_owned_get_function(&mut self, function: &MirFunction) {
        let owner_ty = crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
        if function.result == owner_ty {
            self.error(format!(
                "function '{}' cannot return an ordinary Map owned-get v1 root",
                function.owner.0
            ));
        }
        for parameter in &function.parameters {
            if function
                .values
                .get(parameter)
                .is_some_and(|value| value.ty == owner_ty)
            {
                self.error(format!(
                    "function '{}' cannot receive an ordinary Map owned-get v1 root as a parameter",
                    function.owner.0
                ));
            }
        }
        for block in function.blocks.values() {
            for parameter in &block.parameters {
                if function
                    .values
                    .get(&parameter.value)
                    .is_some_and(|value| value.ty == owner_ty)
                {
                    self.error(format!(
                        "function '{}' cannot receive an ordinary Map owned-get v1 root as a block parameter",
                        function.owner.0
                    ));
                }
            }
            for instruction in &block.instructions {
                if instruction_touches_ordinary_map_owned_get(&instruction.kind)
                    || (matches!(&instruction.kind, MirInstructionKind::Clone { .. })
                        && instruction_touches_ordinary_map_owned_get_type(
                            function,
                            &instruction.kind,
                        ))
                {
                    self.validate_ordinary_map_owned_get_instruction(
                        function,
                        &instruction.kind,
                        instruction.id.as_str(),
                    );
                } else if instruction_touches_ordinary_map_owned_get_type(
                    function,
                    &instruction.kind,
                ) {
                    self.error(format!(
                        "{} ordinary Map owned-get v1 value is used outside its dedicated receipt-bound operation",
                        instruction.id
                    ));
                }
            }
        }
    }

    fn validate_ordinary_map_owned_any_v2_function(&mut self, function: &MirFunction) {
        let owner_ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
        let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        let exact_values_local_index_i32_result_branch =
            self.has_values_local_index_i32_result_branch_receipt(function);
        let receipted_map_root_producer = self
            .allow_ordinary_map_owned_any_v2_map_root_producer_caller
            && (ordinary_map_owned_any_v2_map_root_producer_caller_bridge_for_producer(
                self.program,
                function,
            )
            .is_some()
                || map_root_get_any_caller_bridge_for_producer(self.program, function).is_some());
        if function.result == owner_ty && !receipted_map_root_producer {
            self.error(format!(
                "function '{}' cannot return an ordinary Map/Any owner-v2 root",
                function.owner.0
            ));
        }
        if function.result == values_ty && !self.values_producer_return_is_receipted(function) {
            self.error(format!(
                "function '{}' cannot return an ordinary Map/Any owner-v2 Values List",
                function.owner.0
            ));
        }
        for parameter in &function.parameters {
            if function
                .values
                .get(parameter)
                .is_some_and(|value| value.ty == owner_ty)
            {
                self.error(format!(
                    "function '{}' cannot receive an ordinary Map/Any owner-v2 root as a parameter",
                    function.owner.0
                ));
            }
            if function
                .values
                .get(parameter)
                .is_some_and(|value| value.ty == values_ty)
            {
                self.error(format!(
                    "function '{}' cannot receive an ordinary Map/Any owner-v2 Values List as a parameter",
                    function.owner.0
                ));
            }
        }
        for block in function.blocks.values() {
            for parameter in &block.parameters {
                if function
                    .values
                    .get(&parameter.value)
                    .is_some_and(|value| value.ty == owner_ty)
                    && !exact_values_local_index_i32_result_branch
                {
                    self.error(format!(
                        "function '{}' cannot receive an ordinary Map/Any owner-v2 root as a block parameter",
                        function.owner.0
                    ));
                }
                if function
                    .values
                    .get(&parameter.value)
                    .is_some_and(|value| value.ty == values_ty)
                {
                    self.error(format!(
                        "function '{}' cannot receive an ordinary Map/Any owner-v2 Values List as a block parameter",
                        function.owner.0
                    ));
                }
            }
            for instruction in &block.instructions {
                let is_owner_clone = matches!(&instruction.kind, MirInstructionKind::Clone { .. })
                    && instruction_touches_ordinary_map_owned_any_v2_type(
                        function,
                        &instruction.kind,
                    );
                let is_get_any_binding_move = match &instruction.kind {
                    MirInstructionKind::Move { result, source } => self
                        .program
                        .ordinary_map_owned_any_v2_get_any_result_receipts()
                        .ok()
                        .is_some_and(|receipts| {
                            receipts.iter().any(|receipt| {
                                receipt.owner == function.owner
                                    && receipt.result == *source
                                    && receipt.binding == *result
                            })
                        }),
                    _ => false,
                };
                let is_values_binding_move = match &instruction.kind {
                    MirInstructionKind::Move { result, source } => {
                        self.values_result_receipts().is_some_and(|receipts| {
                            receipts.iter().any(|receipt| {
                                receipt.owner == function.owner
                                    && receipt.result == *source
                                    && receipt.binding == *result
                            })
                        }) || (self.allow_ordinary_map_owned_any_v2_values_producer_caller
                            && (ordinary_map_owned_any_v2_values_producer_caller_bridge_for_producer(
                                self.program,
                                function,
                            )
                            .is_some_and(|receipt| {
                                (receipt.producer_values_result == *source
                                    && receipt.producer_snapshot_value == *result)
                                    || (receipt.producer_snapshot_value == *source
                                        && receipt.producer_return_value == *result)
                            })
                                || ordinary_map_owned_any_v2_values_producer_caller_bridge_for_caller(
                                    self.program,
                                    function,
                                )
                                .is_some_and(|receipt| {
                                    receipt.call_result == *source
                                        && receipt.caller_binding == *result
                                })))
                        || (self.allow_values_crosscall_string_caller
                            && ordinary_map_owned_any_v2_values_crosscall_string_caller_move_is_receipted(
                                self.program,
                                function,
                                result,
                                source,
                            ))
                        || self.values_result_crosscall_profile.is_some_and(|version| {
                            ordinary_map_owned_any_v2_values_result_crosscall_string_caller_move_is_receipted(
                                self.program,
                                function,
                                result,
                                source,
                                version,
                            )
                        })
                        || (self.allow_ordinary_map_owned_any_v2_map_root_producer_caller
                            && ordinary_map_owned_any_v2_map_root_producer_caller_move_is_receipted(
                                self.program,
                                function,
                                result,
                                source,
                            ))
                    }
                    _ => false,
                };
                let is_values_len = matches!(
                    &instruction.kind,
                    MirInstructionKind::ListOp {
                        operation: crate::core::mir::MirListOperation::Len,
                        list,
                        argument: None,
                        ..
                    } if function.values.get(list).is_some_and(|value| value.ty == values_ty)
                );
                let is_values_producer_caller_call = match &instruction.kind {
                    MirInstructionKind::Call {
                        result: Some(result),
                        callee: ResolvedCallee::Function(target),
                        ..
                    } => self.values_call_is_receipted(function, &instruction.id, target, result),
                    _ => false,
                };
                let is_map_root_producer_caller_call = self
                    .allow_ordinary_map_owned_any_v2_map_root_producer_caller
                    && ordinary_map_owned_any_v2_map_root_producer_caller_call_is_receipted(
                        self.program,
                        function,
                        &instruction.id,
                        &instruction.kind,
                    );
                if instruction_touches_ordinary_map_owned_any_v2(&instruction.kind)
                    || is_owner_clone
                    || is_get_any_binding_move
                    || is_values_binding_move
                    || is_values_len
                    || (matches!(&instruction.kind, MirInstructionKind::Drop { .. })
                        && instruction_touches_ordinary_map_owned_any_v2_type(
                            function,
                            &instruction.kind,
                        ))
                {
                    self.validate_ordinary_map_owned_any_v2_instruction(
                        function,
                        &instruction.kind,
                        instruction.id.as_str(),
                    );
                }
                if instruction_touches_ordinary_map_owned_any_v2_type(function, &instruction.kind)
                    && !instruction_touches_ordinary_map_owned_any_v2(&instruction.kind)
                    && !is_owner_clone
                    && !is_get_any_binding_move
                    && !is_values_binding_move
                    && !is_values_len
                    && !is_values_producer_caller_call
                    && !is_map_root_producer_caller_call
                    && !matches!(&instruction.kind, MirInstructionKind::Drop { .. })
                {
                    self.error(format!(
                        "{} ordinary Map/Any owner-v2 value is used outside its dedicated receipt-bound operation",
                        instruction.id
                    ));
                }
            }
        }
    }

    fn validate_ordinary_map_owned_any_v2_instruction(
        &mut self,
        function: &MirFunction,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        match instruction {
            MirInstructionKind::Move { result, source } => {
                let exact_result_transfer = self
                    .program
                    .ordinary_map_owned_any_v2_get_any_result_receipts()
                    .ok()
                    .is_some_and(|receipts| {
                        receipts.iter().any(|receipt| {
                            receipt.owner == function.owner
                                && receipt.result == *source
                                && receipt.binding == *result
                        })
                    });
                let exact_values_transfer = self.values_result_receipts().is_some_and(|receipts| {
                    receipts.iter().any(|receipt| {
                        receipt.owner == function.owner
                            && receipt.result == *source
                            && receipt.binding == *result
                    })
                }) || (self
                    .allow_ordinary_map_owned_any_v2_values_producer_caller
                    && (ordinary_map_owned_any_v2_values_producer_caller_bridge_for_producer(
                        self.program,
                        function,
                    )
                    .is_some_and(|receipt| {
                        (receipt.producer_values_result == *source
                            && receipt.producer_snapshot_value == *result)
                            || (receipt.producer_snapshot_value == *source
                                && receipt.producer_return_value == *result)
                    }) || ordinary_map_owned_any_v2_values_producer_caller_bridge_for_caller(
                        self.program,
                        function,
                    )
                    .is_some_and(|receipt| {
                        receipt.call_result == *source && receipt.caller_binding == *result
                    })))
                    || (self.allow_values_crosscall_string_caller
                        && ordinary_map_owned_any_v2_values_crosscall_string_caller_move_is_receipted(
                            self.program,
                            function,
                            result,
                            source,
                        ))
                    || self.values_result_crosscall_profile.is_some_and(|version| {
                        ordinary_map_owned_any_v2_values_result_crosscall_string_caller_move_is_receipted(
                            self.program,
                            function,
                            result,
                            source,
                            version,
                        )
                    });
                if exact_result_transfer {
                    self.validate_ordinary_map_owned_any_v2_get_result(function, source, subject);
                    self.validate_ordinary_map_owned_any_v2_get_result(function, result, subject);
                } else if exact_values_transfer {
                    self.validate_ordinary_map_owned_any_v2_values_type(
                        function,
                        source,
                        subject,
                        "Values result",
                    );
                    self.validate_ordinary_map_owned_any_v2_values_type(
                        function,
                        result,
                        subject,
                        "Values binding",
                    );
                } else if self.allow_ordinary_map_owned_any_v2_map_root_producer_caller
                    && ordinary_map_owned_any_v2_map_root_producer_caller_move_is_receipted(
                        self.program,
                        function,
                        result,
                        source,
                    )
                {
                    self.validate_ordinary_map_owned_any_v2_type(
                        function,
                        source,
                        subject,
                        "producer/caller Move source",
                    );
                    self.validate_ordinary_map_owned_any_v2_type(
                        function,
                        result,
                        subject,
                        "producer/caller Move result",
                    );
                } else {
                    self.error(format!(
                        "{subject} ordinary Map/Any owner-v2 Move is not the direct receipt-bound GetAny result transfer"
                    ));
                }
            }
            MirInstructionKind::Clone { result, source } => {
                let owner_ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
                let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
                let result_ty = function.values.get(result).map(|value| &value.ty);
                let source_ty = function.values.get(source).map(|value| &value.ty);
                if result_ty == Some(&owner_ty) && source_ty == Some(&owner_ty) {
                    self.validate_ordinary_map_owned_any_v2_type(
                        function,
                        result,
                        subject,
                        "Clone result",
                    );
                    self.validate_ordinary_map_owned_any_v2_type(
                        function,
                        source,
                        subject,
                        "Clone source",
                    );
                } else if result_ty == Some(&values_ty)
                    && source_ty == Some(&values_ty)
                    && ordinary_map_owned_any_v2_values_borrow_clone_is_receipted(
                        self.program,
                        function,
                        result,
                        source,
                        self.allow_ordinary_map_owned_any_v2_values_producer_caller,
                    )
                {
                    self.validate_ordinary_map_owned_any_v2_values_type(
                        function,
                        result,
                        subject,
                        "Values borrow Clone result",
                    );
                    self.validate_ordinary_map_owned_any_v2_values_type(
                        function,
                        source,
                        subject,
                        "Values borrow Clone source",
                    );
                } else {
                    self.error(format!(
                        "{subject} ordinary Map/Any owner-v2 Clone is neither a canonical root clone nor a receipt-bound Values borrow clone"
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2New { result } => {
                if self.allow_computed_string_key_owner_chain
                    || self.has_values_local_index_i32_result_branch_receipt(function)
                {
                    self.validate_ordinary_map_owned_any_v2_root_result(
                        function,
                        result,
                        subject,
                        "fallible New result",
                    );
                } else {
                    self.validate_ordinary_map_owned_any_v2_type(
                        function,
                        result,
                        subject,
                        "New result",
                    );
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
                result,
                source,
                key,
                value,
            } => {
                self.validate_ordinary_map_owned_any_v2_type(
                    function,
                    result,
                    subject,
                    "Set result",
                );
                self.validate_ordinary_map_owned_any_v2_type(
                    function,
                    source,
                    subject,
                    "Set source",
                );
                self.validate_owned_string_value(function, key, subject, "Set key");
                self.validate_ordinary_map_owned_any_v2_set_payload(function, value, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2SetAnyResult {
                result,
                source,
                key,
                value,
            } => {
                if !self.allow_computed_string_key_owner_chain
                    && !self.has_values_local_index_i32_result_branch_receipt(function)
                {
                    self.error(format!(
                        "{subject} ordinary Map/Any SetAnyResult requires its exact CheckedProgram-bound ComputedStringKeyOwnerChain branch receipt"
                    ));
                }
                self.validate_ordinary_map_owned_any_v2_root_result(
                    function,
                    result,
                    subject,
                    "SetAnyResult carrier",
                );
                self.validate_ordinary_map_owned_any_v2_type(
                    function,
                    source,
                    subject,
                    "SetAnyResult source root",
                );
                self.validate_owned_string_value(function, key, subject, "SetAnyResult key");
                self.validate_ordinary_map_owned_any_v2_set_payload(function, value, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. } => {
                let exact_result_caller = self.values_result_crosscall_profile.is_some_and(|version| {
                    ordinary_map_owned_any_v2_values_result_crosscall_string_caller_set_is_receipted(
                        self.program,
                        function,
                        subject,
                        version,
                    )
                });
                if !exact_result_caller {
                    self.error(format!(
                        "{subject} ordinary Map SetResult requires its exact V3/V4 Checker/MIR producer receipt; status/tag semantics remain outside symbolic proof"
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Remove {
                result,
                source,
                key,
            } => {
                self.validate_ordinary_map_owned_any_v2_type(
                    function,
                    result,
                    subject,
                    "Remove result",
                );
                self.validate_ordinary_map_owned_any_v2_type(
                    function,
                    source,
                    subject,
                    "Remove source",
                );
                self.validate_owned_string_value(function, key, subject, "Remove key");
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root } => {
                self.validate_ordinary_map_owned_any_v2_type(function, root, subject, "Size root");
                self.validate_i32_value(
                    function,
                    result,
                    subject,
                    "ordinary Map/Any owner-v2 Size result",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { result, root, key } => {
                self.validate_ordinary_map_owned_any_v2_type(
                    function,
                    root,
                    subject,
                    "Contains root",
                );
                self.validate_bool_value(
                    function,
                    result,
                    subject,
                    "ordinary Map/Any owner-v2 Contains result",
                );
                self.validate_owned_string_value(function, key, subject, "Contains key");
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, root, key } => {
                self.validate_ordinary_map_owned_any_v2_type(
                    function,
                    root,
                    subject,
                    "GetAny root",
                );
                self.validate_owned_string_value(function, key, subject, "GetAny key");
                self.validate_ordinary_map_owned_any_v2_get_result(function, result, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultString {
                result,
                root,
                key,
                default,
                callsite,
                call_target,
                type_argument,
            } => {
                if !self.allow_ordinary_map_owned_any_v2_get_or_default_string {
                    self.error(format!(
                        "{subject} AnyOwner-v2 GetOrDefault<String> requires exact CheckedProgram/MIR route validation"
                    ));
                }
                self.validate_ordinary_map_owned_any_v2_type(
                    function,
                    root,
                    subject,
                    "GetOrDefault<String> borrowed root",
                );
                self.validate_owned_string_value(
                    function,
                    key,
                    subject,
                    "GetOrDefault<String> key",
                );
                self.validate_owned_string_value(
                    function,
                    default,
                    subject,
                    "GetOrDefault<String> caller default",
                );
                self.validate_owned_string_value(
                    function,
                    result,
                    subject,
                    "GetOrDefault<String> result",
                );
                let string_type =
                    crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
                let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
                if type_argument != &string_type {
                    self.error(format!(
                        "{subject} AnyOwner-v2 GetOrDefault<String> uses a noncanonical type argument"
                    ));
                }
                let exact = self
                    .program
                    .ordinary_map_owned_any_v2_get_or_default_string_receipts()
                    .ok()
                    .is_some_and(|receipts| {
                        let matching = receipts
                            .iter()
                            .filter(|receipt| {
                                receipt.owner == function.owner
                                    && receipt.instruction.as_str() == subject
                            })
                            .collect::<Vec<_>>();
                        matches!(
                            matching.as_slice(),
                            [receipt]
                                if receipt.result == *result
                                    && receipt.root == *root
                                    && receipt.key == *key
                                    && receipt.default == *default
                                    && receipt.callsite == *callsite
                                    && receipt.call_target == *call_target
                                    && receipt.type_argument == *type_argument
                                    && receipt.root_type_desc == crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
                                    && receipt.key_type_desc == string_type
                                    && receipt.default_type_desc == string_type
                                    && receipt.result_type_desc == string_type
                                    && receipt.pair_type_desc == crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id()
                                    && receipt.payload_owner_type_desc == crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id()
                                    && receipt.selected_payload_type_desc == string_type
                                    && receipt.miss_payload_type_desc == i32_type
                                    && receipt.helper_default_pack_from == string_type
                                    && !receipt.helper_default_pack.0.trim().is_empty()
                                    && receipt.helper_map_permission == crate::core::Permission::View
                                    && receipt.validate_against_program(self.program).is_ok()
                        )
                    });
                if !exact {
                    self.error(format!(
                        "{subject} AnyOwner-v2 GetOrDefault<String> has no exact String payload/i32-miss/default-pack Checker and TypeDesc bridge"
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultI32 {
                result,
                root,
                key,
                default,
                callsite,
                call_target,
                type_argument,
            } => {
                if !self.allow_ordinary_map_owned_any_v2_get_or_default_i32 {
                    self.error(format!(
                        "{subject} AnyOwner-v2 GetOrDefault<i32> requires exact CheckedProgram/MIR route validation"
                    ));
                }
                self.validate_ordinary_map_owned_any_v2_type(
                    function,
                    root,
                    subject,
                    "GetOrDefault<i32> borrowed root",
                );
                self.validate_owned_string_value(function, key, subject, "GetOrDefault<i32> key");
                self.validate_i32_value(
                    function,
                    default,
                    subject,
                    "GetOrDefault<i32> caller default",
                );
                self.validate_i32_value(function, result, subject, "GetOrDefault<i32> result");
                let expected_type =
                    crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
                if type_argument != &expected_type {
                    self.error(format!(
                        "{subject} AnyOwner-v2 GetOrDefault<i32> uses a noncanonical type argument"
                    ));
                }
                let exact = self
                    .program
                    .ordinary_map_owned_any_v2_get_or_default_i32_receipts()
                    .ok()
                    .is_some_and(|receipts| {
                        let matching = receipts
                            .iter()
                            .filter(|receipt| {
                                receipt.owner == function.owner
                                    && receipt.instruction.as_str() == subject
                            })
                            .collect::<Vec<_>>();
                        matches!(
                            matching.as_slice(),
                            [receipt]
                                if receipt.result == *result
                                    && receipt.root == *root
                                    && receipt.key == *key
                                    && receipt.default == *default
                                    && receipt.callsite == *callsite
                                    && receipt.call_target == *call_target
                                    && receipt.type_argument == *type_argument
                                    && receipt.root_type_desc
                                        == crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
                                    && receipt.key_type_desc
                                        == crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String)
                                    && receipt.default_type_desc == expected_type
                                    && receipt.result_type_desc == expected_type
                                    && receipt.pair_type_desc
                                        == crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id()
                                    && receipt.payload_owner_type_desc
                                        == crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id()
                                    && receipt.selected_payload_type_desc == expected_type
                                    && receipt.helper_map_permission == crate::core::Permission::View
                                    && receipt.validate_against_program(self.program).is_ok()
                        )
                    });
                if !exact {
                    self.error(format!(
                        "{subject} AnyOwner-v2 GetOrDefault<i32> has no exact whole-pair/default-sentinel Checker and TypeDesc bridge"
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack {
                result,
                pair,
                projection,
            } => {
                self.validate_ordinary_map_owned_any_v2_get_result(function, pair, subject);
                let receipts = self
                    .program
                    .ordinary_map_owned_any_v2_unpack_result_receipts();
                let exact = receipts.as_ref().ok().is_some_and(|receipts| {
                    let matching = receipts
                        .iter()
                        .filter(|receipt| {
                            receipt.owner == function.owner
                                && receipt.instruction.as_str() == subject
                        })
                        .collect::<Vec<_>>();
                    matches!(
                        matching.as_slice(),
                        [receipt]
                            if receipt.result == *result
                                && receipt.pair == *pair
                                && receipt.projection == *projection
                                && function.values.get(result).is_some_and(|value| {
                                    value.ty == receipt.result_type
                                        && validate_ordinary_map_owned_any_v2_checked_get_unpack_type(
                                            self.program.type_catalog(),
                                            &value.ty,
                                        )
                                        .is_ok()
                                })
                                && receipt.validate_against_program(self.program).is_ok()
                    )
                });
                if !exact {
                    self.error(format!(
                        "{subject} ordinary Map/Any owner-v2 unpack lacks an exact Checker/GetAny pair receipt or supported result TypeDesc"
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack {
                result,
                values,
                index,
                projection,
            } => {
                self.validate_ordinary_map_owned_any_v2_values_type(
                    function,
                    values,
                    subject,
                    "Values unpack snapshot",
                );
                let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
                let i64_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I64);
                let index_type = function.values.get(index).map(|value| value.ty.clone());
                let index_is_signed = index_type.as_ref().is_some_and(|ty| {
                    (ty == &i32_type || ty == &i64_type)
                        && self
                            .program
                            .type_catalog()
                            .get(ty)
                            .is_some_and(|descriptor| {
                                matches!(
                                    descriptor.abi,
                                    MirAbiClass::Integer {
                                        bits: 32 | 64,
                                        signed: true
                                    }
                                )
                            })
                });
                let receipts = self
                    .program
                    .ordinary_map_owned_any_v2_values_unpack_result_receipts();
                let exact_local = receipts.as_ref().ok().is_some_and(|receipts| {
                    let matching = receipts
                        .iter()
                        .filter(|receipt| {
                            receipt.owner == function.owner
                                && receipt.instruction.as_str() == subject
                        })
                        .collect::<Vec<_>>();
                    matches!(
                        matching.as_slice(),
                        [receipt]
                            if receipt.result == *result
                                && receipt.values == *values
                                && receipt.index == *index
                                && receipt.projection == *projection
                                && index_type.as_ref() == Some(&receipt.index_type)
                                && receipt.conversion_to == receipt.result_type
                                && match receipt.result_owner_settlement {
                                    crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::ExplicitWholeDrop => receipt
                                        .result_owner_whole_drop_unproved_uses
                                        .as_ref()
                                        .is_some_and(|uses| uses.is_empty())
                                        && receipt.result_owner_field_move.is_none(),
                                    crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::ExplicitStringDrop => {
                                        receipt.result_type
                                            == crate::core::ir::primitive_type_id(
                                                crate::core::PrimitiveType::String,
                                            )
                                            && receipt.result_owner_settlement_point.is_some()
                                            && receipt.result_owner_settlement_index.is_some()
                                            && receipt.result_owner_string_drop_source_uses.is_some()
                                            && receipt.result_owner_string_drop_statement_order.is_some()
                                            && receipt.result_owner_string_drop_reads.as_ref().is_some_and(|reads| !reads.is_empty())
                                            && receipt.result_owner_settlement_instruction.is_some()
                                            && receipt.result_owner_whole_drop_unproved_uses.is_none()
                                            && receipt.result_owner_field_move.is_none()
                                    }
                                    crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::DirectStringFieldMove => {
                                        receipt.result_owner_whole_drop_unproved_uses.is_none()
                                            && receipt.result_owner_field_move.as_ref().is_some_and(|field_move| {
                                                field_move.source.field_name == "text"
                                                    && field_move.source.has_exact_source_use_sets()
                                                    && receipt.result_owner_settlement_instruction.as_ref()
                                                        == Some(&field_move.text_settlement_instruction)
                                            })
                                    }
                                    _ => receipt.result_owner_whole_drop_unproved_uses.is_none()
                                        && receipt.result_owner_field_move.is_none(),
                                }
                                && index_is_signed
                                && function.values.get(result).is_some_and(|value| {
                                    value.ty == receipt.result_type
                                        && validate_ordinary_map_owned_any_v2_unpack_type(
                                            self.program.type_catalog(),
                                            &value.ty,
                                        )
                                        .is_ok()
                                })
                                && receipt.validate_against_program(self.program).is_ok()
                    )
                });
                let exact_crosscall = self.allow_values_crosscall_string_caller
                    && ordinary_map_owned_any_v2_values_crosscall_string_caller_unpack_is_receipted(
                        self.program,
                        function,
                        subject,
                        result,
                        values,
                        index,
                        projection,
                    );
                let exact_result_crosscall = self.values_result_crosscall_profile.is_some_and(|version| {
                    ordinary_map_owned_any_v2_values_result_crosscall_string_caller_unpack_is_receipted(
                        self.program,
                        function,
                        subject,
                        result,
                        values,
                        index,
                        version,
                    )
                });
                if exact_crosscall || exact_result_crosscall {
                    self.validate_owned_string_value(
                        function,
                        result,
                        subject,
                        "cross-call Values String unpack result",
                    );
                }
                if !exact_local && !exact_crosscall && !exact_result_crosscall {
                    self.error(format!(
                        "{subject} ordinary Map/Any owner-v2 Values unpack lacks its exact Checker/index/snapshot receipt or supported result TypeDesc"
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { result, root } => {
                self.validate_ordinary_map_owned_any_v2_type(
                    function,
                    root,
                    subject,
                    "IsEmpty root",
                );
                self.validate_bool_value(
                    function,
                    result,
                    subject,
                    "ordinary Map/Any owner-v2 IsEmpty result",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } => {
                self.validate_ordinary_map_owned_any_v2_type(
                    function,
                    root,
                    subject,
                    "Drop operand",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Values { result, root } => {
                self.validate_ordinary_map_owned_any_v2_type(
                    function,
                    root,
                    subject,
                    "Values root",
                );
                self.validate_ordinary_map_owned_any_v2_values_type(
                    function,
                    result,
                    subject,
                    "Values result",
                );
                let has_local_result_receipt =
                    self.values_result_receipts().is_some_and(|receipts| {
                        receipts.iter().any(|receipt| {
                            receipt.owner == function.owner
                                && receipt.result == *result
                                && receipt.root == *root
                        })
                    });
                let has_producer_caller_receipt = self
                    .allow_ordinary_map_owned_any_v2_values_producer_caller
                    && ordinary_map_owned_any_v2_values_producer_caller_bridge_for_producer(
                        self.program,
                        function,
                    )
                    .is_some_and(|receipt| {
                        receipt.producer_values_instruction.as_str() == subject
                            && receipt.producer_values_result == *result
                            && receipt.producer_result_type
                                == crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id()
                    });
                let has_crosscall_string_caller_receipt = self
                    .allow_values_crosscall_string_caller
                    && ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_producer(
                        self.program,
                        function,
                    )
                    .is_some_and(|receipt| {
                        receipt.producer_values_instruction.as_str() == subject
                            && receipt.producer_values_result == *result
                            && receipt.values_type
                                == crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id()
                    });
                let has_result_crosscall_string_caller_receipt =
                    self.values_result_crosscall_profile.is_some_and(|version| {
                        ordinary_map_owned_any_v2_values_result_crosscall_string_caller_bridge_for_owner(
                            self.program,
                            &function.owner,
                            true,
                            version,
                        )
                        .is_some_and(|receipt| {
                            receipt.producer_values_instruction.as_str() == subject
                                && receipt.producer_values_result == *result
                                && receipt.values_type
                                    == crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id()
                        })
                    });
                if !has_local_result_receipt
                    && !has_producer_caller_receipt
                    && !has_crosscall_string_caller_receipt
                    && !has_result_crosscall_string_caller_receipt
                {
                    self.error(format!(
                        "{subject} Values operation has no exact Core result receipt"
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2DynamicArg0 { result, projection } => {
                if !self.allow_ordinary_map_owned_any_v2_dynamic_keys_get {
                    self.error(format!(
                        "{subject} argv[0] projection is admitted only by exact DynamicKeysGet route validation"
                    ));
                    return;
                }
                let receipt = match crate::core::mir::validate_ordinary_map_owned_any_v2_dynamic_keys_get_mir_route(self.program) {
                    Ok(receipt) => receipt,
                    Err(message) => {
                        self.error(format!("{subject} DynamicKeysGet receipt rejected: {message}"));
                        return;
                    }
                };
                let expected_result =
                    match MirValueId::new(format!("expr:{}", receipt.args_projection.0)) {
                        Ok(value) => value,
                        Err(error) => {
                            self.error(format!(
                                "{subject} argv[0] result identity is invalid: {error}"
                            ));
                            return;
                        }
                    };
                if function.owner != receipt.owner
                    || *projection != receipt.args_projection
                    || *result != expected_result
                {
                    self.error(format!(
                        "{subject} argv[0] operands differ from the exact DynamicKeysGet Checker/MIR receipt"
                    ));
                    return;
                }
                self.validate_owned_string_value(
                    function,
                    result,
                    subject,
                    "DynamicKeysGet argv[0] String result",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { result, root } => {
                if !self.allow_ordinary_map_owned_any_v2_keys_snapshot
                    && !self.allow_ordinary_map_owned_any_v2_dynamic_keys_get
                {
                    self.error(format!(
                        "{subject} Keys operation is admitted only by exact KeysSnapshot or DynamicKeysGet route validation"
                    ));
                    return;
                }
                if self.allow_ordinary_map_owned_any_v2_dynamic_keys_get {
                    let receipt = match crate::core::mir::validate_ordinary_map_owned_any_v2_dynamic_keys_get_mir_route(self.program) {
                        Ok(receipt) => receipt,
                        Err(message) => {
                            self.error(format!("{subject} DynamicKeysGet Keys receipt rejected: {message}"));
                            return;
                        }
                    };
                    let expected_result =
                        match MirValueId::new(format!("expr:{}", receipt.keys_action.point.0)) {
                            Ok(value) => value,
                            Err(error) => {
                                self.error(format!(
                                    "{subject} DynamicKeysGet Keys identity is invalid: {error}"
                                ));
                                return;
                            }
                        };
                    if receipt.keys_action.source_local.as_ref() != Some(&receipt.set_action.local)
                    {
                        self.error(format!(
                            "{subject} DynamicKeysGet Keys receipt source root does not match the exact Set root"
                        ));
                        return;
                    }
                    let expected_root =
                        match MirValueId::new(format!("local:{}", receipt.set_action.local.0 .0)) {
                            Ok(value) => value,
                            Err(error) => {
                                self.error(format!(
                                    "{subject} DynamicKeysGet root identity is invalid: {error}"
                                ));
                                return;
                            }
                        };
                    if function.owner != receipt.owner
                        || *result != expected_result
                        || *root != expected_root
                        || function.values.get(result).map(|value| &value.ty)
                            != Some(&receipt.keys_snapshot.snapshot_type)
                    {
                        self.error(format!(
                            "{subject} Keys operands or List<String> TypeDesc differ from the exact DynamicKeysGet receipt"
                        ));
                        return;
                    }
                    self.validate_ordinary_map_owned_any_v2_type(
                        function,
                        root,
                        subject,
                        "DynamicKeysGet Keys root",
                    );
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_owned_string_list(&receipt.keys_snapshot.snapshot_type)
                    {
                        self.error(format!(
                            "{subject} DynamicKeysGet Keys TypeDesc rejected: {message}"
                        ));
                        return;
                    }
                    for operation in [
                        MirGlueOperation::MoveOut,
                        MirGlueOperation::Clone,
                        MirGlueOperation::Drop,
                    ] {
                        if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_list_glue(&receipt.keys_snapshot.snapshot_type, operation)
                        {
                            self.error(format!(
                                "{subject} DynamicKeysGet Keys List glue rejected: {message}"
                            ));
                        }
                        if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_glue(&receipt.keys_snapshot.copied_key_type, operation)
                        {
                            self.error(format!(
                                "{subject} DynamicKeysGet Keys String glue rejected: {message}"
                            ));
                        }
                    }
                    return;
                }
                let receipt = match crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_mir_route(self.program) {
                    Ok(receipt) => receipt,
                    Err(message) => {
                        self.error(format!("{subject} KeysSnapshot receipt rejected: {message}"));
                        return;
                    }
                };
                let expected_result =
                    match MirValueId::new(format!("expr:{}", receipt.keys_operation.0)) {
                        Ok(value) => value,
                        Err(error) => {
                            self.error(format!(
                                "{subject} Keys result identity is invalid: {error}"
                            ));
                            return;
                        }
                    };
                let expected_root =
                    match MirValueId::new(format!("local:{}", receipt.source_local.0 .0)) {
                        Ok(value) => value,
                        Err(error) => {
                            self.error(format!("{subject} Keys root identity is invalid: {error}"));
                            return;
                        }
                    };
                if *result != expected_result || *root != expected_root {
                    self.error(format!(
                        "{subject} Keys operands differ from the exact Checker snapshot receipt"
                    ));
                }
                self.validate_ordinary_map_owned_any_v2_type(function, root, subject, "Keys root");
                let result_ty = function.values.get(result).map(|value| value.ty.clone());
                if result_ty.as_ref() != Some(&receipt.snapshot_type) {
                    self.error(format!(
                        "{subject} Keys result does not use its Checker-receipted List<String> TypeDesc"
                    ));
                    return;
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_owned_string_list(&receipt.snapshot_type)
                {
                    self.error(format!(
                        "{subject} Keys List<String> TypeDesc rejected: {message}"
                    ));
                }
                for operation in [
                    MirGlueOperation::MoveOut,
                    MirGlueOperation::Clone,
                    MirGlueOperation::Drop,
                ] {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_list_glue(&receipt.snapshot_type, operation)
                    {
                        self.error(format!(
                            "{subject} Keys List<String> glue rejected: {message}"
                        ));
                    }
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_glue(&receipt.key_type, operation)
                    {
                        self.error(format!(
                            "{subject} Keys owned String glue rejected: {message}"
                        ));
                    }
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack {
                result,
                keys,
                projection,
            } => {
                if !self.allow_ordinary_map_owned_any_v2_keys_snapshot
                    && !self.allow_ordinary_map_owned_any_v2_dynamic_keys_get
                {
                    self.error(format!(
                        "{subject} Keys unpack is admitted only by exact KeysSnapshot or DynamicKeysGet route validation"
                    ));
                    return;
                }
                if self.allow_ordinary_map_owned_any_v2_dynamic_keys_get {
                    let receipt = match crate::core::mir::validate_ordinary_map_owned_any_v2_dynamic_keys_get_mir_route(self.program) {
                        Ok(receipt) => receipt,
                        Err(message) => {
                            self.error(format!("{subject} DynamicKeysGet KeysUnpack receipt rejected: {message}"));
                            return;
                        }
                    };
                    let keys_receipt = &receipt.keys_snapshot;
                    let expected_keys = match MirValueId::new(format!(
                        "local:{}",
                        keys_receipt.snapshot_local.0 .0
                    )) {
                        Ok(value) => value,
                        Err(error) => {
                            self.error(format!(
                                "{subject} DynamicKeysGet snapshot identity is invalid: {error}"
                            ));
                            return;
                        }
                    };
                    let expected_result = match MirValueId::new(format!(
                        "expr:{}",
                        keys_receipt.copied_key_projection.0
                    )) {
                        Ok(value) => value,
                        Err(error) => {
                            self.error(format!("{subject} DynamicKeysGet String result identity is invalid: {error}"));
                            return;
                        }
                    };
                    if function.owner != receipt.owner
                        || *keys != expected_keys
                        || *result != expected_result
                        || *projection != keys_receipt.copied_key_projection
                        || function.values.get(keys).map(|value| &value.ty)
                            != Some(&keys_receipt.snapshot_type)
                        || function.values.get(result).map(|value| &value.ty)
                            != Some(&keys_receipt.copied_key_type)
                    {
                        self.error(format!(
                            "{subject} KeysUnpack operands or String/List<String> TypeDesc differ from the exact DynamicKeysGet receipt"
                        ));
                        return;
                    }
                    self.validate_owned_string_value(
                        function,
                        result,
                        subject,
                        "DynamicKeysGet copied String result",
                    );
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_owned_string_list(&keys_receipt.snapshot_type)
                    {
                        self.error(format!(
                            "{subject} DynamicKeysGet KeysUnpack List TypeDesc rejected: {message}"
                        ));
                    }
                    return;
                }
                let receipt = match crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_mir_route(self.program) {
                    Ok(receipt) => receipt,
                    Err(message) => {
                        self.error(format!("{subject} KeysSnapshot receipt rejected: {message}"));
                        return;
                    }
                };
                let expected_keys =
                    match MirValueId::new(format!("local:{}", receipt.snapshot_local.0 .0)) {
                        Ok(value) => value,
                        Err(error) => {
                            self.error(format!(
                                "{subject} Keys snapshot identity is invalid: {error}"
                            ));
                            return;
                        }
                    };
                let expected_result =
                    match MirValueId::new(format!("expr:{}", receipt.index_projection.0)) {
                        Ok(value) => value,
                        Err(error) => {
                            self.error(format!(
                                "{subject} Keys result identity is invalid: {error}"
                            ));
                            return;
                        }
                    };
                if *result != expected_result
                    || *keys != expected_keys
                    || *projection != receipt.index_projection
                {
                    self.error(format!(
                        "{subject} Keys unpack operands differ from the exact index-zero Checker receipt"
                    ));
                }
                let result_ty = function.values.get(result).map(|value| value.ty.clone());
                let keys_ty = function.values.get(keys).map(|value| value.ty.clone());
                if result_ty.as_ref() != Some(&receipt.key_type)
                    || keys_ty.as_ref() != Some(&receipt.snapshot_type)
                {
                    self.error(format!(
                        "{subject} Keys unpack values do not use the Checker-receipted String/List<String> TypeDescs"
                    ));
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_owned_string(&receipt.key_type)
                {
                    self.error(format!(
                        "{subject} Keys unpack String TypeDesc rejected: {message}"
                    ));
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_owned_string_list(&receipt.snapshot_type)
                {
                    self.error(format!(
                        "{subject} Keys source List<String> TypeDesc rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::ListOp {
                result,
                operation,
                list,
                argument,
                list_operation_contract,
            } if function.values.get(list).is_some_and(|value| {
                value.ty == crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id()
            }) =>
            {
                let receipt_bound = *operation == crate::core::mir::MirListOperation::Len
                    && argument.is_none()
                    && ordinary_map_owned_any_v2_values_list_is_receipted(
                        self.program,
                        function,
                        list,
                        self.allow_ordinary_map_owned_any_v2_values_producer_caller,
                    )
                    && (ordinary_map_owned_any_v2_values_local_list_is_receipted(
                        self.program,
                        function,
                        list,
                    ) || (self.allow_ordinary_map_owned_any_v2_values_producer_caller
                        && ordinary_map_owned_any_v2_values_producer_caller_len_is_receipted(
                            self.program,
                            function,
                            subject,
                            result,
                            list,
                            argument,
                        )));
                if !receipt_bound {
                    self.error(format!(
                        "{subject} Values List operation is outside the exact receipt-bound Len surface"
                    ));
                    return;
                }
                self.validate_ordinary_map_owned_any_v2_values_type(
                    function,
                    list,
                    subject,
                    "List receiver",
                );
                self.validate_i32_value(function, result, subject, "Values Len result");
                let result_ty = function.values.get(result).map(|value| value.ty.clone());
                let list_ty = function.values.get(list).map(|value| value.ty.clone());
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| function.values.get(value))
                    .map(|value| value.ty.clone());
                if let (Some(result_ty), Some(list_ty), Some(contract)) =
                    (result_ty, list_ty, list_operation_contract.as_ref())
                {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_list_operation_receipt_with_argument(
                            &result_ty,
                            &list_ty,
                            argument_ty.as_ref(),
                            *operation,
                            contract,
                        )
                    {
                        self.error(format!("{subject} Values Len contract rejected: {message}"));
                    }
                } else {
                    self.error(format!(
                        "{subject} Values Len has incomplete TypeDesc evidence"
                    ));
                }
            }
            MirInstructionKind::Drop { value } => {
                if function.values.get(value).is_some_and(|value| {
                    value.ty == crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id()
                }) {
                    let exact_settlement = ordinary_map_owned_any_v2_values_local_list_is_receipted(
                        self.program,
                        function,
                        value,
                    ) || (self
                        .allow_ordinary_map_owned_any_v2_values_producer_caller
                        && ordinary_map_owned_any_v2_values_list_is_receipted(
                            self.program,
                            function,
                            value,
                            true,
                        )
                        && ordinary_map_owned_any_v2_values_producer_caller_drop_is_receipted(
                            self.program,
                            function,
                            subject,
                            value,
                        ))
                        || (self.allow_values_crosscall_string_caller
                            && ordinary_map_owned_any_v2_values_crosscall_string_caller_snapshot_drop_is_receipted(
                                self.program,
                                function,
                                subject,
                                value,
                            ))
                        || self.values_result_crosscall_profile.is_some_and(|version| {
                            ordinary_map_owned_any_v2_values_result_crosscall_string_caller_snapshot_drop_is_receipted(
                                self.program,
                                function,
                                subject,
                                value,
                                version,
                            )
                        });
                    if !exact_settlement {
                        self.error(format!(
                            "{subject} Values Drop has no exact Core result settlement receipt"
                        ));
                    }
                    self.validate_ordinary_map_owned_any_v2_values_type(
                        function,
                        value,
                        subject,
                        "Values Drop operand",
                    );
                } else if self.allow_ordinary_map_owned_any_v2_map_root_producer_caller
                    && map_root_get_any_caller_string_drop_is_receipted(
                        self.program,
                        function,
                        subject,
                        value,
                    )
                {
                    self.validate_owned_string_value(
                        function,
                        value,
                        subject,
                        "GetAny-caller String Drop operand",
                    );
                } else if self.allow_values_crosscall_string_caller
                    && ordinary_map_owned_any_v2_values_crosscall_string_caller_string_drop_is_receipted(
                        self.program,
                        function,
                        subject,
                        value,
                    )
                {
                    self.validate_owned_string_value(
                        function,
                        value,
                        subject,
                        "cross-call Values String Drop operand",
                    );
                } else if self.values_result_crosscall_profile.is_some_and(|version| {
                    ordinary_map_owned_any_v2_values_result_crosscall_string_caller_string_drop_is_receipted(
                        self.program,
                        function,
                        subject,
                        value,
                        version,
                    )
                }) {
                    self.validate_owned_string_value(
                        function,
                        value,
                        subject,
                        "Result-branch cross-call Values String Drop operand",
                    );
                } else {
                    self.validate_ordinary_map_owned_any_v2_get_result(function, value, subject);
                }
            }
            _ => self.error(format!(
                "{subject} is not a dedicated ordinary Map/Any owner-v2 operation"
            )),
        }
    }

    fn validate_ordinary_map_owned_any_v2_type(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        if function.values.get(value).is_none_or(|value| {
            value.ty != crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
        }) {
            self.error(format!(
                "{subject} ordinary Map/Any owner-v2 {role} does not use its canonical TypeDesc"
            ));
        }
    }

    fn validate_ordinary_map_owned_any_v2_root_result(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let result_ty = crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id();
        if function
            .values
            .get(value)
            .is_none_or(|value| value.ty != result_ty)
        {
            self.error(format!(
                "{subject} ordinary Map/Any owner-v2 {role} is not the canonical Result<owner-v2 root, i32> carrier"
            ));
            return;
        }
        match crate::core::mir::types::ordinary_map_owned_any_v2_source_result_type_id() {
            Ok(source_result_ty) => {
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_ordinary_map_owned_any_v2_root_result(
                        &result_ty,
                        &source_result_ty,
                    )
                {
                    self.error(format!(
                        "{subject} ordinary Map/Any owner-v2 {role} Result TypeDesc is invalid: {message}"
                    ));
                }
            }
            Err(message) => self.error(format!(
                "{subject} ordinary Map/Any owner-v2 {role} source Result identity is invalid: {message}"
            )),
        }
    }

    fn validate_ordinary_map_owned_any_v2_get_result(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let result_ty = crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
        if function
            .values
            .get(value)
            .is_none_or(|value| value.ty != result_ty)
        {
            self.error(format!(
                "{subject} ordinary Map/Any owner-v2 GetAny result does not use the canonical (bool, DynamicAny) TypeDesc"
            ));
            return;
        }
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_glue(&result_ty, operation)
            {
                self.error(format!(
                    "{subject} ordinary Map/Any owner-v2 GetAny result TypeDesc rejected: {message}"
                ));
            }
        }
        let payload_ty = crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id();
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_glue(&payload_ty, operation)
            {
                self.error(format!(
                    "{subject} ordinary Map/Any owner-v2 DynamicAny owner TypeDesc rejected: {message}"
                ));
            }
        }
        let sentinel_i32 = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        if let Err(message) = self
            .program
            .type_catalog()
            .validate_copy_scalar(&sentinel_i32)
        {
            self.error(format!(
                "{subject} ordinary Map/Any owner-v2 GetAny miss sentinel i32 TypeDesc rejected: {message}"
            ));
        }
    }

    fn validate_ordinary_map_owned_any_v2_values_type(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        if function
            .values
            .get(value)
            .is_none_or(|value| value.ty != values_ty)
        {
            self.error(format!(
                "{subject} ordinary Map/Any owner-v2 {role} does not use its canonical Values List TypeDesc"
            ));
            return;
        }
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_list_glue(&values_ty, operation)
            {
                self.error(format!(
                    "{subject} ordinary Map/Any owner-v2 Values List TypeDesc rejected: {message}"
                ));
            }
        }
    }

    fn validate_ordinary_map_owned_any_v2_set_payload(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let Some(value) = function.values.get(value) else {
            self.error(format!(
                "{subject} ordinary Map/Any owner-v2 Set payload value is absent"
            ));
            return;
        };
        if let Err(message) = validate_ordinary_map_owned_any_v2_selected_payload_type(
            self.program.type_catalog(),
            &value.ty,
        ) {
            self.error(format!(
                "{subject} ordinary Map/Any owner-v2 Set payload TypeDesc is unsupported: {message}"
            ));
        }
    }

    fn validate_ordinary_map_owned_get_instruction(
        &mut self,
        function: &MirFunction,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        match instruction {
            MirInstructionKind::Clone { result, source } => {
                self.validate_ordinary_map_owned_get_type(
                    function,
                    result,
                    subject,
                    "Clone result",
                );
                self.validate_ordinary_map_owned_get_type(
                    function,
                    source,
                    subject,
                    "Clone source",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { result } => {
                self.validate_ordinary_map_owned_get_type(function, result, subject, "New result");
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set {
                result,
                source,
                key,
                value,
            } => {
                self.validate_ordinary_map_owned_get_type(function, result, subject, "Set result");
                self.validate_ordinary_map_owned_get_type(function, source, subject, "Set source");
                self.validate_owned_get_string_value(function, key, subject, "Set key");
                self.validate_map_set_payload(
                    function,
                    value,
                    subject,
                    "ordinary Map owned-get v1",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove {
                result,
                source,
                key,
            } => {
                self.validate_ordinary_map_owned_get_type(
                    function,
                    result,
                    subject,
                    "Remove result",
                );
                self.validate_ordinary_map_owned_get_type(
                    function,
                    source,
                    subject,
                    "Remove source",
                );
                self.validate_owned_get_string_value(function, key, subject, "Remove key");
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { result, root } => {
                self.validate_ordinary_map_owned_get_type(function, root, subject, "Size root");
                self.validate_i32_value(
                    function,
                    result,
                    subject,
                    "ordinary Map owned-get v1 Size result",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { result, root, key } => {
                self.validate_ordinary_map_owned_get_type(function, root, subject, "Contains root");
                self.validate_bool_value(
                    function,
                    result,
                    subject,
                    "ordinary Map owned-get v1 Contains result",
                );
                self.validate_owned_get_string_value(function, key, subject, "Contains key");
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { result, root, key } => {
                self.validate_ordinary_map_owned_get_type(function, root, subject, "GetI32 root");
                self.validate_owned_get_string_value(function, key, subject, "GetI32 key");
                self.validate_owned_get_result(function, result, subject, false);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { result, root, key } => {
                self.validate_ordinary_map_owned_get_type(
                    function,
                    root,
                    subject,
                    "GetString root",
                );
                self.validate_owned_get_string_value(function, key, subject, "GetString key");
                self.validate_owned_get_result(function, result, subject, true);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { result, root, key } => {
                self.validate_ordinary_map_owned_get_type(function, root, subject, "GetAny root");
                self.validate_owned_get_string_value(function, key, subject, "GetAny key");
                let validation = value_type(function, result)
                    .ok_or_else(|| "GetAny result value is absent".to_string())
                    .and_then(|ty| {
                        self.program
                            .type_catalog()
                            .validate_ordinary_map_owned_get_any_result_type(&ty)
                    });
                if let Err(message) = validation {
                    self.error(format!(
                        "{subject} ordinary Map GetAny result rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { result, root } => {
                self.validate_ordinary_map_owned_get_type(function, root, subject, "Values root");
                self.validate_owned_get_values_result(function, result, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { root } => {
                self.validate_ordinary_map_owned_get_type(function, root, subject, "Drop operand");
            }
            _ => self.error(format!(
                "{subject} is not a dedicated ordinary Map owned-get v1 operation"
            )),
        }
    }

    fn validate_ordinary_map_owned_get_type(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        if function.values.get(value).is_none_or(|value| {
            value.ty != crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id()
        }) {
            self.error(format!(
                "{subject} ordinary Map owned-get v1 {role} does not use its distinct canonical TypeDesc"
            ));
        }
    }

    fn validate_owned_get_string_value(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let valid = function
            .values
            .get(value)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String)
                    && self
                        .program
                        .type_catalog()
                        .validate_owned_string(&descriptor.id)
                        .is_ok()
            });
        if !valid {
            self.error(format!(
                "{subject} ordinary Map owned-get v1 {role} is not the canonical owned String TypeDesc"
            ));
        }
    }

    fn validate_owned_get_result(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        subject: &str,
        string_payload: bool,
    ) {
        let Some(ty) = value_type(function, result) else {
            self.error(format!("{subject} ordinary Map owned-get result is absent"));
            return;
        };
        let validation = if string_payload {
            self.program
                .type_catalog()
                .validate_result_string_i32_variant(&ty)
        } else {
            self.program
                .type_catalog()
                .validate_copy_result_i32_variant(&ty)
        };
        if let Err(message) = validation {
            let shape = if string_payload {
                "Result<string, i32>"
            } else {
                "Result<i32, i32>"
            };
            self.error(format!(
                "{subject} ordinary Map owned-get result must use canonical {shape}: {message}"
            ));
        }
    }

    fn validate_owned_get_values_result(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        subject: &str,
    ) {
        let Some(ty) = value_type(function, result) else {
            self.error(format!("{subject} ordinary Map Values result is absent"));
            return;
        };
        if !is_canonical_managed_any_list_type(self.program.type_catalog(), &ty) {
            self.error(format!(
                "{subject} ordinary Map Values result must use canonical owned List<ManagedAny> TypeDesc"
            ));
        }
    }

    fn validate_ordinary_map_owner_function(&mut self, function: &MirFunction) {
        let owner_ty =
            crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
        if function.result == owner_ty {
            self.error(format!(
                "function '{}' cannot return an ordinary Map owner lifecycle v1 root",
                function.owner.0
            ));
        }
        for parameter in &function.parameters {
            if function
                .values
                .get(parameter)
                .is_some_and(|value| value.ty == owner_ty)
            {
                self.error(format!(
                    "function '{}' cannot receive an ordinary Map owner lifecycle v1 root as a parameter",
                    function.owner.0
                ));
            }
        }
        for block in function.blocks.values() {
            for parameter in &block.parameters {
                if function
                    .values
                    .get(&parameter.value)
                    .is_some_and(|value| value.ty == owner_ty)
                {
                    self.error(format!(
                        "function '{}' cannot receive an ordinary Map owner lifecycle v1 root as a block parameter",
                        function.owner.0
                    ));
                }
            }
            for instruction in &block.instructions {
                let is_owner_clone = matches!(&instruction.kind, MirInstructionKind::Clone { .. })
                    && instruction_touches_ordinary_type(function, &instruction.kind);
                if instruction_touches_ordinary_map_owner(&instruction.kind) || is_owner_clone {
                    self.validate_ordinary_map_owner_instruction(
                        function,
                        &instruction.kind,
                        instruction.id.as_str(),
                    );
                }
                if instruction_touches_ordinary_type(function, &instruction.kind)
                    && !instruction_touches_ordinary_map_owner(&instruction.kind)
                    && !is_owner_clone
                {
                    self.error(format!(
                        "{} ordinary Map owner lifecycle v1 value is used outside its dedicated receipt-bound operation",
                        instruction.id
                    ));
                }
            }
        }
    }

    fn validate_ordinary_map_owner_instruction(
        &mut self,
        function: &MirFunction,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        match instruction {
            MirInstructionKind::Clone { result, source } => {
                self.validate_ordinary_map_owner_type(function, result, subject, "Clone result");
                self.validate_ordinary_map_owner_type(function, source, subject, "Clone source");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleNew { result } => {
                self.validate_ordinary_map_owner_type(function, result, subject, "New result");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleSet {
                result,
                source,
                key,
                value,
            } => {
                self.validate_ordinary_map_owner_type(function, result, subject, "Set result");
                self.validate_ordinary_map_owner_type(function, source, subject, "Set source");
                self.validate_owned_string_value(function, key, subject, "Set key");
                self.validate_map_set_payload(
                    function,
                    value,
                    subject,
                    "ordinary Map owner lifecycle v1",
                );
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleRemove {
                result,
                source,
                key,
            } => {
                self.validate_ordinary_map_owner_type(function, result, subject, "Remove result");
                self.validate_ordinary_map_owner_type(function, source, subject, "Remove source");
                self.validate_owned_string_value(function, key, subject, "Remove key");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleSize { result, root } => {
                self.validate_ordinary_map_owner_type(function, root, subject, "Size root");
                self.validate_i32_value(
                    function,
                    result,
                    subject,
                    "ordinary Map owner lifecycle v1 Size result",
                );
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleContains { result, root, key } => {
                self.validate_ordinary_map_owner_type(function, root, subject, "Contains root");
                self.validate_bool_value(
                    function,
                    result,
                    subject,
                    "ordinary Map owner lifecycle v1 Contains result",
                );
                self.validate_owned_string_value(function, key, subject, "Contains key");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { root } => {
                self.validate_ordinary_map_owner_type(function, root, subject, "Drop operand");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson {
                result, root, ..
            } => {
                self.validate_ordinary_map_owner_type(function, root, subject, "to_json root");
                self.validate_owned_string_value(function, result, subject, "to_json result");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln {
                result, root, ..
            } => {
                self.validate_ordinary_map_owner_type(function, root, subject, "println root");
                let is_unit = function
                    .values
                    .get(result)
                    .and_then(|value| self.program.type_catalog().get(&value.ty))
                    .is_some_and(|descriptor| descriptor.is_canonical_ffi_unit());
                if !is_unit {
                    self.error(format!(
                        "{subject} ordinary Map observer println result is not canonical Unit"
                    ));
                }
            }
            _ => self.error(format!(
                "{subject} is not a dedicated ordinary Map owner lifecycle v1 operation"
            )),
        }
    }

    fn validate_ordinary_map_owner_type(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        role: &str,
    ) {
        if function.values.get(value).is_none_or(|value| {
            value.ty
                != crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id()
        }) {
            self.error(format!(
                "{subject} ordinary Map owner lifecycle v1 {role} does not use its canonical TypeDesc"
            ));
        }
    }

    fn validate_owned_string_value(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        role: &str,
    ) {
        let valid = function
            .values
            .get(value)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String)
                    && self
                        .program
                        .type_catalog()
                        .validate_owned_string(&descriptor.id)
                        .is_ok()
            });
        if !valid {
            self.error(format!(
                "{subject} ordinary Map owner lifecycle v1 {role} is not the canonical owned String TypeDesc"
            ));
        }
    }

    fn validate_map_any_owner_function(&mut self, function: &MirFunction) {
        let owner_ty = crate::core::mir::types::map_any_owner_v1_type_id();
        if function.result == owner_ty {
            self.error(format!(
                "function '{}' cannot return a MapAnyOwner v1 root",
                function.owner.0
            ));
        }
        for parameter in &function.parameters {
            if function
                .values
                .get(parameter)
                .is_some_and(|value| value.ty == owner_ty)
            {
                self.error(format!(
                    "function '{}' cannot receive a MapAnyOwner v1 root as a parameter",
                    function.owner.0
                ));
            }
        }
        for block in function.blocks.values() {
            for parameter in &block.parameters {
                if function
                    .values
                    .get(&parameter.value)
                    .is_some_and(|value| value.ty == owner_ty)
                {
                    self.error(format!(
                        "function '{}' cannot receive a MapAnyOwner v1 root as a block parameter",
                        function.owner.0
                    ));
                }
            }
            for instruction in &block.instructions {
                if instruction_touches_map_any_owner(function, &instruction.kind) {
                    self.validate_map_any_owner_instruction(
                        function,
                        &instruction.kind,
                        instruction.id.as_str(),
                    );
                }
            }
        }
    }

    fn validate_map_any_owner_instruction(
        &mut self,
        function: &MirFunction,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        match instruction {
            MirInstructionKind::MapRootNew { result } => {
                self.validate_map_any_owner_type(function, result, subject, "New result");
            }
            MirInstructionKind::MapRootSet {
                result,
                source,
                key,
                value,
            } => {
                self.validate_map_any_owner_type(function, result, subject, "Set result");
                self.validate_map_any_owner_type(function, source, subject, "Set source");
                if key.contains('\0') {
                    self.error(format!("{subject} MapAnyOwner v1 key contains NUL"));
                }
                self.validate_map_any_owner_set_payload(function, value, subject);
            }
            MirInstructionKind::MapRootRemove {
                result,
                source,
                key,
            } => {
                self.validate_map_any_owner_type(function, result, subject, "Remove result");
                self.validate_map_any_owner_type(function, source, subject, "Remove source");
                if key.contains('\0') {
                    self.error(format!("{subject} MapAnyOwner v1 key contains NUL"));
                }
            }
            MirInstructionKind::MapRootSize { result, root } => {
                self.validate_map_any_owner_type(function, root, subject, "Size root");
                self.validate_i32_value(function, result, subject, "MapAnyOwner v1 Size result");
            }
            MirInstructionKind::MapRootContains { result, root, key } => {
                self.validate_map_any_owner_type(function, root, subject, "Contains root");
                self.validate_bool_value(
                    function,
                    result,
                    subject,
                    "MapAnyOwner v1 Contains result",
                );
                if key.contains('\0') {
                    self.error(format!("{subject} MapAnyOwner v1 key contains NUL"));
                }
            }
            MirInstructionKind::MapRootDrop { root } => {
                self.validate_map_any_owner_type(function, root, subject, "Drop operand");
            }
            _ => self.error(format!(
                "{subject} is not a dedicated MapAnyOwner v1 lifecycle operation"
            )),
        }
    }

    fn validate_map_any_owner_type(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        role: &str,
    ) {
        if function
            .values
            .get(value)
            .is_none_or(|value| value.ty != crate::core::mir::types::map_any_owner_v1_type_id())
        {
            self.error(format!(
                "{subject} MapAnyOwner v1 {role} does not use the canonical v1 TypeDesc"
            ));
        }
    }

    fn validate_i32_value(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        role: &str,
    ) {
        let valid = function
            .values
            .get(value)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
                    && self
                        .program
                        .type_catalog()
                        .validate_copy_scalar(&descriptor.id)
                        .is_ok()
            });
        if !valid {
            self.error(format!(
                "{subject} {role} is not the canonical i32 TypeDesc"
            ));
        }
    }

    fn validate_bool_value(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        role: &str,
    ) {
        let valid = function
            .values
            .get(value)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::Bool)
                    && self
                        .program
                        .type_catalog()
                        .validate_copy_scalar(&descriptor.id)
                        .is_ok()
            });
        if !valid {
            self.error(format!(
                "{subject} {role} is not the canonical bool TypeDesc"
            ));
        }
    }

    fn validate_map_root_set_payload(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
    ) {
        self.validate_map_set_payload(function, value, subject, "MapRoot");
    }

    fn validate_map_any_owner_set_payload(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
    ) {
        self.validate_map_set_payload(function, value, subject, "MapAnyOwner v1");
    }

    fn validate_map_set_payload(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        profile: &str,
    ) {
        let valid = function
            .values
            .get(value)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
            .is_some_and(|descriptor| match descriptor.kind {
                MirTypeKind::Primitive(crate::core::PrimitiveType::I32) => self
                    .program
                    .type_catalog()
                    .validate_copy_scalar(&descriptor.id)
                    .is_ok(),
                MirTypeKind::Primitive(crate::core::PrimitiveType::String) => self
                    .program
                    .type_catalog()
                    .validate_owned_string(&descriptor.id)
                    .is_ok(),
                _ => false,
            });
        if !valid {
            self.error(format!(
                "{subject} {profile} Set payload is outside the receipted i32/String TypeDesc profile"
            ));
        }
    }

    fn validate_type(&mut self, ty: &crate::core::ResolvedTypeId, subject: &str) {
        if !self.checked_types.insert(ty.clone()) {
            return;
        }
        let result = self.validate_type_contract(ty);
        if let Err(message) = result {
            self.error(format!(
                "{subject} type '{}' rejected: {message}",
                ty.as_str()
            ));
        }
    }

    fn validate_type_contract(&mut self, ty: &crate::core::ResolvedTypeId) -> Result<(), String> {
        let catalog = self.program.type_catalog();
        let descriptor = catalog
            .get(ty)
            .ok_or_else(|| format!("TypeDesc '{}' is absent", ty.as_str()))?
            .clone();
        match &descriptor.layout {
            MirLayout::Unit => {
                if descriptor.is_canonical_ffi_unit() {
                    Ok(())
                } else {
                    Err("Unit TypeDesc has an inconsistent ABI/ownership/glue contract".into())
                }
            }
            MirLayout::Scalar => {
                if catalog.validate_copy_scalar(ty).is_ok()
                    || catalog.validate_copy_float_scalar(ty).is_ok()
                    || catalog.validate_owned_string(ty).is_ok()
                {
                    Ok(())
                } else {
                    Err("scalar TypeDesc is outside the verifier scalar contract".into())
                }
            }
            MirLayout::Handle => {
                if catalog.validate_session_channel(ty).is_ok()
                    || catalog.validate_owned_string(ty).is_ok()
                {
                    Ok(())
                } else {
                    Err("handle TypeDesc is not the canonical owned String contract".into())
                }
            }
            MirLayout::Pointer { .. } => {
                if catalog.validate_reference_type(ty).is_ok() {
                    Ok(())
                } else {
                    Err(
                        "pointer TypeDesc is outside the immutable scalar reference contract"
                            .into(),
                    )
                }
            }
            MirLayout::MapRoot => {
                catalog.validate_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)?;
                catalog.validate_glue(ty, crate::core::mir::types::MirGlueOperation::Drop)?;
                if catalog
                    .validate_glue(ty, crate::core::mir::types::MirGlueOperation::Clone)
                    .is_ok()
                {
                    return Err("MapRoot TypeDesc unexpectedly exposes generic Clone glue".into());
                }
                Ok(())
            }
            MirLayout::MapAnyOwnerV1 => {
                catalog.validate_glue(ty, MirGlueOperation::MoveOut)?;
                catalog.validate_glue(ty, MirGlueOperation::Drop)?;
                if catalog.validate_glue(ty, MirGlueOperation::Clone).is_ok() {
                    return Err(
                        "MapAnyOwner v1 TypeDesc unexpectedly exposes generic Clone glue".into(),
                    );
                }
                Ok(())
            }
            MirLayout::OrdinaryMapOwnerLifecycleI32StringV1 => {
                catalog.validate_glue(ty, MirGlueOperation::MoveOut)?;
                catalog.validate_glue(ty, MirGlueOperation::Clone)?;
                catalog.validate_glue(ty, MirGlueOperation::Drop)?;
                Ok(())
            }
            MirLayout::OrdinaryMapOwnedGetI32StringV1 => {
                catalog.validate_glue(ty, MirGlueOperation::MoveOut)?;
                catalog.validate_glue(ty, MirGlueOperation::Clone)?;
                catalog.validate_glue(ty, MirGlueOperation::Drop)?;
                Ok(())
            }
            MirLayout::OrdinaryMapOwnedAnyV2 => {
                if *ty != crate::core::mir::types::ordinary_map_owned_any_v2_type_id() {
                    return Err(
                        "ordinary Map/Any owner-v2 TypeDesc identity is not canonical".into(),
                    );
                }
                if descriptor.kind != MirTypeKind::OrdinaryMapOwnedAnyV2
                    || descriptor.abi != MirAbiClass::OpaqueHandle
                    || descriptor.ownership != MirOwnership::Move
                    || !descriptor.needs_drop_glue
                    || !descriptor.needs_clone_glue
                    || descriptor.session_protocol.is_some()
                    || descriptor.drop_plan.is_some()
                    || descriptor.variant_drop_plan.is_some()
                {
                    return Err(
                        "ordinary Map/Any owner-v2 TypeDesc has an inconsistent ABI/ownership contract".into(),
                    );
                }
                for operation in [
                    MirGlueOperation::MoveOut,
                    MirGlueOperation::Clone,
                    MirGlueOperation::Drop,
                ] {
                    catalog.validate_glue(ty, operation)?;
                }
                Ok(())
            }
            MirLayout::OrdinaryMapOwnedAnyV2Payload => {
                if *ty != crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id()
                    || descriptor.kind != MirTypeKind::OrdinaryMapOwnedAnyV2Payload
                    || descriptor.abi != MirAbiClass::OpaqueHandle
                    || descriptor.ownership != MirOwnership::Move
                    || !descriptor.needs_drop_glue
                    || !descriptor.needs_clone_glue
                    || descriptor.session_protocol.is_some()
                    || descriptor.drop_plan.is_some()
                    || descriptor.variant_drop_plan.is_some()
                {
                    return Err(
                        "ordinary Map/Any owner-v2 payload TypeDesc identity/ABI is not canonical"
                            .into(),
                    );
                }
                for operation in [
                    MirGlueOperation::MoveOut,
                    MirGlueOperation::Clone,
                    MirGlueOperation::Drop,
                ] {
                    catalog.validate_glue(ty, operation)?;
                }
                Ok(())
            }
            MirLayout::OrdinaryMapOwnedAnyV2ValuesElement => {
                if *ty
                    != crate::core::mir::types::ordinary_map_owned_any_v2_values_element_type_id()
                {
                    return Err(
                        "ordinary Map/Any owner-v2 Values element TypeDesc identity is not canonical"
                            .into(),
                    );
                }
                for operation in [
                    MirGlueOperation::MoveOut,
                    MirGlueOperation::Clone,
                    MirGlueOperation::Drop,
                ] {
                    catalog.validate_glue(ty, operation)?;
                }
                Ok(())
            }
            MirLayout::ManagedAnyI32StringV1 => {
                if !catalog.is_managed_any_i32_string_v1(ty) {
                    return Err(
                        "ManagedAny TypeDesc identity does not match its canonical owner profile"
                            .into(),
                    );
                }
                for operation in [
                    MirGlueOperation::MoveOut,
                    MirGlueOperation::Clone,
                    MirGlueOperation::Drop,
                ] {
                    catalog.validate_glue(ty, operation)?;
                }
                Ok(())
            }
            MirLayout::List { element } => {
                if self
                    .ordinary_map_owned_any_v2_checker_values_source_types
                    .contains(ty)
                {
                    return self.validate_ordinary_map_owned_any_v2_checker_values_source_type(
                        ty, element,
                    );
                }
                catalog
                    .validate_list_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)?;
                self.validate_type(element, "List element");
                Ok(())
            }
            MirLayout::Set { element } => {
                catalog
                    .validate_set_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)?;
                self.validate_type(element, "Set element");
                Ok(())
            }
            MirLayout::Tuple(elements)
                if *ty
                    == crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id() =>
            {
                if descriptor.kind != (MirTypeKind::Tuple { arity: 2 })
                    || elements
                        != &vec![
                            crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool),
                            crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id(),
                        ]
                {
                    return Err(
                        "ordinary Map/Any owner-v2 GetAny result TypeDesc is not exact (bool, DynamicAny)"
                            .into(),
                    );
                }
                for operation in [
                    MirGlueOperation::MoveOut,
                    MirGlueOperation::Clone,
                    MirGlueOperation::Drop,
                ] {
                    catalog.validate_glue(ty, operation)?;
                }
                catalog.validate_recursive_tuple_abi(ty)?;
                for element in elements {
                    self.validate_type(element, "ordinary Map/Any owner-v2 GetAny tuple field");
                }
                Ok(())
            }
            MirLayout::Tuple(elements) => {
                catalog.validate_recursive_tuple_abi(ty)?;
                for element in elements {
                    self.validate_type(element, "tuple element");
                }
                Ok(())
            }
            MirLayout::Record { fields, .. } => {
                match descriptor.ownership {
                    MirOwnership::Linear => self.require_linear_aggregate(ty, &descriptor)?,
                    MirOwnership::Copy => self.require_copy_aggregate(ty, &descriptor)?,
                    MirOwnership::Move => self.require_move_aggregate(ty, &descriptor)?,
                    ownership => {
                        return Err(format!(
                            "record '{}' ownership {:?} is outside the verifier capability",
                            ty.as_str(),
                            ownership
                        ));
                    }
                }
                for field in fields {
                    self.validate_type(&field.ty, "record field");
                }
                Ok(())
            }
            MirLayout::Option { variants, .. } | MirLayout::Result { variants, .. } => {
                if descriptor.ownership == MirOwnership::Copy {
                    self.require_copy_aggregate(ty, &descriptor)?;
                } else if self.allow_values_local_index_i32_result_branch
                    && ty
                        == &crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id()
                {
                    self.validate_values_local_index_i32_result_branch_result_type(ty)?;
                } else {
                    let validation = if self.allow_recoverable_flow_result
                        && matches!(descriptor.kind, MirTypeKind::Result)
                    {
                        catalog.validate_recoverable_result_variant(ty)
                    } else if self.ordinary_map_owned_get_result_string_types.contains(ty)
                        && matches!(descriptor.kind, MirTypeKind::Result)
                    {
                        catalog.validate_result_string_i32_variant(ty)
                    } else if self.allow_result_move_variant
                        && matches!(descriptor.kind, MirTypeKind::Result)
                    {
                        catalog
                            .validate_result_move_projection_variant(ty)
                            .map(|_| ())
                    } else {
                        catalog.validate_non_copy_variant_contract(ty)
                    };
                    validation.map_err(|message| {
                        format!(
                            "non-Copy variant TypeDesc is outside the verifier capability: {message}"
                        )
                    })?;
                }
                for variant in variants {
                    for field in &variant.fields {
                        self.validate_type(&field.ty, "variant field");
                    }
                }
                Ok(())
            }
            MirLayout::Enum { variants, .. } => {
                // Checker-materialized enum layouts (user enum type defs and
                // the multi-target Flow union) share the flat Copy variant
                // contract; a multi-target union additionally proves the
                // tagged-union contract (Copy scalars and owned Strings, one
                // payload per variant, complete glue) from the same
                // checker-owned TypeDesc regardless of combined ownership.
                // User enums stay flat-only.
                if descriptor.kind == MirTypeKind::FlowStateSet {
                    catalog
                        .validate_multi_target_union_variant(ty)
                        .map_err(|message| {
                            format!(
                                "multi-target union TypeDesc is outside the verifier capability: {message}"
                            )
                        })?;
                    // R6-1040: the compiler-owned Fault sink's payload fields
                    // (StateId/EventId/SystemTrace) sit outside the flat leaf
                    // families, but the union contract already proved each of
                    // them glue-complete; recursing into them as standalone
                    // enums would re-litigate shapes the sink owns.
                    for variant in variants {
                        if variant.name == "Fault" {
                            continue;
                        }
                        for field in &variant.fields {
                            self.validate_type(&field.ty, "variant field");
                        }
                    }
                    return Ok(());
                } else if descriptor.ownership != MirOwnership::Copy {
                    return Err(
                        "non-Copy enum TypeDesc is outside the verifier capability: only the flat Copy variant contract is admitted"
                            .into(),
                    );
                } else {
                    catalog.validate_flat_copy_variant(ty).map_err(|message| {
                        format!("Copy enum TypeDesc is outside the verifier capability: {message}")
                    })?;
                }
                for variant in variants {
                    for field in &variant.fields {
                        self.validate_type(&field.ty, "variant field");
                    }
                }
                Ok(())
            }
            layout => Err(format!(
                "layout {layout:?} is outside the verifier capability"
            )),
        }
    }

    fn validate_ordinary_map_owned_any_v2_checker_values_source_type(
        &self,
        ty: &crate::core::ResolvedTypeId,
        element: &crate::core::ResolvedTypeId,
    ) -> Result<(), String> {
        let catalog = self.program.type_catalog();
        let descriptor = catalog
            .get(ty)
            .ok_or_else(|| "Checker Values source List TypeDesc is absent".to_string())?;
        let expected_list_glue = crate::core::mir::types::MirGlueContract {
            move_out: MirGlueKind::List,
            clone: MirGlueKind::List,
            drop: MirGlueKind::List,
        };
        if descriptor.kind != MirTypeKind::List
            || descriptor.layout
                != (MirLayout::List {
                    element: element.clone(),
                })
            || descriptor.abi != MirAbiClass::OpaqueHandle
            || descriptor.ownership != MirOwnership::Move
            || !descriptor.needs_drop_glue
            || !descriptor.needs_clone_glue
            || descriptor.glue != expected_list_glue
            || descriptor.session_protocol.is_some()
            || descriptor.drop_plan.is_some()
            || descriptor.variant_drop_plan.is_some()
        {
            return Err(
                "Checker Values source List TypeDesc has an inconsistent canonical List contract"
                    .into(),
            );
        }
        let element_descriptor = catalog
            .get(element)
            .ok_or_else(|| "Checker Values source DynamicAny TypeDesc is absent".to_string())?;
        if element_descriptor.kind != MirTypeKind::DynamicAny
            || element_descriptor.abi != MirAbiClass::OpaqueHandle
            || element_descriptor.layout != MirLayout::Opaque
            || element_descriptor.ownership != MirOwnership::Move
        {
            return Err(
                "Checker Values source List element is not the canonical DynamicAny TypeDesc"
                    .into(),
            );
        }
        let is_executable_type = self.program.functions().values().any(|function| {
            function.result == *ty
                || function.parameters.iter().any(|parameter| {
                    function
                        .values
                        .get(parameter)
                        .is_some_and(|value| value.ty == *ty)
                })
                || function.values.values().any(|value| value.ty == *ty)
        });
        if is_executable_type {
            return Err(
                "Checker Values source List<DynamicAny> TypeDesc leaked into executable MIR".into(),
            );
        }
        Ok(())
    }

    fn require_copy_aggregate(
        &self,
        ty: &crate::core::ResolvedTypeId,
        descriptor: &crate::core::mir::types::MirTypeDesc,
    ) -> Result<(), String> {
        if descriptor.abi != MirAbiClass::Aggregate
            || !descriptor.has_canonical_copy_noop_metadata()
        {
            return Err(format!(
                "aggregate '{}' is not Copy with canonical no-op glue",
                ty.as_str()
            ));
        }
        Ok(())
    }

    fn require_linear_aggregate(
        &self,
        ty: &crate::core::ResolvedTypeId,
        descriptor: &crate::core::mir::types::MirTypeDesc,
    ) -> Result<(), String> {
        if descriptor.ownership != MirOwnership::Linear
            || descriptor.abi != MirAbiClass::Aggregate
            || descriptor.glue.move_out != MirGlueKind::Aggregate
            || descriptor.glue.clone != MirGlueKind::Aggregate
            || descriptor.glue.drop != MirGlueKind::Aggregate
            || descriptor.drop_plan.is_none()
        {
            return Err(format!(
                "linear aggregate '{}' lacks the canonical aggregate Move/Clone/Drop glue plan",
                ty.as_str()
            ));
        }
        Ok(())
    }

    fn require_move_aggregate(
        &self,
        ty: &crate::core::ResolvedTypeId,
        descriptor: &crate::core::mir::types::MirTypeDesc,
    ) -> Result<(), String> {
        if descriptor.ownership != MirOwnership::Move
            || descriptor.abi != MirAbiClass::Aggregate
            || descriptor.glue
                != (crate::core::mir::types::MirGlueContract {
                    move_out: MirGlueKind::Aggregate,
                    clone: MirGlueKind::Aggregate,
                    drop: MirGlueKind::Aggregate,
                })
            || !descriptor.needs_drop_glue
            || !descriptor.needs_clone_glue
            || descriptor.drop_plan.is_none()
        {
            return Err(format!(
                "move record '{}' lacks canonical aggregate glue/drop plan",
                ty.as_str()
            ));
        }
        Ok(())
    }

    fn validate_instruction(
        &mut self,
        function: &MirFunction,
        instruction_id: &crate::core::mir::MirInstructionId,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        let catalog = self.program.type_catalog();
        match instruction {
            MirInstructionKind::ManagedAny { result, source, operation, contract } => {
                if let Err(message) = catalog.validate_managed_any_contract(
                    function, source, result.as_ref(), *operation, contract.as_ref())
                {
                    self.error(format!("{subject} ManagedAny contract rejected: {message}"));
                }
            }
            MirInstructionKind::Actor { .. } => self.error(format!("{subject}: Actor mailbox/state instruction requires its complete finite execution receipt and no proof obligations")),
            MirInstructionKind::MapRootNew { result } => {
                if instruction_touches_map_any_owner(function, instruction) {
                    self.validate_map_any_owner_instruction(function, instruction, subject);
                } else {
                    self.validate_map_root_type(function, result, subject, "New result");
                }
            }
            MirInstructionKind::MapRootSet {
                result,
                source,
                key,
                value,
            } => {
                if instruction_touches_map_any_owner(function, instruction) {
                    self.validate_map_any_owner_instruction(function, instruction, subject);
                } else {
                    self.validate_map_root_type(function, result, subject, "Set result");
                    self.validate_map_root_type(function, source, subject, "Set source");
                    if key.contains('\0') {
                        self.error(format!("{subject} MapRoot key contains NUL"));
                    }
                    self.validate_map_root_set_payload(function, value, subject);
                }
            }
            MirInstructionKind::MapRootRemove {
                result,
                source,
                key,
            } => {
                if instruction_touches_map_any_owner(function, instruction) {
                    self.validate_map_any_owner_instruction(function, instruction, subject);
                } else {
                    self.validate_map_root_type(function, result, subject, "Remove result");
                    self.validate_map_root_type(function, source, subject, "Remove source");
                    if key.contains('\0') {
                        self.error(format!("{subject} MapRoot key contains NUL"));
                    }
                }
            }
            MirInstructionKind::MapRootSize { result, root } => {
                if instruction_touches_map_any_owner(function, instruction) {
                    self.validate_map_any_owner_instruction(function, instruction, subject);
                } else {
                    self.validate_map_root_type(function, root, subject, "Size root");
                    self.validate_i32_value(function, result, subject, "MapRoot Size result");
                }
            }
            MirInstructionKind::MapRootContains { result, root, key } => {
                if instruction_touches_map_any_owner(function, instruction) {
                    self.validate_map_any_owner_instruction(function, instruction, subject);
                } else {
                    self.validate_map_root_type(function, root, subject, "Contains root");
                    self.validate_bool_value(function, result, subject, "MapRoot Contains result");
                    if key.contains('\0') {
                        self.error(format!("{subject} MapRoot key contains NUL"));
                    }
                }
            }
            MirInstructionKind::MapRootDrop { root } => {
                if instruction_touches_map_any_owner(function, instruction) {
                    self.validate_map_any_owner_instruction(function, instruction, subject);
                } else {
                    self.validate_map_root_type(function, root, subject, "Drop operand");
                }
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleNew { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleSet { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleSize { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleContains { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln { .. } => {
                self.validate_ordinary_map_owner_instruction(function, instruction, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Println { .. } => {
                if !self.allow_direct_print {
                    self.error(format!(
                        "{subject} AnyOwner-v2 stdout observation is admitted only by exact DirectPrint route validation"
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { .. } => {
                if !self.allow_ordinary_map_owned_any_v2_is_empty {
                    self.error(format!(
                        "{subject} AnyOwner-v2 IsEmpty is admitted only by exact Checker/MIR route validation"
                    ));
                }
                self.validate_ordinary_map_owned_any_v2_instruction(function, instruction, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetAnyResult { .. } => {
                self.error(format!(
                    "{subject} Result-valued GetAny is admitted only by its exact CheckedProgram-bound L2 capability route"
                ));
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2SetAnyResult { .. } => {
                if !self.allow_computed_string_key_owner_chain
                    && !self.has_values_local_index_i32_result_branch_receipt(function)
                {
                    self.error(format!(
                        "{subject} SetAnyResult requires the exact ComputedStringKeyOwnerChain verifier route"
                    ));
                }
                self.validate_ordinary_map_owned_any_v2_instruction(function, instruction, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2New { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2SetStringResult { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetStringResult { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Size { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultString { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Values { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2DynamicArg0 { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { .. } => {
                self.validate_ordinary_map_owned_any_v2_instruction(function, instruction, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1NewResult { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1SetResult { .. } => {
                self.error(format!(
                    "{subject} owned-Get ResultBranchV3 is admitted only by its exact CheckedProgram/MIR route"
                ));
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { .. } => {
                self.validate_ordinary_map_owned_get_instruction(function, instruction, subject);
            }
            MirInstructionKind::Const { result, literal } => {
                let Some(ty) = value_type(function, result) else {
                    self.error(format!("{subject} constant result '{}' is absent", result));
                    return;
                };
                let supported = match literal {
                    ResolvedLiteral::Int(_) | ResolvedLiteral::Bool(_) => {
                        catalog.validate_copy_scalar(&ty).is_ok()
                    }
                    ResolvedLiteral::String(_) => catalog.validate_owned_string(&ty).is_ok(),
                    ResolvedLiteral::FloatBits(_) => {
                        catalog.validate_copy_float_scalar(&ty).is_ok()
                    }
                    ResolvedLiteral::Unit => false,
                };
                if !supported {
                    self.error(format!(
                        "{subject} literal {literal:?} is outside the verifier TypeDesc contract"
                    ));
                }
            }
            MirInstructionKind::Load { result, place } => {
                if place.projections.iter().any(|projection| {
                    matches!(
                        projection,
                        crate::core::ir::ResolvedProjection::Index { .. }
                    )
                }) {
                    self.error(format!(
                        "{subject} indexed Load is outside the verifier capability"
                    ));
                }
                if place.projections.iter().any(|projection| {
                    matches!(
                        projection,
                        crate::core::ir::ResolvedProjection::Deref { .. }
                    )
                }) {
                    self.error(format!(
                        "{subject} dereference Load is outside the verifier capability"
                    ));
                }
                let _ = result;
            }
            MirInstructionKind::Copy { result, source } => {
                self.require_same_type(function, result, source, subject);
                if let Some(ty) = value_type(function, source) {
                    if catalog.validate_copy_value(&ty).is_err() {
                        self.error(format!("{subject} Copy source '{}' is not Copy", source));
                    }
                }
            }
            MirInstructionKind::Move { result, source } => {
                self.require_same_type(function, result, source, subject);
                if let Some(ty) = value_type(function, source) {
                    let valid =
                        value_operation_is_supported(catalog, &ty, MirGlueOperation::MoveOut);
                    if !valid {
                        self.error(format!(
                            "{subject} Move source '{}' has no verifier glue",
                            source
                        ));
                    }
                }
            }
            MirInstructionKind::Clone { result, source } => {
                self.require_same_type(function, result, source, subject);
                if let Some(ty) = value_type(function, source) {
                    let valid = value_operation_is_supported(catalog, &ty, MirGlueOperation::Clone);
                    if !valid {
                        self.error(format!(
                            "{subject} Clone source '{}' has no verifier glue",
                            source
                        ));
                    }
                }
            }
            MirInstructionKind::Drop { value } => {
                if let Some(ty) = value_type(function, value) {
                    let valid = value_operation_is_supported(catalog, &ty, MirGlueOperation::Drop);
                    if !valid {
                        self.error(format!(
                            "{subject} Drop value '{}' has no verifier glue",
                            value
                        ));
                    }
                }
            }
            MirInstructionKind::Borrow {
                result,
                source,
                mutable,
                ..
            } => {
                let (Some(source_ty), Some(result_ty)) =
                    (value_type(function, source), value_type(function, result))
                else {
                    return;
                };
                if let Err(message) = catalog.validate_borrow(&source_ty, &result_ty, *mutable) {
                    self.error(format!("{subject} borrow rejected: {message}"));
                }
            }
            MirInstructionKind::EndBorrow { borrow } => {
                if let Some(ty) = value_type(function, borrow) {
                    if let Err(message) = catalog.validate_reference_type(&ty) {
                        self.error(format!("{subject} end-borrow rejected: {message}"));
                    }
                }
            }
            MirInstructionKind::Project {
                result,
                base,
                projection,
                list_index_contract,
            } => {
                let (Some(base_ty), Some(result_ty)) =
                    (value_type(function, base), value_type(function, result))
                else {
                    return;
                };
                match projection {
                    MirProjection::Index(index) => {
                        let Some(index_ty) = value_type(function, index) else {
                            return;
                        };
                        let Some(receipt) = list_index_contract else {
                            self.error(format!(
                                "{subject} List index projection has no canonical receipt"
                            ));
                            return;
                        };
                        if let Err(message) = catalog.validate_list_index_projection_receipt(
                            &base_ty, &index_ty, &result_ty, receipt,
                        ) {
                            self.error(format!("{subject} indexed projection rejected: {message}"));
                        }
                    }
                    MirProjection::Dereference => {
                        if let Err(message) = catalog.validate_dereference(&base_ty, &result_ty) {
                            self.error(format!("{subject} dereference rejected: {message}"));
                        }
                    }
                    MirProjection::ReadPath(receipt) => {
                        if list_index_contract.is_some() {
                            self.error(format!(
                                "{subject} List index receipt is attached to a read projection path"
                            ));
                        }
                        if let Err(message) =
                            catalog.validate_read_projection_receipt(&base_ty, &result_ty, receipt)
                        {
                            self.error(format!(
                                "{subject} read projection path rejected: {message}"
                            ));
                        }
                    }
                    MirProjection::Field(_) | MirProjection::Tuple(_) => {
                        if list_index_contract.is_some() {
                            self.error(format!(
                                "{subject} List index receipt is attached to a non-index projection"
                            ));
                            return;
                        }
                        if let Err(message) =
                            catalog.validate_projection(&base_ty, &result_ty, projection)
                        {
                            self.error(format!("{subject} projection rejected: {message}"));
                        }
                    }
                }
            }
            MirInstructionKind::MoveProject {
                result,
                base,
                projection,
            } => {
                let (Some(base_ty), Some(result_ty)) =
                    (value_type(function, base), value_type(function, result))
                else {
                    return;
                };
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_move_projection(&base_ty, &result_ty, projection)
                {
                    self.error(format!("{subject} MoveProject rejected: {message}"));
                }
            }
            MirInstructionKind::MoveProjectDrop {
                result,
                base,
                projection,
                contract,
            } => {
                let (Some(base_ty), Some(result_ty)) =
                    (value_type(function, base), value_type(function, result))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} MoveProjectDrop has no canonical residual receipt"
                    ));
                    return;
                };
                if let Err(message) = catalog
                    .validate_record_move_projection_drop_receipt(&base_ty, &result_ty, receipt)
                {
                    self.error(format!("{subject} MoveProjectDrop rejected: {message}"));
                }
                if !matches!(projection, MirProjection::Field(_)) {
                    self.error(format!(
                        "{subject} MoveProjectDrop requires a direct record field projection"
                    ));
                }
            }
            MirInstructionKind::VariantProject {
                result,
                base,
                contract,
            } => {
                let (Some(base_ty), Some(result_ty)) =
                    (value_type(function, base), value_type(function, result))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} direct variant projection has no canonical trap receipt"
                    ));
                    return;
                };
                let generic_result_copy_projection =
                    self.program.instances().values().any(|instance| {
                        instance.function == function.owner
                            && matches!(
                                &instance.contract,
                                MirGenericInstanceContract::ScalarVariantProjection {
                                    contract
                                } if contract.projection.nominal.as_str() == "builtin:type:Result"
                                    && contract.projection.ownership == MirOwnership::Copy
                            )
                    });
                let receipt_validation = if generic_result_copy_projection {
                    catalog.validate_generic_result_projection_trap_receipt(
                        &base_ty, &result_ty, receipt,
                    )
                } else {
                    catalog.validate_variant_projection_trap_receipt(&base_ty, &result_ty, receipt)
                };
                if let Err(message) = receipt_validation {
                    self.error(format!(
                        "{subject} direct variant projection rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::VariantProjectOr {
                result,
                base,
                fallback,
                contract,
            } => {
                let (Some(base_ty), Some(result_ty), Some(fallback_ty)) = (
                    value_type(function, base),
                    value_type(function, result),
                    value_type(function, fallback),
                ) else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} variant projection fallback has no canonical receipt"
                    ));
                    return;
                };
                let generic_option_copy_fallback =
                    self.program.instances().values().any(|instance| {
                        instance.function == function.owner
                            && matches!(
                                &instance.contract,
                                MirGenericInstanceContract::ScalarVariantProjectionFallback {
                                    contract
                                } if contract.projection.nominal.as_str() == "builtin:type:Option"
                                    && contract.projection.ownership == MirOwnership::Copy
                            )
                    });
                let generic_result_copy_fallback =
                    self.program.instances().values().any(|instance| {
                        instance.function == function.owner
                            && matches!(
                                &instance.contract,
                                MirGenericInstanceContract::ScalarVariantProjectionFallback {
                                    contract
                                } if contract.projection.nominal.as_str() == "builtin:type:Result"
                                    && contract.projection.ownership == MirOwnership::Copy
                            )
                    });
                let receipt_validation = if generic_option_copy_fallback {
                    catalog
                        .validated_copy_option_generic_projection_fallback_contract(
                            &base_ty,
                            &receipt.projection.variant,
                            &receipt.projection.field,
                            &result_ty,
                            &fallback_ty,
                        )
                        .map(|_| ())
                } else if generic_result_copy_fallback
                    && catalog.get(&base_ty).is_some_and(|descriptor| {
                        matches!(
                            &descriptor.layout,
                            MirLayout::Result { ok, error, .. }
                                if ok == &result_ty && ok != error
                        )
                    })
                {
                    catalog
                        .validated_generic_result_scalar_projection_fallback_contract(
                            &base_ty,
                            &receipt.projection.variant,
                            &receipt.projection.field,
                            &result_ty,
                            &fallback_ty,
                        )
                        .map(|_| ())
                } else {
                    catalog.validate_variant_projection_fallback_receipt(
                        &base_ty,
                        &result_ty,
                        &fallback_ty,
                        receipt,
                    )
                };
                if let Err(message) = receipt_validation {
                    self.error(format!(
                        "{subject} variant projection fallback rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::VariantProjectMove {
                result,
                base,
                contract,
            } => {
                let (Some(base_ty), Some(result_ty)) =
                    (value_type(function, base), value_type(function, result))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} consuming direct variant projection has no canonical move receipt"
                    ));
                    return;
                };
                if let Err(message) = catalog
                    .validate_variant_move_projection_trap_receipt(&base_ty, &result_ty, receipt)
                {
                    self.error(format!(
                        "{subject} consuming direct variant projection rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::Construct {
                result,
                kind,
                fields,
            } => {
                let Some(result_ty) = value_type(function, result) else {
                    return;
                };
                let field_types = fields
                    .iter()
                    .filter_map(|value| value_type(function, value))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    self.error(format!("{subject} aggregate field is absent"));
                } else if let Err(message) =
                    catalog.validate_aggregate(&result_ty, kind, &field_types)
                {
                    self.error(format!("{subject} aggregate rejected: {message}"));
                }
            }
            MirInstructionKind::ConstructList {
                result,
                elements,
                list_construct_contract,
            } => {
                let Some(result_ty) = value_type(function, result) else {
                    return;
                };
                let element_types = elements
                    .iter()
                    .filter_map(|value| value_type(function, value))
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    self.error(format!("{subject} List element is absent"));
                } else if let Some(receipt) = list_construct_contract.as_ref() {
                    if let Err(message) =
                        catalog.validate_list_construct_receipt(&result_ty, &element_types, receipt)
                    {
                        self.error(format!("{subject} List construction rejected: {message}"));
                    }
                } else {
                    self.error(format!(
                        "{subject} List construction has no canonical receipt"
                    ));
                }
            }
            MirInstructionKind::ListOp {
                result,
                operation,
                list,
                argument,
                list_operation_contract,
            } => {
                let (Some(result_ty), Some(list_ty)) =
                    (value_type(function, result), value_type(function, list))
                else {
                    return;
                };
                let Some(receipt) = list_operation_contract.as_ref() else {
                    self.error(format!("{subject} List operation has no canonical receipt"));
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| function.values.get(value))
                    .map(|value| value.ty.clone());
                if let Err(message) = catalog.validate_list_operation_receipt_with_argument(
                    &result_ty,
                    &list_ty,
                    argument_ty.as_ref(),
                    *operation,
                    receipt,
                ) {
                    self.error(format!("{subject} List operation rejected: {message}"));
                }
            }
            MirInstructionKind::ConstructSet { result, elements } => {
                let Some(result_ty) = value_type(function, result) else {
                    return;
                };
                let element_types = elements
                    .iter()
                    .filter_map(|value| value_type(function, value))
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    self.error(format!("{subject} Set element is absent"));
                } else if let Err(message) =
                    catalog.validate_set_construct(&result_ty, &element_types)
                {
                    self.error(format!("{subject} Set construction rejected: {message}"));
                }
            }
            MirInstructionKind::SetOp {
                result,
                operation,
                set,
                argument,
            } => {
                let (Some(result_ty), Some(set_ty)) =
                    (value_type(function, result), value_type(function, set))
                else {
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| value_type(function, value));
                if argument.is_some() && argument_ty.is_none() {
                    self.error(format!("{subject} Set argument is absent"));
                } else if let Err(message) = catalog.validate_set_operation(
                    &result_ty,
                    &set_ty,
                    argument_ty.as_ref(),
                    *operation,
                ) {
                    self.error(format!("{subject} Set operation rejected: {message}"));
                }
            }
            MirInstructionKind::ConstructVariant {
                result,
                nominal,
                variant,
                fields,
            } => {
                let Some(result_ty) = value_type(function, result) else {
                    return;
                };
                let field_ids = fields
                    .iter()
                    .map(|(field, _)| field.clone())
                    .collect::<Vec<_>>();
                let field_types = fields
                    .iter()
                    .filter_map(|(_, value)| value_type(function, value))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    self.error(format!("{subject} variant payload is absent"));
                } else if let Err(message) = catalog.validate_variant_construct(
                    &result_ty,
                    nominal,
                    variant,
                    &field_ids,
                    &field_types,
                ) {
                    self.error(format!(
                        "{subject} variant construction rejected: {message}"
                    ));
                } else if catalog.get(&result_ty).map_or(true, |descriptor| {
                    descriptor.ownership != MirOwnership::Copy
                }) {
                    self.error(format!("{subject} non-Copy variant construction is outside the verifier capability"));
                }
            }
            MirInstructionKind::ConstructVariantMove {
                result,
                nominal,
                variant,
                fields,
            } => {
                let Some(result_ty) = value_type(function, result) else {
                    self.error(format!("{subject} variant result is absent"));
                    return;
                };
                let move_variant = catalog
                    .validate_multi_target_union_variant(&result_ty)
                    .map(|_| ())
                    .or_else(|_| {
                        catalog.validate_option_move_variant(&result_ty).map(|_| ())
                    })
                    .or_else(|_| {
                        if self.allow_recoverable_flow_result {
                            catalog.validate_recoverable_result_variant(&result_ty)
                        } else {
                            Err("recoverable Flow Result construction is outside this verifier profile".into())
                        }
                    })
                    .or_else(|_| {
                        if self.allow_nested_option_tuple_variant {
                            catalog
                                .validate_option_nested_tuple_variant(&result_ty)
                                .map(|_| ())
                        } else {
                            Err("nested Option tuple construction requires the explicit projection receipt".into())
                        }
                    })
                    .or_else(|_| {
                        if self
                            .ordinary_map_owned_get_result_string_types
                            .contains(&result_ty)
                        {
                            catalog.validate_result_string_i32_variant(&result_ty)
                        } else {
                            Err("owned Map getter Result construction is outside this profile".into())
                        }
                    })
                    .or_else(|_| {
                        if self.allow_result_move_variant {
                            catalog
                                .validate_result_move_projection_variant(&result_ty)
                                .map(|_| ())
                        } else {
                            Err("move-owned Result construction requires the promoted generic Result projection receipt".into())
                        }
                    });
                if let Err(message) = move_variant {
                    self.error(format!(
                        "{subject} ConstructVariantMove rejected: {message}"
                    ));
                    return;
                }
                let field_ids = fields
                    .iter()
                    .map(|(field, _)| field.clone())
                    .collect::<Vec<_>>();
                let field_types = fields
                    .iter()
                    .filter_map(|(_, value)| value_type(function, value))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    self.error(format!("{subject} variant payload is absent"));
                } else if let Err(message) = catalog.validate_variant_construct(
                    &result_ty,
                    nominal,
                    variant,
                    &field_ids,
                    &field_types,
                ) {
                    self.error(format!(
                        "{subject} ConstructVariantMove rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::UpdateRecord {
                result,
                base,
                kind,
                fields,
                record_update_contract,
                record_update_move_contract,
            } => {
                let (Some(result_ty), Some(base_ty)) =
                    (value_type(function, result), value_type(function, base))
                else {
                    return;
                };
                let field_types = fields
                    .iter()
                    .filter_map(|value| value_type(function, value))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    self.error(format!("{subject} record update field is absent"));
                } else {
                    // The shape-only validator is insufficient for an owned
                    // aggregate: without a move receipt, consumers cannot
                    // know whether the base is transferred or cloned. Keep
                    // this proof boundary closed even when the ordinary MIR
                    // builder intentionally preserves the operation.
                    if record_update_move_contract.is_none()
                        && catalog
                            .get(&result_ty)
                            .is_some_and(|descriptor| descriptor.ownership != MirOwnership::Copy)
                    {
                        self.error(format!(
                            "{subject} non-Copy record update requires a validated move receipt"
                        ));
                    }
                    let validation = if let Some(receipt) = record_update_move_contract {
                        catalog.validate_record_update_move_receipt(
                            &result_ty,
                            &base_ty,
                            kind,
                            &field_types,
                            receipt,
                        )
                    } else if let Some(receipt) = record_update_contract {
                        catalog.validate_record_update_receipt(
                            &result_ty,
                            &base_ty,
                            kind,
                            &field_types,
                            receipt,
                        )
                    } else {
                        catalog.validate_record_update(&result_ty, &base_ty, kind, &field_types)
                    };
                    if let Err(message) = validation {
                        self.error(format!("{subject} record update rejected: {message}"));
                    }
                }
            }
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => {
                self.validate_binary(function, result, *op, left, right, subject);
            }
            MirInstructionKind::Unary {
                result,
                op,
                operand,
            } => {
                self.require_same_type_if_unary(function, result, operand, *op, subject);
            }
            MirInstructionKind::BuiltinCall {
                kind,
                arguments,
                string_field_contract,
                ..
            } => {
                if !matches!(
                    kind,
                    crate::core::mir::types::MirBuiltinKind::Abs
                        | crate::core::mir::types::MirBuiltinKind::Min
                        | crate::core::mir::types::MirBuiltinKind::Max
                        | crate::core::mir::types::MirBuiltinKind::PrintlnBool
                        | crate::core::mir::types::MirBuiltinKind::PrintlnInt
                        | crate::core::mir::types::MirBuiltinKind::PrintlnString
                        | crate::core::mir::types::MirBuiltinKind::PrintlnFloat
                        | crate::core::mir::types::MirBuiltinKind::SessionOpen
                        | crate::core::mir::types::MirBuiltinKind::SessionPair
                        | crate::core::mir::types::MirBuiltinKind::Assert
                        | crate::core::mir::types::MirBuiltinKind::AssertEq
                        | crate::core::mir::types::MirBuiltinKind::AssertNe
                        | crate::core::mir::types::MirBuiltinKind::AssertApproxEq
                ) {
                    self.error(format!(
                        "{subject} builtin is outside the verifier capability"
                    ));
                }
                if let Some(receipt) = string_field_contract {
                    if !self.allow_direct_print && self.program_touches_map_or_any() {
                        self.error(format!(
                            "{subject} String-field borrow output requires exact Checker/MIR route validation"
                        ));
                    }
                    if *kind != crate::core::mir::types::MirBuiltinKind::PrintlnString {
                        self.error(format!(
                            "{subject} String-field borrow receipt is attached to a non-String builtin"
                        ));
                    } else if arguments.len() != 1 {
                        self.error(format!(
                            "{subject} String-field borrow println requires one record source"
                        ));
                    } else if let Some(source_ty) = value_type(function, &arguments[0]) {
                        if let Err(message) =
                            catalog.validate_string_field_borrow_receipt(&source_ty, receipt)
                        {
                            self.error(format!(
                                "{subject} String-field borrow receipt rejected: {message}"
                            ));
                        }
                    }
                }
            }
            MirInstructionKind::SessionCall {
                result,
                operation,
                endpoint,
                payload,
                contract,
            } => {
                let (Some(endpoint_ty), Some(result_ty)) =
                    (value_type(function, endpoint), value_type(function, result))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} SessionCall has no canonical residual/ABI receipt"
                    ));
                    return;
                };
                if let Err(message) =
                    catalog.validate_session_call_contract(&endpoint_ty, &result_ty, receipt)
                {
                    self.error(format!("{subject} SessionCall rejected: {message}"));
                }
                if payload.is_some() != receipt.payload_ty.is_some() {
                    self.error(format!(
                        "{subject} SessionCall payload disagrees with its receipt TypeDesc"
                    ));
                }
                if let Some(payload) = payload {
                    if let (Some(payload_ty), Some(receipt_ty)) =
                        (value_type(function, payload), receipt.payload_ty.as_ref())
                    {
                        if payload_ty != *receipt_ty {
                            self.error(format!(
                                "{subject} SessionCall payload value disagrees with its receipt TypeDesc identity"
                            ));
                        }
                    }
                }
                if receipt.operation != *operation {
                    self.error(format!(
                        "{subject} SessionCall receipt disagrees with MIR operation"
                    ));
                }
            }
            MirInstructionKind::SessionPairBind { lo, hi, contract } => {
                let (Some(lo_ty), Some(hi_ty)) =
                    (value_type(function, lo), value_type(function, hi))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} typed session_pair binding has no canonical receipt"
                    ));
                    return;
                };
                if let Err(message) = catalog.validate_session_pair_bind_receipt(
                    &receipt.pair_ty,
                    &lo_ty,
                    &hi_ty,
                    receipt,
                ) {
                    self.error(format!(
                        "{subject} typed session_pair binding rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::VariantPredicate {
                result,
                predicate,
                variant,
                contract,
            } => {
                let (Some(result_ty), Some(variant_ty)) =
                    (value_type(function, result), value_type(function, variant))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} variant predicate has no canonical receipt"
                    ));
                    return;
                };
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_variant_predicate_receipt(
                        &result_ty,
                        &variant_ty,
                        *predicate,
                        receipt,
                    )
                {
                    self.error(format!("{subject} variant predicate rejected: {message}"));
                }
            }
            MirInstructionKind::Convert { result, source } => {
                let (Some(source_ty), Some(result_ty)) =
                    (value_type(function, source), value_type(function, result))
                else {
                    return;
                };
                match catalog.validate_conversion(&source_ty, &result_ty) {
                    Ok(contract)
                        if matches!(
                            contract.kind,
                            crate::core::mir::types::MirConversionKind::ScalarIdentity
                                | crate::core::mir::types::MirConversionKind::SignedI32ToI64
                                | crate::core::mir::types::MirConversionKind::SignedI32ToFloat64
                                | crate::core::mir::types::MirConversionKind::SignedI64ToFloat64
                                | crate::core::mir::types::MirConversionKind::Float64ToFloat32
                        ) => {}
                    Ok(_) => self.error(format!(
                        "{subject} conversion is outside the verifier capability"
                    )),
                    Err(message) => self.error(format!("{subject} conversion rejected: {message}")),
                }
            }
            MirInstructionKind::Call {
                result,
                callee,
                type_arguments,
                arguments,
                variant_call_contract,
                ..
            } => {
                self.validate_call(
                    function,
                    instruction_id,
                    result.as_ref(),
                    callee,
                    type_arguments,
                    arguments,
                    variant_call_contract.as_ref(),
                    subject,
                );
            }
            MirInstructionKind::ClosureBind {
                closure_local,
                function_value,
                abi,
                ..
            } => {
                // The bind defines only a Copy function-TypeDesc identity;
                // no runtime closure pointer is created.
                if let Err(message) = super::mir::validate_closure_callable_value(
                    self.program,
                    function,
                    closure_local,
                    function_value,
                    &abi.source_function_type,
                    abi.source_abi,
                    abi.source_parameter_types.len(),
                ) {
                    self.error(format!("{subject} closure bind callable value rejected: {message}"));
                }
            }
            MirInstructionKind::ClosureInvoke {
                closure_local,
                function_value,
                abi,
                result,
                callee,
                type_arguments,
                arguments,
            } => {
                if let Err(message) = super::mir::validate_closure_callable_value(
                    self.program,
                    function,
                    closure_local,
                    function_value,
                    &abi.source_function_type,
                    abi.source_abi,
                    abi.source_parameter_types.len(),
                ) {
                    self.error(format!("{subject} closure invoke callable value rejected: {message}"));
                }
                let direct = ResolvedCallee::Function(callee.clone());
                for argument in arguments {
                    if value_type(function, argument).is_some_and(|ty| {
                        self.program.type_catalog().validate_session_channel(&ty).is_ok()
                    }) {
                        self.error(format!("{subject} ClosureInvoke has no TransferSession effect receipt"));
                    }
                }
                self.validate_call(
                    function,
                    instruction_id,
                    result.as_ref(),
                    &direct,
                    type_arguments,
                    arguments,
                    None,
                    subject,
                );
            }
            MirInstructionKind::FlowTransition {
                result,
                transition,
                arguments,
                effect_receipt,
            } => self.validate_flow_transition(
                function,
                result,
                transition,
                arguments,
                effect_receipt.as_ref(),
                subject,
            ),
            MirInstructionKind::Nop => {}
        }
    }

    fn validate_flow_transition(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        transition: &crate::core::NodeId,
        arguments: &[MirValueId],
        effect_receipt: Option<&crate::core::mir::types::MirFlowEffectReceipt>,
        subject: &str,
    ) {
        let Some(contract) = self.program.transitions().get(transition) else {
            self.error(format!(
                "{subject} FlowTransition '{}' has no canonical contract",
                transition.0
            ));
            return;
        };
        let recoverable = contract.effect.is_recoverable();
        let union =
            crate::core::mir::multi_target_union_shape(contract, self.program.type_catalog());
        if (!recoverable
            && !matches!(
                contract.effect,
                crate::core::mir::MirTransitionEffect::SilentLocal
                    | crate::core::mir::MirTransitionEffect::Boundary
            ))
            || (contract.targets.len() != 1 && !union)
            || (!recoverable && contract.failure.is_some())
            || contract.is_fallback
            || contract.is_ffi_pinned
            || (recoverable && contract.failure.is_none())
        {
            self.error(format!(
                "{subject} FlowTransition is outside the silent-local/recoverable transition capability"
            ));
        }
        let argument_types = arguments
            .iter()
            .filter_map(|argument| function.values.get(argument).map(|value| value.ty.clone()))
            .collect::<Vec<_>>();
        if argument_types.len() == arguments.len() {
            let result_ty = function
                .values
                .get(result)
                .map(|value| value.ty.clone())
                .unwrap_or_else(|| contract.result.clone());
            if let Err(message) = crate::core::mir::validate_flow_effect_receipt(
                transition,
                contract,
                &argument_types,
                &result_ty,
                effect_receipt,
                self.program.type_catalog(),
            ) {
                self.error(format!("{subject} {message}"));
            }
        }
        let Some(target) = self.program.functions().get(&contract.owner) else {
            self.error(format!(
                "{subject} FlowTransition target '{}' is absent",
                contract.owner.0
            ));
            return;
        };
        if arguments.len() != target.parameters.len() {
            self.error(format!(
                "{subject} FlowTransition argument arity disagrees with its canonical body"
            ));
        }
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if value_type(function, argument) != value_type(target, parameter) {
                self.error(format!(
                    "{subject} FlowTransition argument TypeDesc disagrees with its canonical body"
                ));
            }
        }
        match value_type(function, result) {
            Some(actual)
                if actual == contract.result
                    && (recoverable || actual == target.result)
                    && (!recoverable || target.result == contract.result) => {}
            _ => self.error(format!(
                "{subject} FlowTransition result TypeDesc disagrees with its canonical contract"
            )),
        }
    }

    fn validate_call(
        &mut self,
        function: &MirFunction,
        instruction_id: &crate::core::mir::MirInstructionId,
        result: Option<&MirValueId>,
        callee: &ResolvedCallee,
        type_arguments: &[crate::core::ResolvedTypeId],
        arguments: &[MirValueId],
        variant_call_contract: Option<&crate::core::mir::types::MirVariantCallAbiContract>,
        subject: &str,
    ) {
        if let ResolvedCallee::Extern(owner) = callee {
            validate_extern_call_capabilities(
                self.program,
                self.allow_typed_ffi_verifier_proofs,
                function,
                instruction_id,
                result,
                owner,
                arguments,
                subject,
                |message| self.error(message),
            );
            return;
        }
        let owner = match callee {
            ResolvedCallee::Function(owner) => owner.clone(),
            ResolvedCallee::ProtocolMethod { .. } => {
                if let Err(message) = crate::core::mir::validate_protocol_method_identity(callee) {
                    self.error(format!("{subject} {message}"));
                    return;
                }
                let Some(owner) = crate::core::mir::canonical_protocol_call_target(callee) else {
                    self.error(format!(
                        "{subject} callee is outside the canonical MIR verifier capability"
                    ));
                    return;
                };
                owner
            }
            _ => {
                self.error(format!(
                    "{subject} callee is outside the canonical MIR verifier capability"
                ));
                return;
            }
        };
        let Some(target) = self.program.functions().get(&owner) else {
            self.error(format!("{subject} callee '{}' is absent", owner.0));
            return;
        };
        let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        let targets_values = target.result == values_ty
            || result.is_some_and(|result| {
                function
                    .values
                    .get(result)
                    .is_some_and(|value| value.ty == values_ty)
            });
        let values_route_call = result.is_some_and(|result| {
            self.values_call_is_receipted(function, instruction_id, &owner, result)
        });
        if targets_values && !values_route_call {
            self.error(format!(
                "{subject} Values result call is outside the exact Checker-bound producer/caller route"
            ));
            return;
        }
        let map_root_ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
        let targets_map_root = target.result == map_root_ty
            || result.is_some_and(|result| {
                function
                    .values
                    .get(result)
                    .is_some_and(|value| value.ty == map_root_ty)
            });
        let map_root_route_call = self.allow_ordinary_map_owned_any_v2_map_root_producer_caller
            && result.is_some_and(|result| {
                map_root_producer_caller_bridge_for_caller(self.program, function).is_some_and(
                    |receipt| {
                        receipt.caller_call_instruction == *instruction_id
                            && receipt.call_target == owner
                            && receipt.caller_call_result == *result
                    },
                ) || map_root_get_any_caller_bridge_for_caller(self.program, function).is_some_and(
                    |receipt| {
                        receipt.caller_call_instruction == *instruction_id
                            && receipt.call_target == owner
                            && receipt.caller_call_result == *result
                    },
                )
            });
        if targets_map_root && !map_root_route_call {
            self.error(format!(
                "{subject} MapRoot result call is outside the exact Checker-bound producer/caller route"
            ));
            return;
        }
        let instance = self
            .program
            .instances()
            .values()
            .find(|instance| instance.function == owner);
        if let Some(instance) = instance {
            let managed_any_crossing = crate::verifier::mir::direct_call_uses_managed_any(
                target,
                self.program.type_catalog(),
            ) || type_arguments.iter().any(|ty| {
                self.program
                    .type_catalog()
                    .contains_dynamic_or_managed_any(ty)
            });
            if managed_any_crossing
                && !matches!(
                    &instance.contract,
                    MirGenericInstanceContract::ManagedAnyIdentity
                        | MirGenericInstanceContract::ManagedAnyTrapCleanup { .. }
                )
            {
                self.error(format!(
                    "{subject} ManagedAny cannot cross an unreceipted generic instance ABI"
                ));
                return;
            }
            if instance.arguments != type_arguments {
                self.error(format!(
                    "{subject} generic arguments disagree with instance '{}'",
                    instance.id
                ));
                return;
            }
            match instance.contract {
                MirGenericInstanceContract::ScalarIdentity
                | MirGenericInstanceContract::OwnedStringIdentity
                | MirGenericInstanceContract::ScalarSetFacade { .. }
                | MirGenericInstanceContract::ScalarListFacade { .. }
                | MirGenericInstanceContract::ScalarListConstruct { .. }
                | MirGenericInstanceContract::ScalarListProjection { .. }
                | MirGenericInstanceContract::ScalarRecordProjection { .. }
                | MirGenericInstanceContract::ScalarRecordUpdate { .. }
                | MirGenericInstanceContract::ScalarTupleProjection { .. }
                | MirGenericInstanceContract::OwnedRecordProjection { .. }
                | MirGenericInstanceContract::OwnedRecordProjectionDrop { .. }
                | MirGenericInstanceContract::OwnedRecordUpdate { .. }
                | MirGenericInstanceContract::ScalarVariantPredicate { .. }
                | MirGenericInstanceContract::ScalarVariantProjection { .. }
                | MirGenericInstanceContract::ScalarVariantProjectionFallback { .. } => {}
                MirGenericInstanceContract::ScalarArgumentProjection {
                    parameter_index,
                    parameter_count,
                } => {
                    let Some(concrete) = instance.arguments.first() else {
                        self.error(format!(
                            "{subject} argument projection instance has no concrete substitution"
                        ));
                        return;
                    };
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_scalar_generic_arguments(&instance.arguments)
                    {
                        self.error(format!(
                            "{subject} argument projection substitution is unsupported: {message}"
                        ));
                        return;
                    }
                    if let Err(message) =
                        crate::core::mir::validate_generic_argument_projection_shape(
                            target,
                            concrete,
                            parameter_index,
                            parameter_count,
                        )
                    {
                        self.error(format!(
                            "{subject} argument projection target receipt is invalid: {message}"
                        ));
                        return;
                    }
                    let exact_arguments = arguments.len() == parameter_count as usize
                        && arguments
                            .iter()
                            .zip(&target.parameters)
                            .all(|(actual, formal)| {
                                let actual_ty = function.values.get(actual).map(|value| &value.ty);
                                let formal_ty = target.values.get(formal).map(|value| &value.ty);
                                actual_ty == Some(concrete)
                                    && formal_ty == Some(concrete)
                                    && actual_ty == formal_ty
                            });
                    let exact_result = result.is_some_and(|result| {
                        function
                            .values
                            .get(result)
                            .is_some_and(|value| value.ty == *concrete)
                    });
                    if !exact_arguments || !exact_result || parameter_index >= parameter_count {
                        self.error(format!(
                            "{subject} argument projection call disagrees with its exact substitution, arity, or returned parameter position"
                        ));
                        return;
                    }
                }
                MirGenericInstanceContract::ManagedAnyIdentity => {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_managed_any_identity_instance(instance, target)
                    {
                        self.error(format!(
                            "{subject} ManagedAny identity instance receipt rejected: {message}"
                        ));
                        return;
                    }
                }
                MirGenericInstanceContract::ManagedAnyTrapCleanup { .. } => {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_managed_any_trap_cleanup_instance(instance, target)
                    {
                        self.error(format!(
                            "{subject} ManagedAny trap-cleanup instance receipt rejected: {message}"
                        ));
                        return;
                    }
                }
            }
        } else if !type_arguments.is_empty() {
            self.error(format!(
                "{subject} generic arguments target a non-instance function"
            ));
            return;
        } else if function_has_ensures(function)
            && !variant_call_contract.is_some_and(|receipt| {
                receipt.mode == crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
                    && result
                        .and_then(|result| function.values.get(result))
                        .is_some_and(|value| {
                            self.program
                                .type_catalog()
                                .get(&value.ty)
                                .is_some_and(|descriptor| descriptor.kind == MirTypeKind::Result)
                        })
            })
            // R6-1073: unless the call routes to the direct-scalar evaluator.
            // `eval_direct_scalar_call` symbolically executes the callee body
            // (explore_block, merged return paths), propagates the callee's
            // trap and FFI obligations into this function's accounting, and
            // rejects recursion, arity drift and out-of-ABI types itself — so
            // a Copy-scalar (or unit) call whose graph cannot reach back into
            // this function is exactly modeled even here.  The gate mirrors
            // the evaluator's own routing preconditions and admits nothing
            // the evaluator would not route: aggregate/String callees keep
            // the registered floor.
            && !self.call_routes_to_direct_scalar_evaluation(function, target, result)
            && !self.call_routes_to_direct_owned_string_evaluation(function, target, result)
            && !self.call_routes_to_direct_managed_any_evaluation(function, target, result)
            && !values_route_call
            && !map_root_route_call
            && !self.call_routes_to_managed_any_trap_cleanup_evaluation(
                function,
                target,
                instance,
                type_arguments,
                result,
            )
        {
            self.error(format!(
                "{subject} ordinary call in a contract-bearing function is outside the verifier capability"
            ));
            return;
        }
        if arguments.len() != target.parameters.len() {
            self.error(format!("{subject} call arity disagrees with callee"));
        }
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if value_type(function, argument) != value_type(target, parameter) {
                self.error(format!(
                    "{subject} call argument TypeDesc disagrees with callee"
                ));
            }
        }
        match (result, self.program.type_catalog().get(&target.result)) {
            (Some(result), Some(_))
                if value_type(function, result).as_ref() != Some(&target.result) =>
            {
                self.error(format!(
                    "{subject} call result TypeDesc disagrees with callee"
                ));
            }
            (None, Some(descriptor)) if !descriptor.is_canonical_ffi_unit() => {
                self.error(format!("{subject} non-unit call has no result value"));
            }
            _ => {}
        }
    }

    fn call_routes_to_direct_managed_any_evaluation(
        &self,
        function: &MirFunction,
        target: &MirFunction,
        result: Option<&MirValueId>,
    ) -> bool {
        let catalog = self.program.type_catalog();
        crate::verifier::mir::direct_call_uses_managed_any(target, catalog)
            && target.validate().is_ok()
            && catalog.validate_managed_any_function(target).is_ok()
            && target.blocks.len() == 1
            && crate::verifier::mir::is_direct_managed_any_call_type(catalog, &target.result)
            && target.parameters.iter().all(|parameter| {
                value_type(target, parameter).is_some_and(|ty| {
                    crate::verifier::mir::is_direct_managed_any_call_type(catalog, &ty)
                })
            })
            && !crate::verifier::mir::direct_call_graph_reaches(
                self.program,
                &target.owner,
                &function.owner,
                &mut BTreeSet::new(),
            )
            && (result.is_some()
                || catalog
                    .get(&target.result)
                    .is_some_and(|desc| desc.is_canonical_ffi_unit()))
    }

    /// A generic ManagedAny cleanup call is admitted here only for its exact
    /// sealed trap profile. Its evaluator records the sealed E0801 or E0802 trap
    /// and terminates the normal path, so the structural i32 return cannot be
    /// mistaken for a usable value in a contract-bearing caller.
    fn call_routes_to_managed_any_trap_cleanup_evaluation(
        &self,
        function: &MirFunction,
        target: &MirFunction,
        instance: Option<&MirInstance>,
        type_arguments: &[crate::core::ResolvedTypeId],
        result: Option<&MirValueId>,
    ) -> bool {
        let Some(instance) = instance else {
            return false;
        };
        if !matches!(
            &instance.contract,
            MirGenericInstanceContract::ManagedAnyTrapCleanup { .. }
        ) {
            return false;
        }
        instance.function == target.owner
            && instance.arguments == type_arguments
            && self
                .program
                .type_catalog()
                .validate_managed_any_trap_cleanup_instance(instance, target)
                .is_ok()
            && !crate::verifier::mir::direct_call_graph_reaches(
                self.program,
                &target.owner,
                &function.owner,
                &mut BTreeSet::new(),
            )
            && result.is_some()
    }

    /// R6-1073: mirror of the verifier's direct-scalar call routing.  The
    /// evaluator routes a non-instance call with no type arguments to
    /// `eval_direct_scalar_call` exactly when the callee result is inside
    /// the Copy scalar ABI (or unit) — and that evaluator refuses recursion
    /// and unit-result mismatches itself.  The gate re-checks the same
    /// preconditions so it admits only calls the evaluator actually routes,
    /// keeping the capability gate and the symbolic engine aligned instead
    /// of fail-closed against something the engine models.
    fn call_routes_to_direct_scalar_evaluation(
        &self,
        function: &MirFunction,
        target: &MirFunction,
        result: Option<&MirValueId>,
    ) -> bool {
        let catalog = self.program.type_catalog();
        !crate::verifier::mir::direct_call_uses_managed_any(target, catalog)
            && crate::verifier::mir::is_direct_scalar_call_type(catalog, &target.result)
            && !crate::verifier::mir::direct_call_graph_reaches(
                self.program,
                &target.owner,
                &function.owner,
                &mut BTreeSet::new(),
            )
            && (result.is_some()
                || catalog
                    .get(&target.result)
                    .is_some_and(|descriptor| descriptor.is_canonical_ffi_unit()))
    }

    /// R6-1074: mirror of the verifier's direct owned-String call routing.
    /// `eval_direct_owned_string_call` validates the owned-String return
    /// shape, consumes non-Copy arguments with MoveOut glue, executes the
    /// callee body, and requires exactly one trap-free return path — so an
    /// owned-String callee is exactly modeled in a contract-bearing
    /// function too.  R6-1077: the return-shape ledger now composes one-edge
    /// wrappers (`wrap() { greet() }`) whose callee candidacy it re-proves
    /// recursively, so this mirror follows automatically.  The recursion
    /// guard is load-bearing here (unlike the scalar route, whose evaluator
    /// re-checks it): the checker admits recursive bodies, and without the
    /// guard a contract-bearing caller of a recursive String callee would
    /// send the evaluator into unbounded exploration — the gate keeps that
    /// shape fail-closed.
    fn call_routes_to_direct_owned_string_evaluation(
        &self,
        function: &MirFunction,
        target: &MirFunction,
        result: Option<&MirValueId>,
    ) -> bool {
        let catalog = self.program.type_catalog();
        !crate::verifier::mir::direct_call_uses_managed_any(target, catalog)
            && catalog.validate_owned_string(&target.result).is_ok()
            && crate::core::mir::validate_owned_string_return_shape(
                target,
                self.program.functions(),
                catalog,
            )
            .is_ok()
            && !crate::verifier::mir::direct_call_graph_reaches(
                self.program,
                &target.owner,
                &function.owner,
                &mut BTreeSet::new(),
            )
            && result.is_some()
    }

    fn validate_identity_instance(&mut self, function: &MirFunction, subject: &str) {
        let [parameter] = function.parameters.as_slice() else {
            self.error(format!(
                "instance '{}' identity body has an invalid signature",
                subject
            ));
            return;
        };
        if value_type(function, parameter).as_ref() != Some(&function.result) {
            self.error(format!(
                "instance '{}' identity body has an invalid signature",
                subject
            ));
        }
    }

    fn validate_binary(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        op: ResolvedBinaryOp,
        left: &MirValueId,
        right: &MirValueId,
        subject: &str,
    ) {
        let (Some(result_ty), Some(left_ty), Some(right_ty)) = (
            value_type(function, result),
            value_type(function, left),
            value_type(function, right),
        ) else {
            return;
        };
        let catalog = self.program.type_catalog();
        // The ordinary Map/Any snapshot equality face deliberately keeps the
        // generation-bearing Any owner as one operand and the borrowed String
        // peer as the other.  Their TypeDesc identities are therefore
        // different by construction; the lowerer brands this exact
        // instruction as `binary.managed_any_string_read`, and the ManagedAny
        // owner ledger/Checker seal validates the owner side and its cleanup.
        // Do this narrow check before the scalar same-TypeDesc rule instead
        // of erasing the owner into a guessed String descriptor.
        let managed_any_string_read = subject.contains("binary.managed_any_string_read")
            && matches!(op, ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual)
            && catalog_is_managed_any_string_pair(catalog, &left_ty, &right_ty)
            && catalog.get(&result_ty).is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::Bool)
                    && descriptor.is_canonical_copy_scalar(false)
            });
        if managed_any_string_read {
            return;
        }
        if left_ty != right_ty {
            self.error(format!(
                "{subject} binary operands have different TypeDesc identities"
            ));
            return;
        }
        let snapshot_string_operation = (function
            .ownership
            .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            || crate::verifier::mir::direct_call_uses_managed_any(function, catalog))
            && catalog.validate_owned_string(&left_ty).is_ok();
        if snapshot_string_operation {
            let valid = match op {
                ResolvedBinaryOp::Add => catalog.validate_owned_string(&result_ty).is_ok(),
                ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual => {
                    catalog.get(&result_ty).is_some_and(|desc| {
                        desc.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::Bool)
                            && catalog.validate_copy_scalar(&result_ty).is_ok()
                    })
                }
                _ => false,
            };
            if !valid {
                self.error(format!(
                    "{subject} String operator is outside the exact snapshot proof domain"
                ));
            }
            return;
        }
        let scalar = self.program.type_catalog().get(&left_ty);
        let Some(descriptor) = scalar else {
            return;
        };
        if !descriptor.is_canonical_copy_scalar(true) {
            self.error(format!(
                "{subject} binary operand TypeDesc is outside the complete Copy scalar contract"
            ));
            return;
        }
        let result_descriptor = self.program.type_catalog().get(&result_ty);
        let right_descriptor = self.program.type_catalog().get(&right_ty);
        let float_shape = [Some(descriptor), right_descriptor, result_descriptor]
            .iter()
            .flatten()
            .any(|descriptor| {
                descriptor.is_canonical_copy_scalar(true)
                    && matches!(descriptor.abi, MirAbiClass::Float { bits: 32 | 64 })
            });
        if float_shape {
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_copy_float_binary(&result_ty, &left_ty, &right_ty, op)
            {
                self.error(format!("{subject} float binary rejected: {message}"));
            }
            return;
        }
        let valid = match descriptor.abi {
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true,
            } => matches!(
                op,
                ResolvedBinaryOp::Add
                    | ResolvedBinaryOp::Subtract
                    | ResolvedBinaryOp::Multiply
                    | ResolvedBinaryOp::Divide
                    | ResolvedBinaryOp::Remainder
                    // Power belongs only to the signed i32/i64 Copy arm. The
                    // typed proof term uses the explicit VM/native checked-i64
                    // profile, wrapping only successful i32 results.
                    | ResolvedBinaryOp::Power
                    | ResolvedBinaryOp::Equal
                    | ResolvedBinaryOp::NotEqual
                    | ResolvedBinaryOp::Less
                    | ResolvedBinaryOp::Greater
                    | ResolvedBinaryOp::LessEqual
                    | ResolvedBinaryOp::GreaterEqual
            ),
            MirAbiClass::Bool => matches!(
                op,
                ResolvedBinaryOp::Equal
                    | ResolvedBinaryOp::NotEqual
                    | ResolvedBinaryOp::LogicalAnd
                    | ResolvedBinaryOp::LogicalOr
            ),
            _ => false,
        };
        let power_profile_supported = op != ResolvedBinaryOp::Power
            || matches!(
                POWER_PROFILE_0_1_11_COMPAT,
                CheckedPowerProfile::VmNativeI64ThenI32Wrap
            );
        let result_is_bool =
            self.program
                .type_catalog()
                .get(&result_ty)
                .is_some_and(|descriptor| {
                    descriptor.is_canonical_copy_scalar(false)
                        && descriptor.abi == MirAbiClass::Bool
                });
        let comparison = matches!(
            op,
            ResolvedBinaryOp::Equal
                | ResolvedBinaryOp::NotEqual
                | ResolvedBinaryOp::Less
                | ResolvedBinaryOp::Greater
                | ResolvedBinaryOp::LessEqual
                | ResolvedBinaryOp::GreaterEqual
        );
        if !valid
            || !power_profile_supported
            || (comparison && !result_is_bool)
            || (!comparison && result_ty != left_ty)
        {
            self.error(format!(
                "{subject} binary operator is outside the verifier capability"
            ));
        }
    }

    fn require_same_type_if_unary(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        operand: &MirValueId,
        op: ResolvedUnaryOp,
        subject: &str,
    ) {
        match op {
            ResolvedUnaryOp::Negate | ResolvedUnaryOp::Not => {
                if op == ResolvedUnaryOp::Negate
                    && value_type(function, result) != value_type(function, operand)
                {
                    self.error(format!(
                        "{subject} negate result TypeDesc disagrees with operand"
                    ));
                }
                if op == ResolvedUnaryOp::Negate {
                    let (Some(result_ty), Some(operand_ty)) =
                        (value_type(function, result), value_type(function, operand))
                    else {
                        return;
                    };
                    let is_float =
                        self.program
                            .type_catalog()
                            .get(&operand_ty)
                            .is_some_and(|descriptor| {
                                descriptor.is_canonical_copy_scalar(true)
                                    && matches!(
                                        descriptor.abi,
                                        MirAbiClass::Float { bits: 32 | 64 }
                                    )
                            });
                    if is_float {
                        if let Err(message) = self.program.type_catalog().validate_copy_float_unary(
                            &result_ty,
                            &operand_ty,
                            op,
                        ) {
                            self.error(format!("{subject} float negate rejected: {message}"));
                        }
                        return;
                    }
                }
                if op == ResolvedUnaryOp::Not
                    && !value_type(function, result)
                        .and_then(|ty| self.program.type_catalog().get(&ty))
                        .is_some_and(|descriptor| {
                            descriptor.is_canonical_copy_scalar(false)
                                && descriptor.abi == MirAbiClass::Bool
                        })
                {
                    self.error(format!("{subject} Not result is not a canonical bool"));
                }
            }
            ResolvedUnaryOp::BorrowShared
            | ResolvedUnaryOp::BorrowMutable
            | ResolvedUnaryOp::Dereference => self.error(format!(
                "{subject} unary {op:?} is outside the explicit MIR Borrow/Project capability"
            )),
        }
    }

    fn require_same_type(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        source: &MirValueId,
        subject: &str,
    ) {
        if value_type(function, result) != value_type(function, source) {
            self.error(format!(
                "{subject} result and source TypeDesc identities disagree"
            ));
        }
    }

    fn validate_terminator(
        &mut self,
        function: &MirFunction,
        terminator: &MirTerminator,
        subject: &str,
    ) {
        match terminator {
            MirTerminator::Goto { .. } | MirTerminator::Branch { .. } => {}
            MirTerminator::Switch { scrutinee, arms } => {
                let Some(ty) = value_type(function, scrutinee) else {
                    return;
                };
                if arms
                    .iter()
                    .any(|arm| matches!(arm.case, MirSwitchCase::Literal(_)))
                {
                    // Scalar literal switch (R6-1051): the shared catalog
                    // contract proves the Copy-scalar scrutinee, literal
                    // shapes, and exhaustive coverage before any consumer.
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_scalar_switch(&ty, arms)
                    {
                        self.error(format!("{subject} Switch rejected: {message}"));
                    }
                } else if self.program.type_catalog().variant_layout(&ty).is_none() {
                    self.error(format!(
                        "{subject} Switch is outside the Copy variant verifier capability"
                    ));
                } else if let Err(message) = self.program.type_catalog().validate_switch(&ty, arms)
                {
                    self.error(format!("{subject} Switch rejected: {message}"));
                }
            }
            MirTerminator::SwitchMove { scrutinee, arms } => {
                let Some(scrutinee_ty) = value_type(function, scrutinee) else {
                    self.error(format!("{subject} SwitchMove scrutinee is absent"));
                    return;
                };
                let switch_shape = self
                    .program
                    .type_catalog()
                    .validate_multi_target_union_variant(&scrutinee_ty)
                    .map(|_| scrutinee_ty.clone())
                    .or_else(|_| {
                        self.program
                            .type_catalog()
                            .validate_option_string_variant(&scrutinee_ty)
                    })
                    .or_else(|_| {
                        if self.allow_recoverable_flow_result {
                            self.program
                                .type_catalog()
                                .validate_recoverable_result_variant(&scrutinee_ty)
                                .map(|_| scrutinee_ty.clone())
                        } else {
                            Err(
                                "recoverable Flow Result switch is outside this verifier profile"
                                    .into(),
                            )
                        }
                    })
                    .or_else(|_| {
                        if self.allow_nested_option_tuple_variant {
                            self.program
                                .type_catalog()
                                .validate_option_nested_tuple_variant(&scrutinee_ty)
                                .map(|_| scrutinee_ty.clone())
                        } else {
                            Err(
                                "nested Option tuple switch is outside this verifier profile"
                                    .into(),
                            )
                        }
                    })
                    .or_else(|_| {
                        if self
                            .ordinary_map_owned_get_result_string_types
                            .contains(&scrutinee_ty)
                        {
                            self.program
                                .type_catalog()
                                .validate_result_string_i32_variant(&scrutinee_ty)
                                .map(|_| scrutinee_ty.clone())
                        } else {
                            Err("owned Map getter Result switch is outside this profile".into())
                        }
                    })
                    .or_else(|_| {
                        if self.allow_result_move_variant {
                            self.program
                                .type_catalog()
                                .validate_result_move_projection_variant(&scrutinee_ty)
                                .map(|_| scrutinee_ty.clone())
                        } else {
                            Err("move-owned Result switch is outside this verifier profile".into())
                        }
                    })
                    .or_else(|_| {
                        if self.has_values_local_index_i32_result_branch_receipt(function)
                            && scrutinee_ty
                                == crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id()
                        {
                            self.validate_values_local_index_i32_result_branch_result_type(
                                &scrutinee_ty,
                            )
                            .map(|_| scrutinee_ty.clone())
                        } else {
                            Err(
                                "ValuesLocalIndexI32 synthetic Result switch is outside its exact Checker-bound branch profile".into(),
                            )
                        }
                    });
                if let Err(message) = switch_shape {
                    self.error(format!("{subject} SwitchMove rejected: {message}"));
                    return;
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_variant_switch_move_contract(&scrutinee_ty, arms)
                {
                    self.error(format!("{subject} SwitchMove rejected: {message}"));
                    return;
                }
                let Some((_, variants)) = self.program.type_catalog().variant_layout(&scrutinee_ty)
                else {
                    self.error(format!(
                        "{subject} SwitchMove has no canonical variant layout"
                    ));
                    return;
                };
                let required = variants
                    .iter()
                    .map(|variant| variant.id.clone())
                    .collect::<BTreeSet<_>>();
                let mut seen = BTreeSet::new();
                if arms.len() != required.len() {
                    self.error(format!(
                        "{subject} SwitchMove requires exactly one explicit arm for each TypeDesc variant"
                    ));
                }
                for arm in arms {
                    let MirSwitchCase::Variant(variant_id) = &arm.case else {
                        self.error(format!(
                            "{subject} SwitchMove requires explicit variant arms; default/literal cases are not covered"
                        ));
                        continue;
                    };
                    let Some(variant) = self
                        .program
                        .type_catalog()
                        .variant(&scrutinee_ty, variant_id)
                    else {
                        self.error(format!(
                            "{subject} SwitchMove variant '{}' is absent from TypeDesc",
                            variant_id.0
                        ));
                        continue;
                    };
                    if !seen.insert(variant.id.clone()) {
                        self.error(format!(
                            "{subject} SwitchMove variant '{}' is repeated",
                            variant.name
                        ));
                    }
                    let Some(target) = function.blocks.get(&arm.target) else {
                        self.error(format!(
                            "{subject} SwitchMove edge target '{}' is absent",
                            arm.target
                        ));
                        continue;
                    };
                    for (index, argument) in arm.arguments.iter().enumerate() {
                        if !function.values.contains_key(argument) {
                            self.error(format!(
                                "{subject} SwitchMove edge argument '{}' is absent",
                                argument
                            ));
                            continue;
                        }
                        let Some(parameter) = target
                            .parameters
                            .get(index)
                            .and_then(|parameter| function.values.get(&parameter.value))
                        else {
                            continue;
                        };
                        if function
                            .values
                            .get(argument)
                            .is_some_and(|value| value.ty != parameter.ty)
                        {
                            self.error(format!(
                                "{subject} SwitchMove edge argument type disagrees with block parameter"
                            ));
                        }
                    }
                    if target.parameters.len() != arm.arguments.len() + arm.bindings.len() {
                        self.error(format!(
                            "{subject} SwitchMove edge arguments and payload bindings disagree with block parameter arity"
                        ));
                    }
                    // A nested tuple receipt intentionally projects more than
                    // one leaf from the same outer Option::Some payload field.
                    // Keep duplicate detection at receipt granularity; a
                    // plain payload projection remains exactly-once.
                    let mut binding_fields = BTreeSet::new();
                    for (index, binding) in arm.bindings.iter().enumerate() {
                        let binding_key = (
                            binding.projection.field.clone(),
                            binding
                                .nested_tuple
                                .as_ref()
                                .map(|nested| nested.field_index),
                        );
                        if !binding_fields.insert(binding_key) {
                            self.error(format!(
                                "{subject} SwitchMove payload field '{}' is bound more than once",
                                binding.projection.field.0
                            ));
                        }
                        let Some(target_parameter) =
                            target.parameters.get(arm.arguments.len() + index)
                        else {
                            continue;
                        };
                        let Some(parameter) = function.values.get(&target_parameter.value) else {
                            continue;
                        };
                        if binding.parameter != target_parameter.value {
                            self.error(format!(
                                "{subject} SwitchMove binding parameter disagrees with target block parameter"
                            ));
                        }
                        let validation = if let Some(nested) = &binding.nested_tuple {
                            self.program
                                .type_catalog()
                                .validate_variant_nested_tuple_payload_projection_receipt(
                                    &scrutinee_ty,
                                    variant_id,
                                    &parameter.ty,
                                    &binding.projection,
                                    nested,
                                )
                        } else {
                            self.program
                                .type_catalog()
                                .validate_variant_payload_projection_receipt(
                                    &scrutinee_ty,
                                    variant_id,
                                    &parameter.ty,
                                    &binding.projection,
                                )
                        };
                        if let Err(message) = validation {
                            self.error(format!("{subject} SwitchMove rejected: {message}"));
                        }
                    }
                }
            }
            MirTerminator::Return { .. } | MirTerminator::Trap { .. } => {}
            MirTerminator::Fault { .. } => {
                self.error(format!(
                    "{subject} Fault is outside the verifier capability"
                ));
            }
            MirTerminator::Unreachable => {
                self.error(format!(
                    "{subject} Unreachable is outside the default verifier island"
                ));
            }
        }
    }

    fn validate_acyclic_cfg(&mut self, function: &MirFunction) {
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut certified_back_edges = BTreeSet::new();
        if let Some(worklist) = &function.proof_worklist {
            fn collect(
                block: &crate::core::mir::MirProofBlock,
                function: &MirFunction,
                allow_ffi_call_path: bool,
                edges: &mut BTreeSet<(crate::core::mir::MirBlockId, crate::core::mir::MirBlockId)>,
            ) {
                for operation in &block.operations {
                    match operation {
                        crate::core::mir::MirProofOperation::Loop {
                            header,
                            body,
                            receipt: Some(receipt),
                            ..
                        } if allow_ffi_call_path && receipt.typed.ffi_call_path.is_some() => {
                            if ffi_call_path_receipt_has_exact_terminal_call(
                                function, header, receipt,
                            ) {
                                for edge in &receipt.edges {
                                    let Some(target) = edge.to_block.as_ref() else {
                                        continue;
                                    };
                                    let Some(mir_edge) = edge.mir_edge.as_ref() else {
                                        continue;
                                    };
                                    if target == header
                                        && matches!(
                                            edge.kind,
                                            MirProofLoopEdgeKind::BodyFallthrough
                                                | MirProofLoopEdgeKind::Continue
                                        )
                                        && function.blocks.get(&edge.from_block).is_some_and(
                                            |from| {
                                                matches!(
                                                    &from.terminator,
                                                    MirTerminator::Goto {
                                                        edge: actual_edge,
                                                        target: actual_target,
                                                        ..
                                                    } if actual_edge == mir_edge
                                                        && actual_target == header
                                                )
                                            },
                                        )
                                    {
                                        edges.insert((edge.from_block.clone(), header.clone()));
                                    }
                                }
                            }
                            collect(body, function, allow_ffi_call_path, edges);
                        }
                        crate::core::mir::MirProofOperation::Loop {
                            header,
                            body_entry,
                            body,
                            stationary: true,
                            receipt,
                            ..
                        } => {
                            // A valid exact FFI profile is handled above so
                            // its guarded Continue/BodyFallthrough edges are
                            // admitted individually. Do not let a forged or
                            // unvalidated profile fall through to this wider
                            // stationary-loop shortcut.
                            if receipt
                                .as_ref()
                                .is_none_or(|receipt| receipt.typed.ffi_call_path.is_none())
                            {
                                edges.insert((body_entry.clone(), header.clone()));
                            }
                            collect(body, function, allow_ffi_call_path, edges);
                        }
                        crate::core::mir::MirProofOperation::Loop {
                            header,
                            body,
                            stationary: false,
                            receipt: Some(receipt),
                            ..
                        } => {
                            for edge in &receipt.edges {
                                if matches!(
                                    edge.kind,
                                    MirProofLoopEdgeKind::BodyFallthrough
                                        | MirProofLoopEdgeKind::Continue
                                ) && edge.to_block.as_ref() == Some(header)
                                    && edge.mir_edge.is_some()
                                    && function.blocks.get(&edge.from_block).is_some_and(|from| {
                                        matches!(
                                            &from.terminator,
                                            MirTerminator::Goto {
                                                edge: actual_edge,
                                                target: actual_target,
                                                ..
                                            } if Some(actual_edge) == edge.mir_edge.as_ref()
                                                && actual_target == header
                                        )
                                    })
                                {
                                    edges.insert((edge.from_block.clone(), header.clone()));
                                }
                            }
                            collect(body, function, allow_ffi_call_path, edges);
                        }
                        crate::core::mir::MirProofOperation::Branch {
                            then_block,
                            else_block,
                            ..
                        } => {
                            collect(then_block, function, allow_ffi_call_path, edges);
                            collect(else_block, function, allow_ffi_call_path, edges);
                        }
                        crate::core::mir::MirProofOperation::Scope(inner) => {
                            collect(inner, function, allow_ffi_call_path, edges)
                        }
                        crate::core::mir::MirProofOperation::Bind { .. }
                        | crate::core::mir::MirProofOperation::Call(_)
                        | crate::core::mir::MirProofOperation::Eval { .. }
                        | crate::core::mir::MirProofOperation::Assign { .. }
                        | crate::core::mir::MirProofOperation::Math(_)
                        | crate::core::mir::MirProofOperation::Break { .. }
                        | crate::core::mir::MirProofOperation::Continue { .. }
                        | crate::core::mir::MirProofOperation::Return { .. }
                        | crate::core::mir::MirProofOperation::Loop { .. }
                        | crate::core::mir::MirProofOperation::Exit => {}
                    }
                }
            }
            collect(
                &worklist.root,
                function,
                self.allow_typed_ffi_verifier_proofs,
                &mut certified_back_edges,
            );
        }
        for block in function.blocks.keys() {
            if !visited.contains(block)
                && self.visit_cfg(
                    function,
                    block,
                    &mut visiting,
                    &mut visited,
                    &certified_back_edges,
                )
            {
                self.error(format!(
                    "function '{}' contains a cyclic CFG outside the verifier capability",
                    function.owner.0
                ));
                return;
            }
        }
    }

    fn visit_cfg(
        &self,
        function: &MirFunction,
        block: &crate::core::mir::MirBlockId,
        visiting: &mut BTreeSet<crate::core::mir::MirBlockId>,
        visited: &mut BTreeSet<crate::core::mir::MirBlockId>,
        certified_back_edges: &BTreeSet<(
            crate::core::mir::MirBlockId,
            crate::core::mir::MirBlockId,
        )>,
    ) -> bool {
        if visited.contains(block) {
            return false;
        }
        visiting.insert(block.clone());
        let Some(block_data) = function.blocks.get(block) else {
            visiting.remove(block);
            return false;
        };
        let targets = match &block_data.terminator {
            MirTerminator::Goto { target, .. } => vec![target],
            MirTerminator::Branch {
                then_target,
                else_target,
                ..
            } => {
                vec![then_target, else_target]
            }
            MirTerminator::Switch { arms, .. } | MirTerminator::SwitchMove { arms, .. } => {
                arms.iter().map(|arm| &arm.target).collect()
            }
            MirTerminator::Return { .. }
            | MirTerminator::Trap { .. }
            | MirTerminator::Fault { .. }
            | MirTerminator::Unreachable => Vec::new(),
        };
        for target in targets {
            if visiting.contains(target) {
                if !certified_back_edges.contains(&(block.clone(), target.clone())) {
                    return true;
                }
            } else if !visited.contains(target)
                && self.visit_cfg(function, target, visiting, visited, certified_back_edges)
            {
                return true;
            }
        }
        visiting.remove(block);
        visited.insert(block.clone());
        false
    }

    fn error(&mut self, message: String) {
        self.errors.insert(message);
    }
}

fn validate_ffi_capability_preflight(
    program: &MirProgram,
    allow_typed_ffi_verifier_proofs: bool,
) -> Vec<String> {
    let has_extern_call = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Call {
                        callee: ResolvedCallee::Extern(_),
                        ..
                    }
                )
            })
        })
    });
    if !has_extern_call && program.ffi_calls().is_empty() {
        return Vec::new();
    }
    let mut errors = std::collections::BTreeSet::new();
    for message in
        crate::core::mir::validate_ffi_receipt_table(program.functions(), program.ffi_calls())
    {
        errors.insert(message);
    }
    for message in crate::core::mir::validate_ffi_symbol_declaration_shapes(program.ffi_calls()) {
        errors.insert(message);
    }
    for function in program.functions().values() {
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                if let MirInstructionKind::Call {
                    result,
                    callee: ResolvedCallee::Extern(owner),
                    arguments,
                    ..
                } = &instruction.kind
                {
                    validate_extern_call_capabilities(
                        program,
                        allow_typed_ffi_verifier_proofs,
                        function,
                        &instruction.id,
                        result.as_ref(),
                        owner,
                        arguments,
                        instruction.id.as_str(),
                        |message| {
                            errors.insert(message);
                        },
                    );
                }
            }
        }
    }
    errors.into_iter().collect()
}

#[allow(clippy::too_many_arguments)]
fn validate_extern_call_capabilities(
    program: &MirProgram,
    allow_typed_ffi_verifier_proofs: bool,
    function: &MirFunction,
    instruction_id: &crate::core::mir::MirInstructionId,
    result: Option<&MirValueId>,
    owner: &crate::core::NodeId,
    arguments: &[MirValueId],
    subject: &str,
    mut record_error: impl FnMut(String),
) {
    let Some(contract) = program.ffi_calls().get(instruction_id) else {
        record_error(format!(
            "{subject} extern callee '{}' has no canonical FFI contract",
            owner.0
        ));
        return;
    };
    let catalog = program.type_catalog();
    let has_string_endpoint = contract
        .parameter_types
        .iter()
        .take(
            contract
                .fixed_argument_count
                .min(contract.parameter_types.len()),
        )
        .any(|ty| catalog.validate_owned_string(ty).is_ok())
        || (result.is_some() && catalog.validate_owned_string(&contract.result_type).is_ok());
    let is_record_candidate = |ty: &crate::core::ResolvedTypeId| {
        catalog.get(ty).is_some_and(|descriptor| {
            matches!(
                &descriptor.layout,
                crate::core::mir::types::MirLayout::Record { .. }
            )
        })
    };
    let has_record_endpoint = contract
        .parameter_types
        .iter()
        .take(
            contract
                .fixed_argument_count
                .min(contract.parameter_types.len()),
        )
        .any(is_record_candidate)
        || (result.is_some() && is_record_candidate(&contract.result_type));
    let uses_scalar_requires_policy =
        crate::core::mir::ffi_call_uses_scalar_requires_verifier_policy(contract);
    let receipt_errors = if allow_typed_ffi_verifier_proofs && has_record_endpoint {
        crate::core::mir::validate_ffi_call_contract_verifier_record_receipt(
            catalog,
            function,
            instruction_id,
            owner,
            result,
            arguments,
            contract,
        )
    } else if allow_typed_ffi_verifier_proofs && uses_scalar_requires_policy {
        crate::core::mir::validate_ffi_call_contract_verifier_scalar_requires_receipt(
            catalog,
            function,
            instruction_id,
            owner,
            result,
            arguments,
            contract,
        )
    } else if allow_typed_ffi_verifier_proofs && has_string_endpoint {
        crate::core::mir::validate_ffi_call_contract_verifier_receipt(
            catalog,
            function,
            instruction_id,
            owner,
            result,
            arguments,
            contract,
        )
    } else if allow_typed_ffi_verifier_proofs {
        crate::core::mir::validate_ffi_call_contract_scalar_verifier_receipt(
            catalog,
            function,
            instruction_id,
            owner,
            result,
            arguments,
            contract,
        )
    } else {
        crate::core::mir::validate_ffi_call_contract_receipt(
            catalog,
            function,
            instruction_id,
            owner,
            result,
            arguments,
            contract,
        )
    };
    for message in &receipt_errors {
        record_error(format!("{subject} {message}"));
    }
    if !receipt_errors.is_empty() {
        return;
    }
    if result.is_none() {
        record_error(format!(
            "{subject} extern call has no canonical result value identity"
        ));
    }
}

fn catalog_is_managed_any_string_pair(
    catalog: &MirTypeCatalog,
    left: &ResolvedTypeId,
    right: &ResolvedTypeId,
) -> bool {
    (catalog.is_managed_any_i32_string_v1(left) && catalog.validate_owned_string(right).is_ok())
        || (catalog.is_managed_any_i32_string_v1(right)
            && catalog.validate_owned_string(left).is_ok())
}

fn value_type(function: &MirFunction, value: &MirValueId) -> Option<crate::core::ResolvedTypeId> {
    function.values.get(value).map(|value| value.ty.clone())
}

fn is_map_root_operation(instruction: &MirInstructionKind) -> bool {
    matches!(
        instruction,
        MirInstructionKind::MapRootNew { .. }
            | MirInstructionKind::MapRootSet { .. }
            | MirInstructionKind::MapRootRemove { .. }
            | MirInstructionKind::MapRootSize { .. }
            | MirInstructionKind::MapRootContains { .. }
            | MirInstructionKind::MapRootDrop { .. }
    )
}

/// Validate the exact list payload that the owned-get `values()` instruction
/// publishes.  A generic List TypeDesc is insufficient: every element is an
/// independently owned ManagedAny snapshot and the List itself must carry the
/// complete move/clone/drop contract consumed by the four MIR backends.
fn is_canonical_managed_any_list_type(catalog: &MirTypeCatalog, ty: &ResolvedTypeId) -> bool {
    let Some(descriptor) = catalog.get(ty) else {
        return false;
    };
    let MirLayout::List { element } = &descriptor.layout else {
        return false;
    };
    descriptor.kind == MirTypeKind::List
        && descriptor.abi == MirAbiClass::OpaqueHandle
        && descriptor.ownership == MirOwnership::Move
        && descriptor.needs_drop_glue
        && descriptor.needs_clone_glue
        && descriptor.session_protocol.is_none()
        && descriptor.drop_plan.is_none()
        && descriptor.variant_drop_plan.is_none()
        && descriptor.glue
            == (crate::core::mir::types::MirGlueContract {
                move_out: MirGlueKind::List,
                clone: MirGlueKind::List,
                drop: MirGlueKind::List,
            })
        && catalog.is_managed_any_i32_string_v1(element)
}

fn instruction_touches_ordinary_map_owner(instruction: &MirInstructionKind) -> bool {
    matches!(
        instruction,
        MirInstructionKind::OrdinaryMapOwnerLifecycleNew { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleSet { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleSize { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleContains { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln { .. }
    )
}

fn instruction_touches_ordinary_map_owned_any_v2(instruction: &MirInstructionKind) -> bool {
    matches!(
        instruction,
        MirInstructionKind::OrdinaryMapOwnedAnyV2New { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2SetAnyResult { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2SetStringResult { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetStringResult { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Size { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultString { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Values { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2DynamicArg0 { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { .. }
    )
}

fn ordinary_map_owned_any_v2_values_borrow_clone_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    result: &MirValueId,
    source: &MirValueId,
    allow_values_producer_caller: bool,
) -> bool {
    let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
    if function.values.get(result).map(|value| &value.ty) != Some(&values_ty)
        || function.values.get(source).map(|value| &value.ty) != Some(&values_ty)
    {
        return false;
    }
    let local_receipted = ordinary_map_owned_any_v2_values_result_receipts_for_graph(program)
        .is_some_and(|receipts| {
            receipts.iter().any(|receipt| {
                receipt.owner == function.owner
                    && receipt.binding == *source
                    && function.blocks.values().any(|block| {
                        block.instructions.iter().any(|instruction| {
                            matches!(
                                &instruction.kind,
                                MirInstructionKind::Clone {
                                    result: clone_result,
                                    source: clone_source,
                                } if clone_result == result && clone_source == source
                            )
                        })
                    })
            })
        });
    let caller_bridge_receipted = allow_values_producer_caller
        && ordinary_map_owned_any_v2_values_producer_caller_bridge_for_caller(program, function)
            .is_some_and(|receipt| {
                receipt.caller_binding == *source
                    && receipt.len_receiver.as_ref() == Some(result)
                    && values_clone_instruction_is_exact(
                        function,
                        result,
                        source,
                        receipt.len_receiver.as_ref(),
                    )
            });
    local_receipted || caller_bridge_receipted
}

fn ordinary_map_owned_any_v2_values_result_receipts_for_graph(
    program: &MirProgram,
) -> Option<Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesResultReceipt>> {
    let bridges = program
        .ordinary_map_owned_any_v2_values_producer_caller_receipts()
        .ok()?;
    program
        .ordinary_map_owned_any_v2_values_result_receipts_excluding_producer_caller(&bridges)
        .ok()
}

fn ordinary_map_owned_any_v2_values_producer_caller_bridge_for_caller(
    program: &MirProgram,
    function: &MirFunction,
) -> Option<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesProducerCallerMirReceipt> {
    let receipts = program
        .ordinary_map_owned_any_v2_values_producer_caller_receipts()
        .ok()?;
    let matching = receipts
        .iter()
        .filter(|receipt| receipt.caller_owner == function.owner)
        .collect::<Vec<_>>();
    let [receipt] = matching.as_slice() else {
        return None;
    };
    if !ordinary_map_owned_any_v2_values_producer_caller_bridge_is_exact(program, receipt) {
        return None;
    }
    Some((*receipt).clone())
}

fn ordinary_map_owned_any_v2_values_producer_caller_bridge_for_producer(
    program: &MirProgram,
    function: &MirFunction,
) -> Option<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesProducerCallerMirReceipt> {
    let receipts = program
        .ordinary_map_owned_any_v2_values_producer_caller_receipts()
        .ok()?;
    let matching = receipts
        .iter()
        .filter(|receipt| receipt.producer_owner == function.owner)
        .collect::<Vec<_>>();
    let [receipt] = matching.as_slice() else {
        return None;
    };
    if !ordinary_map_owned_any_v2_values_producer_caller_bridge_is_exact(program, receipt) {
        return None;
    }
    Some((*receipt).clone())
}

fn ordinary_map_owned_any_v2_values_producer_caller_bridge_is_exact(
    program: &MirProgram,
    receipt: &crate::core::mir::OrdinaryMapOwnedAnyV2ValuesProducerCallerMirReceipt,
) -> bool {
    let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
    let i32_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    if !receipt.unpacks.is_empty()
        || receipt.validate_against_program(program).is_err()
        || receipt.owner_type != values_ty
        || receipt.producer_result_type != values_ty
        || receipt.call_result_type != values_ty
    {
        return false;
    }
    let Some(producer) = program.functions().get(&receipt.producer_owner) else {
        return false;
    };
    let Some(caller) = program.functions().get(&receipt.caller_owner) else {
        return false;
    };
    if producer.result != values_ty
        || producer
            .values
            .get(&receipt.producer_values_result)
            .map(|value| &value.ty)
            != Some(&values_ty)
        || producer
            .values
            .get(&receipt.producer_snapshot_value)
            .map(|value| &value.ty)
            != Some(&values_ty)
        || producer
            .values
            .get(&receipt.producer_return_value)
            .map(|value| &value.ty)
            != Some(&values_ty)
        || caller
            .values
            .get(&receipt.call_result)
            .map(|value| &value.ty)
            != Some(&values_ty)
        || caller
            .values
            .get(&receipt.caller_binding)
            .map(|value| &value.ty)
            != Some(&values_ty)
    {
        return false;
    }
    match (
        receipt.len_instruction.as_ref(),
        receipt.len_result.as_ref(),
        receipt.len_receiver.as_ref(),
        receipt.len_receiver_type.as_ref(),
    ) {
        (None, None, None, None) => true,
        (Some(_), Some(len_result), Some(len_receiver), Some(len_receiver_type)) => {
            len_receiver_type == &values_ty
                && caller.values.get(len_receiver).map(|value| &value.ty) == Some(&values_ty)
                && caller.values.get(len_result).map(|value| &value.ty) == Some(&i32_ty)
                && ordinary_map_owned_any_v2_values_len_receiver_instructions_are_exact(
                    caller, receipt,
                )
        }
        _ => false,
    }
}

fn ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_caller(
    program: &MirProgram,
    function: &MirFunction,
) -> Option<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt> {
    ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_owner(
        program,
        &function.owner,
        false,
    )
}

fn ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_producer(
    program: &MirProgram,
    function: &MirFunction,
) -> Option<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt> {
    ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_owner(
        program,
        &function.owner,
        true,
    )
}

fn ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_owner(
    program: &MirProgram,
    owner: &crate::core::NodeId,
    producer: bool,
) -> Option<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt> {
    let receipts = program
        .ordinary_map_owned_any_v2_values_crosscall_string_caller_receipts()
        .ok()?;
    if receipts.len() != 1 {
        return None;
    }
    let receipt = receipts.first()?;
    let receipt_owner = if producer {
        &receipt.producer_owner
    } else {
        &receipt.caller_owner
    };
    if receipt_owner != owner
        || super::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_type_owner_bridge(
            program,
            receipt,
        )
        .is_err()
    {
        return None;
    }
    Some(receipt.clone())
}

fn ordinary_map_owned_any_v2_values_result_crosscall_string_caller_bridge_for_owner(
    program: &MirProgram,
    owner: &crate::core::NodeId,
    producer: bool,
    expected_version: crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
) -> Option<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCallerMirReceipt> {
    use crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion as Version;

    if !matches!(expected_version, Version::V3 | Version::V4) {
        return None;
    }
    let receipts = program
        .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_receipts()
        .ok()?;
    let [receipt] = receipts.as_slice() else {
        return None;
    };
    let receipt_owner = if producer {
        &receipt.producer_owner
    } else {
        &receipt.caller_owner
    };
    if receipt_owner != owner
        || receipt.checker_caller.profile_version != expected_version
        || super::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_type_owner_bridge(
            program,
            receipt,
        )
        .is_err()
    {
        return None;
    }
    Some(receipt.clone())
}

fn ordinary_map_owned_any_v2_values_result_crosscall_string_caller_move_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    result: &MirValueId,
    source: &MirValueId,
    expected_version: crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
) -> bool {
    let producer = ordinary_map_owned_any_v2_values_result_crosscall_string_caller_bridge_for_owner(
        program,
        &function.owner,
        true,
        expected_version,
    );
    if let Some(receipt) = producer {
        return (receipt.producer_values_result == *source && receipt.producer_snapshot == *result)
            || (receipt.producer_snapshot == *source && receipt.producer_return_value == *result);
    }
    ordinary_map_owned_any_v2_values_result_crosscall_string_caller_bridge_for_owner(
        program,
        &function.owner,
        false,
        expected_version,
    )
    .is_some_and(|receipt| {
        receipt.caller_call_result == *source && receipt.caller_snapshot == *result
    })
}

fn ordinary_map_owned_any_v2_values_result_crosscall_string_caller_unpack_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    subject: &str,
    result: &MirValueId,
    values: &MirValueId,
    index: &MirValueId,
    expected_version: crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
) -> bool {
    ordinary_map_owned_any_v2_values_result_crosscall_string_caller_bridge_for_owner(
        program,
        &function.owner,
        false,
        expected_version,
    )
    .is_some_and(|receipt| {
        receipt.caller_unpack_instruction.as_str() == subject
            && receipt.caller_unpack_result == *result
            && receipt.caller_snapshot == *values
            && receipt.caller_index == *index
            && function
                .values
                .get(result)
                .is_some_and(|value| value.ty == receipt.string_type)
            && function
                .values
                .get(values)
                .is_some_and(|value| value.ty == receipt.values_type)
    })
}

fn ordinary_map_owned_any_v2_values_result_crosscall_string_caller_snapshot_drop_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    subject: &str,
    value: &MirValueId,
    expected_version: crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
) -> bool {
    ordinary_map_owned_any_v2_values_result_crosscall_string_caller_bridge_for_owner(
        program,
        &function.owner,
        false,
        expected_version,
    )
    .is_some_and(|receipt| {
        receipt.caller_snapshot == *value
            && receipt.caller_snapshot_drop_instruction.as_str() == subject
    })
}

fn ordinary_map_owned_any_v2_values_result_crosscall_string_caller_string_drop_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    subject: &str,
    value: &MirValueId,
    expected_version: crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
) -> bool {
    ordinary_map_owned_any_v2_values_result_crosscall_string_caller_bridge_for_owner(
        program,
        &function.owner,
        false,
        expected_version,
    )
    .is_some_and(|receipt| {
        receipt.caller_string == *value
            && receipt.caller_string_drop_instruction.as_str() == subject
    })
}

fn ordinary_map_owned_any_v2_values_result_crosscall_string_caller_set_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    subject: &str,
    expected_version: crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
) -> bool {
    ordinary_map_owned_any_v2_values_result_crosscall_string_caller_bridge_for_owner(
        program,
        &function.owner,
        true,
        expected_version,
    )
    .is_some_and(|receipt| {
        receipt
            .producer_set_entries
            .iter()
            .any(|entry| entry.set_instruction.as_str() == subject)
    })
}

fn ordinary_map_owned_any_v2_values_crosscall_string_caller_move_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    result: &MirValueId,
    source: &MirValueId,
) -> bool {
    let Some(receipt) =
        ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_producer(
            program, function,
        )
    else {
        return ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_caller(
            program, function,
        )
        .is_some_and(|receipt| {
            receipt.caller_call_result == *source && receipt.caller_snapshot == *result
        });
    };
    (receipt.producer_values_result == *source && receipt.producer_snapshot == *result)
        || (receipt.producer_snapshot == *source && receipt.producer_return_value == *result)
}

fn ordinary_map_owned_any_v2_values_crosscall_string_caller_unpack_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    subject: &str,
    result: &MirValueId,
    values: &MirValueId,
    index: &MirValueId,
    projection: &crate::core::NodeId,
) -> bool {
    let Some(receipt) = ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_caller(
        program, function,
    ) else {
        return false;
    };
    let string_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let i32_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    receipt.caller_unpack_instruction.as_str() == subject
        && receipt.caller_unpack_result == *result
        && receipt.caller_snapshot == *values
        && receipt.caller_index_zero == *index
        && receipt.checker_caller.index_projection == *projection
        && function
            .values
            .get(result)
            .is_some_and(|value| value.ty == string_ty)
        && function
            .values
            .get(values)
            .is_some_and(|value| value.ty == receipt.values_type)
        && function
            .values
            .get(index)
            .is_some_and(|value| value.ty == i32_ty)
}

fn ordinary_map_owned_any_v2_values_crosscall_string_caller_snapshot_drop_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    subject: &str,
    value: &MirValueId,
) -> bool {
    ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_caller(program, function)
        .is_some_and(|receipt| {
            receipt.caller_snapshot == *value
                && receipt.caller_snapshot_drop_instruction.as_str() == subject
        })
}

fn ordinary_map_owned_any_v2_values_crosscall_string_caller_string_drop_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    subject: &str,
    value: &MirValueId,
) -> bool {
    ordinary_map_owned_any_v2_values_crosscall_string_caller_bridge_for_caller(program, function)
        .is_some_and(|receipt| {
            receipt.caller_string == *value
                && receipt.caller_string_drop_instruction.as_str() == subject
        })
}

fn ordinary_map_owned_any_v2_values_len_receiver_instructions_are_exact(
    function: &MirFunction,
    receipt: &crate::core::mir::OrdinaryMapOwnedAnyV2ValuesProducerCallerMirReceipt,
) -> bool {
    let (Some(len_instruction), Some(len_result), Some(len_receiver)) = (
        receipt.len_instruction.as_ref(),
        receipt.len_result.as_ref(),
        receipt.len_receiver.as_ref(),
    ) else {
        return false;
    };
    let len_matches = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| instruction.id == *len_instruction)
        .collect::<Vec<_>>();
    let [len_instruction] = len_matches.as_slice() else {
        return false;
    };
    if !matches!(
        &len_instruction.kind,
        MirInstructionKind::ListOp {
            result,
            operation: crate::core::mir::MirListOperation::Len,
            list,
            argument: None,
            list_operation_contract: Some(_),
        } if result == len_result && list == len_receiver
    ) {
        return false;
    }
    let Some(receiver_node) = len_receiver.as_str().strip_prefix("expr:") else {
        return false;
    };
    let Some(len) = receipt.checker_caller.len_observation.as_ref() else {
        return false;
    };
    if receiver_node != len.receiver_node.0
        || len_instruction.id.as_str() != format!("inst:list_op:{}", len.call_node.0)
        || len_result.as_str() != format!("expr:{}", len.call_node.0)
    {
        return false;
    }
    let clone_id = format!("inst:clone:{receiver_node}");
    // The borrow Clone belongs to the receiver expression; its temporary
    // settlement belongs to the checked Len call which borrowed that clone.
    let receiver_drop_id = format!("inst:drop.list-borrow:{}", len.call_node.0);
    let clone_matches = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| instruction.id.as_str() == clone_id)
        .collect::<Vec<_>>();
    let receiver_drops = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| {
            matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == len_receiver)
        })
        .collect::<Vec<_>>();
    let [clone] = clone_matches.as_slice() else {
        return false;
    };
    matches!(
        &clone.kind,
        MirInstructionKind::Clone { result, source }
            if result == len_receiver && source == &receipt.caller_binding
    ) && matches!(receiver_drops.as_slice(), [drop] if drop.id.as_str() == receiver_drop_id)
}

fn values_clone_instruction_is_exact(
    function: &MirFunction,
    result: &MirValueId,
    source: &MirValueId,
    expected_receipt_receiver: Option<&MirValueId>,
) -> bool {
    let Some(node) = result.as_str().strip_prefix("expr:") else {
        return false;
    };
    if expected_receipt_receiver.is_some_and(|receiver| receiver != result) {
        return false;
    }
    let expected_id = format!("inst:clone:{node}");
    let matches = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| instruction.id.as_str() == expected_id)
        .collect::<Vec<_>>();
    matches!(matches.as_slice(), [instruction] if matches!(
        &instruction.kind,
        MirInstructionKind::Clone { result: clone_result, source: clone_source }
            if clone_result == result && clone_source == source
    ))
}

fn ordinary_map_owned_any_v2_values_local_list_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    value: &MirValueId,
) -> bool {
    let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
    if function.values.get(value).map(|value| &value.ty) != Some(&values_ty) {
        return false;
    }
    ordinary_map_owned_any_v2_values_result_receipts_for_graph(program).is_some_and(|receipts| {
        receipts.iter().any(|receipt| {
            receipt.owner == function.owner
                && (receipt.binding == *value
                    || ordinary_map_owned_any_v2_values_borrow_clone_is_receipted(
                        program,
                        function,
                        value,
                        &receipt.binding,
                        false,
                    ))
        })
    })
}

fn ordinary_map_owned_any_v2_values_list_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    value: &MirValueId,
    allow_values_producer_caller: bool,
) -> bool {
    if ordinary_map_owned_any_v2_values_local_list_is_receipted(program, function, value) {
        return true;
    }
    if !allow_values_producer_caller {
        return false;
    }
    ordinary_map_owned_any_v2_values_producer_caller_bridge_for_caller(program, function)
        .is_some_and(|receipt| {
            receipt.caller_binding == *value
                || (receipt.len_receiver.as_ref() == Some(value)
                    && ordinary_map_owned_any_v2_values_len_receiver_instructions_are_exact(
                        function, &receipt,
                    ))
        })
}

fn ordinary_map_owned_any_v2_values_producer_caller_len_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    subject: &str,
    result: &MirValueId,
    list: &MirValueId,
    argument: &Option<MirValueId>,
) -> bool {
    if argument.is_some() {
        return false;
    }
    ordinary_map_owned_any_v2_values_producer_caller_bridge_for_caller(program, function)
        .is_some_and(|receipt| {
            receipt
                .len_instruction
                .as_ref()
                .is_some_and(|instruction| instruction.as_str() == subject)
                && receipt.len_result.as_ref() == Some(result)
                && receipt.len_receiver.as_ref() == Some(list)
                && ordinary_map_owned_any_v2_values_len_receiver_instructions_are_exact(
                    function, &receipt,
                )
        })
}

fn ordinary_map_owned_any_v2_values_producer_caller_drop_is_receipted(
    program: &MirProgram,
    function: &MirFunction,
    subject: &str,
    value: &MirValueId,
) -> bool {
    ordinary_map_owned_any_v2_values_producer_caller_bridge_for_caller(program, function)
        .is_some_and(|receipt| {
            if receipt.caller_binding == *value {
                return receipt.owner_drop_value == *value
                    && receipt.owner_drop_instruction.as_str() == subject;
            }
            receipt.len_receiver.as_ref() == Some(value)
                && ordinary_map_owned_any_v2_values_len_receiver_instructions_are_exact(
                    function, &receipt,
                )
                && receipt
                    .checker_caller
                    .len_observation
                    .as_ref()
                    .is_some_and(|len| {
                        subject == format!("inst:drop.list-borrow:{}", len.call_node.0)
                    })
        })
}

fn instruction_touches_ordinary_map_owned_any_v2_type(
    function: &MirFunction,
    instruction: &MirInstructionKind,
) -> bool {
    let owner_ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
    let payload_ty = crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id();
    let get_result_ty = crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
    let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
    let values_element_ty =
        crate::core::mir::types::ordinary_map_owned_any_v2_values_element_type_id();
    let is_profile_value = |value: &MirValueId| {
        function.values.get(value).is_some_and(|value| {
            value.ty == owner_ty
                || value.ty == payload_ty
                || value.ty == get_result_ty
                || value.ty == values_ty
                || value.ty == values_element_ty
        })
    };
    match instruction {
        MirInstructionKind::OrdinaryMapOwnedAnyV2New { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2SetAnyResult { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2SetStringResult { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2GetStringResult { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Size { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultString { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Values { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { .. } => true,
        MirInstructionKind::MapRootNew { result } => is_profile_value(result),
        MirInstructionKind::MapRootSet {
            result,
            source,
            value,
            ..
        } => is_profile_value(result) || is_profile_value(source) || is_profile_value(value),
        MirInstructionKind::MapRootRemove { result, source, .. } => {
            is_profile_value(result) || is_profile_value(source)
        }
        MirInstructionKind::MapRootSize { result, root }
        | MirInstructionKind::MapRootContains { result, root, .. } => {
            is_profile_value(result) || is_profile_value(root)
        }
        MirInstructionKind::MapRootDrop { root } => is_profile_value(root),
        MirInstructionKind::Copy { result, source }
        | MirInstructionKind::Move { result, source }
        | MirInstructionKind::Clone { result, source } => {
            is_profile_value(result) || is_profile_value(source)
        }
        MirInstructionKind::Drop { value } | MirInstructionKind::EndBorrow { borrow: value } => {
            is_profile_value(value)
        }
        MirInstructionKind::Call {
            result, arguments, ..
        } => {
            result.as_ref().is_some_and(is_profile_value) || arguments.iter().any(is_profile_value)
        }
        MirInstructionKind::ClosureInvoke {
            result,
            function_value,
            arguments,
            ..
        } => {
            is_profile_value(function_value)
                || result.as_ref().is_some_and(is_profile_value)
                || arguments.iter().any(is_profile_value)
        }
        MirInstructionKind::BuiltinCall {
            result, arguments, ..
        } => is_profile_value(result) || arguments.iter().any(is_profile_value),
        _ => false,
    }
}

fn instruction_touches_ordinary_map_owned_get(instruction: &MirInstructionKind) -> bool {
    matches!(
        instruction,
        MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1NewResult { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1SetResult { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { .. }
    )
}

fn instruction_touches_ordinary_map_owned_get_type(
    function: &MirFunction,
    instruction: &MirInstructionKind,
) -> bool {
    let owner_ty = crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
    let is_owner = |value: &MirValueId| {
        function
            .values
            .get(value)
            .is_some_and(|value| value.ty == owner_ty)
    };
    match instruction {
        MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1NewResult { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1SetResult { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { .. } => true,
        MirInstructionKind::MapRootNew { result } => is_owner(result),
        MirInstructionKind::MapRootSet {
            result,
            source,
            value,
            ..
        } => is_owner(result) || is_owner(source) || is_owner(value),
        MirInstructionKind::MapRootRemove { result, source, .. } => {
            is_owner(result) || is_owner(source)
        }
        MirInstructionKind::MapRootSize { result, root }
        | MirInstructionKind::MapRootContains { result, root, .. } => {
            is_owner(result) || is_owner(root)
        }
        MirInstructionKind::MapRootDrop { root } => is_owner(root),
        MirInstructionKind::Copy { result, source }
        | MirInstructionKind::Move { result, source }
        | MirInstructionKind::Clone { result, source } => is_owner(result) || is_owner(source),
        MirInstructionKind::Drop { value } | MirInstructionKind::EndBorrow { borrow: value } => {
            is_owner(value)
        }
        MirInstructionKind::Call {
            result, arguments, ..
        } => result.as_ref().is_some_and(is_owner) || arguments.iter().any(is_owner),
        MirInstructionKind::ClosureInvoke {
            result,
            function_value,
            arguments,
            ..
        } => {
            is_owner(function_value)
                || result.as_ref().is_some_and(is_owner)
                || arguments.iter().any(is_owner)
        }
        MirInstructionKind::BuiltinCall {
            result, arguments, ..
        } => is_owner(result) || arguments.iter().any(is_owner),
        _ => false,
    }
}

fn instruction_touches_ordinary_type(
    function: &MirFunction,
    instruction: &MirInstructionKind,
) -> bool {
    let owner_ty = crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
    let is_owner = |value: &MirValueId| {
        function
            .values
            .get(value)
            .is_some_and(|value| value.ty == owner_ty)
    };
    match instruction {
        MirInstructionKind::OrdinaryMapOwnerLifecycleNew { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleSet { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleSize { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleContains { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln { .. } => true,
        MirInstructionKind::MapRootNew { result } => is_owner(result),
        MirInstructionKind::MapRootSet {
            result,
            source,
            value,
            ..
        } => is_owner(result) || is_owner(source) || is_owner(value),
        MirInstructionKind::MapRootRemove { result, source, .. } => {
            is_owner(result) || is_owner(source)
        }
        MirInstructionKind::MapRootSize { result, root }
        | MirInstructionKind::MapRootContains { result, root, .. } => {
            is_owner(result) || is_owner(root)
        }
        MirInstructionKind::MapRootDrop { root } => is_owner(root),
        MirInstructionKind::Copy { result, source }
        | MirInstructionKind::Move { result, source }
        | MirInstructionKind::Clone { result, source } => is_owner(result) || is_owner(source),
        MirInstructionKind::Drop { value } | MirInstructionKind::EndBorrow { borrow: value } => {
            is_owner(value)
        }
        MirInstructionKind::Call {
            result, arguments, ..
        } => result.as_ref().is_some_and(is_owner) || arguments.iter().any(is_owner),
        MirInstructionKind::ClosureInvoke {
            result,
            function_value,
            arguments,
            ..
        } => {
            is_owner(function_value)
                || result.as_ref().is_some_and(is_owner)
                || arguments.iter().any(is_owner)
        }
        MirInstructionKind::BuiltinCall {
            result, arguments, ..
        } => is_owner(result) || arguments.iter().any(is_owner),
        _ => false,
    }
}

/// MapAnyOwner v1 deliberately reuses the closed MapRoot instruction shapes,
/// while its distinct synthetic TypeDesc and private Checker receipt supply
/// the owner-token semantics. Select that route only from typed MIR identity;
/// a raw MapRoot receipt cannot classify an operation as v1.
fn instruction_touches_map_any_owner(
    function: &MirFunction,
    instruction: &MirInstructionKind,
) -> bool {
    let owner_ty = crate::core::mir::types::map_any_owner_v1_type_id();
    let is_owner = |value: &MirValueId| {
        function
            .values
            .get(value)
            .is_some_and(|value| value.ty == owner_ty)
    };
    match instruction {
        MirInstructionKind::MapRootNew { result } => is_owner(result),
        MirInstructionKind::MapRootSet {
            result,
            source,
            value,
            ..
        } => is_owner(result) || is_owner(source) || is_owner(value),
        MirInstructionKind::MapRootRemove { result, source, .. } => {
            is_owner(result) || is_owner(source)
        }
        MirInstructionKind::MapRootSize { result, root }
        | MirInstructionKind::MapRootContains { result, root, .. } => {
            is_owner(result) || is_owner(root)
        }
        MirInstructionKind::MapRootDrop { root } => is_owner(root),
        MirInstructionKind::ClosureInvoke {
            result,
            function_value,
            arguments,
            ..
        } => {
            is_owner(function_value)
                || result.as_ref().is_some_and(is_owner)
                || arguments.iter().any(is_owner)
        }
        _ => false,
    }
}

fn value_operation_is_supported(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
    operation: MirGlueOperation,
) -> bool {
    let Some(descriptor) = catalog.get(ty) else {
        return false;
    };
    if descriptor.ownership == MirOwnership::Copy {
        catalog.validate_copy_value(ty).is_ok()
    } else {
        catalog.validate_glue(ty, operation).is_ok()
    }
}

fn function_has_ensures(function: &MirFunction) -> bool {
    function
        .contracts
        .iter()
        .any(|contract| contract.kind == MirContractKind::Ensures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ir::{PrimitiveType, ResolvedType};
    use crate::core::NodeId;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn canonical(source: &str) -> MirProgram {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        MirProgram::from_checked_program(&checked).expect("canonical MIR")
    }

    fn get_any_result_i32_fixture() -> (crate::core::CheckedProgram, MirProgram) {
        let source = include_str!(
            "../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_any_result_i32_lineage.mimi"
        );
        let tokens = Lexer::new(source)
            .tokenize()
            .expect("lex GetAnyResult fixture");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse GetAnyResult fixture");
        let checked = crate::core::check_program(&file).expect("check GetAnyResult fixture");
        let owner = NodeId("function:main".into());
        let program = MirProgram::from_checked_program_map_set_get_any_result_i32_for_verifier(
            &checked, &owner,
        )
        .expect("materialize exact Result-valued GetAny L2 MIR");
        (checked, program)
    }

    #[test]
    fn get_any_result_i32_l2_capability_is_source_bound_and_rejects_legacy_route() {
        let (checked, program) = get_any_result_i32_fixture();
        let result_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_L2_VERIFIER_PROFILE;
        let receipt = program.route_receipt(result_profile);

        validate_mir_capabilities_with_ordinary_map_owned_any_v2_set_get_any_result_i32_l2_verifier_route(
            &checked,
            &program,
            &receipt,
        )
        .expect("exact Result-valued GetAny route passes capability preflight");

        assert!(
            validate_mir_capabilities(&program).is_err(),
            "the generic capability gate remains closed for GetAnyResult MIR"
        );

        let legacy_tuple_receipt = program.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_L2_VERIFIER_PROFILE,
        );
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_set_get_any_result_i32_l2_verifier_route(
                &checked,
                &program,
                &legacy_tuple_receipt,
            )
            .is_err(),
            "the old tuple-valued route profile cannot authorize Result-valued GetAny"
        );
    }

    #[test]
    fn get_any_result_i32_l2_capability_rejects_tuple_receipt_from_another_checked_program() {
        let (_checked, program) = get_any_result_i32_fixture();
        let source = include_str!(
            "../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_any_i32_lineage.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex tuple fixture");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse tuple fixture");
        let tuple_checked = crate::core::check_program(&file).expect("check tuple fixture");
        let profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_RESULT_I32_L2_VERIFIER_PROFILE;
        let receipt = program.route_receipt(profile);

        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_set_get_any_result_i32_l2_verifier_route(
                &tuple_checked,
                &program,
                &receipt,
            )
            .is_err(),
            "a CheckerProgram carrying the legacy tuple receipt cannot be paired with this route"
        );
    }

    fn get_or_default_string_fixture(name: &str) -> (crate::core::CheckedProgram, MirProgram) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let source = std::fs::read_to_string(&path).expect("read GetOrDefault<String> fixture");
        let tokens = Lexer::new(&source).tokenize().expect("lex fixture");
        let parsed = crate::loader::parser_for_path(tokens, &path)
            .expect("configure fixture parser")
            .parse_file()
            .expect("parse fixture");
        let mut loader =
            crate::loader::ModuleLoader::new(path.parent().expect("fixture parent").to_path_buf());
        loader
            .load_main_with_file(&path, parsed)
            .expect("load fixture imports");
        let mut file = loader.merge_all().expect("merge fixture imports");
        crate::loader::merge_prelude_into(&mut file);
        let checked = crate::core::check_program(&file).expect("check fixture");
        let excluded = checked
            .source_registry()
            .records()
            .iter()
            .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
            .map(|record| record.id)
            .collect::<std::collections::HashSet<_>>();
        let program = MirProgram::from_checked_program_ordinary_map_get_or_default_string(
            &checked, &excluded,
        )
        .expect("fixture lowers through the canonical scoped String route");
        (checked, program)
    }

    #[test]
    fn get_or_default_string_checked_bridge_accepts_bound_and_inline_values() {
        for (fixture, expected_default_local, expected_set_local) in [
            (
                "mir_ordinary_map_owned_any_v2_get_or_default_string_hit.mimi",
                true,
                true,
            ),
            (
                "mir_ordinary_map_owned_any_v2_get_or_default_string_alias.mimi",
                false,
                true,
            ),
        ] {
            let (checked, program) = get_or_default_string_fixture(fixture);
            let receipts = program
                .ordinary_map_owned_any_v2_get_or_default_string_receipts()
                .expect("derive String MIR receipt");
            let [receipt] = receipts.as_slice() else {
                panic!("fixture has one GetOrDefault<String> receipt");
            };
            assert_eq!(
                receipt.caller_default_local.is_some(),
                expected_default_local
            );
            assert_eq!(receipt.caller_set_value_local.is_some(), expected_set_local);
            validate_ordinary_map_owned_any_v2_get_or_default_string_checked_bridge(
                &checked, &program, receipt,
            )
            .expect("exact Checker/MIR String operands are admitted");
        }
    }

    #[test]
    fn get_or_default_string_checked_bridge_accepts_inline_set_payload() {
        let (checked, program) = get_or_default_string_fixture(
            "mir_ordinary_map_owned_any_v2_get_or_default_string_inline_set.mimi",
        );
        let receipts = program
            .ordinary_map_owned_any_v2_get_or_default_string_receipts()
            .expect("derive inline-payload String MIR receipt");
        let [receipt] = receipts.as_slice() else {
            panic!("fixture has one GetOrDefault<String> receipt");
        };
        assert!(receipt.caller_default_local.is_none());
        assert!(receipt.caller_set_value_local.is_none());
        validate_ordinary_map_owned_any_v2_get_or_default_string_checked_bridge(
            &checked, &program, receipt,
        )
        .expect("exact Checker/MIR inline operands are admitted");
    }

    #[test]
    fn get_or_default_string_checked_bridge_rejects_forged_caller_clones() {
        let (checked, program) = get_or_default_string_fixture(
            "mir_ordinary_map_owned_any_v2_get_or_default_string_hit.mimi",
        );
        let receipts = program
            .ordinary_map_owned_any_v2_get_or_default_string_receipts()
            .expect("derive String MIR receipt");
        let [receipt] = receipts.as_slice() else {
            panic!("fixture has one GetOrDefault<String> receipt");
        };
        let default_local = receipt
            .caller_default_local
            .as_ref()
            .expect("fixture binds its default to a local");
        let default_source = MirValueId::new(format!("local:{}", default_local.0 .0))
            .expect("default source MIR identity");
        let set_local = receipt
            .caller_set_value_local
            .as_ref()
            .expect("fixture binds its Set payload to a local");
        let set_source = MirValueId::new(format!("local:{}", set_local.0 .0))
            .expect("Set payload source MIR identity");
        let initial_root =
            MirValueId::new(format!("local:{}", receipt.caller_initial_root_local.0 .0))
                .expect("initial root MIR identity");
        let set_value = program.functions()[&receipt.owner]
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
                    result,
                    source,
                    value,
                    ..
                } if result == &receipt.root && source == &initial_root => Some(value.clone()),
                _ => None,
            })
            .expect("exact caller Set payload operand");

        let mut wrong_source = program.clone();
        let mut function = wrong_source.functions()[&receipt.owner].clone();
        let clone = function
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Clone { result, source }
                        if result == &receipt.default && source == &default_source
                )
            })
            .expect("caller default Clone");
        let MirInstructionKind::Clone { source, .. } = &mut clone.kind else {
            unreachable!("selected caller default Clone")
        };
        *source = MirValueId::new("local:forged_default").expect("forged source identity");
        wrong_source.replace_function_for_test_only(function);
        assert!(
            validate_ordinary_map_owned_any_v2_get_or_default_string_checked_bridge(
                &checked,
                &wrong_source,
                receipt,
            )
            .is_err(),
            "a Clone from a different local cannot stand in for the Checker default"
        );

        let mut duplicate_clone = program.clone();
        let mut function = duplicate_clone.functions()[&receipt.owner].clone();
        let block = function
            .blocks
            .values_mut()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        &instruction.kind,
                        MirInstructionKind::Clone { result, source }
                            if result == &receipt.default && source == &default_source
                    )
                })
            })
            .expect("caller default Clone block");
        let mut duplicate = block
            .instructions
            .iter()
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Clone { result, source }
                        if result == &receipt.default && source == &default_source
                )
            })
            .expect("caller default Clone")
            .clone();
        duplicate.id = crate::core::mir::MirInstructionId::new("inst:forged_default_clone")
            .expect("forged clone instruction identity");
        block.instructions.push(duplicate);
        duplicate_clone.replace_function_for_test_only(function);
        assert!(
            validate_ordinary_map_owned_any_v2_get_or_default_string_checked_bridge(
                &checked,
                &duplicate_clone,
                receipt,
            )
            .is_err(),
            "a second Clone producing the default operand is rejected"
        );

        let mut wrong_type = program.clone();
        let mut function = wrong_type.functions()[&receipt.owner].clone();
        function
            .values
            .get_mut(&default_source)
            .expect("default local value")
            .ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        wrong_type.replace_function_for_test_only(function);
        assert!(
            validate_ordinary_map_owned_any_v2_get_or_default_string_checked_bridge(
                &checked,
                &wrong_type,
                receipt,
            )
            .is_err(),
            "a default Clone with a non-String source type is rejected"
        );

        let mut wrong_set_source = program.clone();
        let mut function = wrong_set_source.functions()[&receipt.owner].clone();
        let clone = function
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Clone { result, source }
                        if result == &set_value && source == &set_source
                )
            })
            .expect("caller Set payload Clone");
        let MirInstructionKind::Clone { source, .. } = &mut clone.kind else {
            unreachable!("selected caller Set payload Clone")
        };
        *source = MirValueId::new("local:forged_set_payload")
            .expect("forged Set payload source identity");
        wrong_set_source.replace_function_for_test_only(function);
        assert!(
            validate_ordinary_map_owned_any_v2_get_or_default_string_checked_bridge(
                &checked,
                &wrong_set_source,
                receipt,
            )
            .is_err(),
            "a Clone from a different local cannot stand in for the Checker Set payload"
        );

        let mut duplicate_set_clone = program.clone();
        let mut function = duplicate_set_clone.functions()[&receipt.owner].clone();
        let block = function
            .blocks
            .values_mut()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        &instruction.kind,
                        MirInstructionKind::Clone { result, source }
                            if result == &set_value && source == &set_source
                    )
                })
            })
            .expect("caller Set payload Clone block");
        let mut duplicate = block
            .instructions
            .iter()
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Clone { result, source }
                        if result == &set_value && source == &set_source
                )
            })
            .expect("caller Set payload Clone")
            .clone();
        duplicate.id = crate::core::mir::MirInstructionId::new("inst:forged_set_payload_clone")
            .expect("forged Set payload clone instruction identity");
        block.instructions.push(duplicate);
        duplicate_set_clone.replace_function_for_test_only(function);
        assert!(
            validate_ordinary_map_owned_any_v2_get_or_default_string_checked_bridge(
                &checked,
                &duplicate_set_clone,
                receipt,
            )
            .is_err(),
            "a second Clone producing the Set payload operand is rejected"
        );
    }

    fn keys_snapshot_program() -> (crate::core::CheckedProgram, MirProgram) {
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
"#;
        let tokens = Lexer::new(source).tokenize().expect("lex KeysSnapshot");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse KeysSnapshot");
        let checked = crate::core::check_program(&file).expect("check KeysSnapshot");
        let program = MirProgram::from_checked_program(&checked).expect("lower KeysSnapshot");
        (checked, program)
    }

    #[test]
    fn keys_snapshot_capability_requires_its_checker_route_and_keeps_generic_gate_closed() {
        let (checked, program) = keys_snapshot_program();
        assert!(checked
            .validate_backend(crate::core::BackendProfile::Verifier)
            .is_ok());
        let route = crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_route(
            &checked, &program,
        )
        .expect("exact Checker-bound KeysSnapshot route");
        assert_eq!(
            route.profile,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE
        );
        let witness =
            crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_mir_route(&program)
                .expect("exact MIR KeysSnapshot witness");
        assert_eq!(witness.static_key_utf8, b"answer");
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_keys_snapshot_route(
            &checked, &program, &route,
        )
        .expect("the source-sealed KeysSnapshot profile passes its capability gate");

        assert!(
            validate_mir_capabilities(&program).is_err(),
            "the generic verifier gate must not inherit Keys permission"
        );
        assert!(
            validate_mir_capabilities_with_direct_print_route_receipt(&program, &route).is_err(),
            "a DirectPrint consumer cannot borrow KeysSnapshot authority"
        );
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_local_index_i32_route(
                &checked, &program, &route,
            )
            .is_err(),
            "a Values consumer cannot borrow KeysSnapshot authority"
        );
    }

    #[test]
    fn keys_snapshot_capability_rejects_wrong_source_stale_route_and_missing_witness() {
        let (checked, program) = keys_snapshot_program();
        let route = crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_route(
            &checked, &program,
        )
        .expect("exact Checker-bound KeysSnapshot route");

        let unrelated = crate::core::check_program(
            &Parser::new(Lexer::new("func main() -> i32 { 0 }").tokenize().unwrap())
                .parse_file()
                .expect("parse unrelated checked source"),
        )
        .expect("check unrelated source");
        assert!(unrelated
            .validate_backend(crate::core::BackendProfile::Verifier)
            .is_ok());
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_keys_snapshot_route(
                &unrelated, &program, &route,
            )
            .is_err(),
            "another CheckedProgram cannot supply the source authority"
        );

        let component_only_source = r#"
flow Decision {
    state Pending { v: i32 }
    state Yes { v: i32 }
    state No { v: i32 }
    transition decide(Pending, ok: bool) -> Yes | No {
        if ok { return Yes { v: 1 } }
        return No { v: 0 }
    }
}
func main() -> i32 { 0 }
"#;
        let component_only = crate::core::check_program(
            &Parser::new(Lexer::new(component_only_source).tokenize().unwrap())
                .parse_file()
                .expect("parse component-only CheckedProgram"),
        )
        .expect("check component-only CheckedProgram");
        assert!(component_only
            .validate_backend(crate::core::BackendProfile::Verifier)
            .is_ok());
        assert!(component_only
            .validate_backend(crate::core::BackendProfile::Component)
            .is_err());
        let wrong_backend =
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_keys_snapshot_route(
                &component_only,
                &program,
                &route,
            )
            .expect_err("a component-only CheckedProgram has no Keys source authority");
        assert!(
            wrong_backend
                .iter()
                .any(|error| error.contains("KeysSnapshot Checker/MIR route rejected")),
            "Keys admission must validate the Verifier backend before source-route matching: {wrong_backend:?}"
        );
        assert!(
            wrong_backend
                .iter()
                .all(|error| !error.contains("backend Component")),
            "Keys admission must not silently switch to the Component backend: {wrong_backend:?}"
        );

        let mut wrong_profile = route.clone();
        wrong_profile.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE.into();
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_keys_snapshot_route(
                &checked,
                &program,
                &wrong_profile,
            )
            .is_err(),
            "a route receipt from another profile cannot authorize Keys"
        );

        let mut stale_program = program.clone();
        let mut main = stale_program.functions()[&NodeId("function:main".into())].clone();
        let keys = main
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { .. }
                )
            })
            .expect("Keys instruction");
        let MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { root, .. } = &mut keys.kind else {
            unreachable!("selected instruction is Keys")
        };
        *root = MirValueId::new("local:forged_keys_root").expect("valid forged MIR id");
        stale_program.replace_function_for_test_only(main);
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_keys_snapshot_route(
                &checked,
                &stale_program,
                &route,
            )
            .is_err(),
            "an old route receipt cannot authorize changed MIR"
        );

        let mut missing_witness = program;
        let mut main = missing_witness.functions()[&NodeId("function:main".into())].clone();
        main.blocks.values_mut().for_each(|block| {
            block.instructions.retain(|instruction| {
                !matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack { .. }
                )
            });
        });
        missing_witness.replace_function_for_test_only(main);
        let resealed = missing_witness
            .route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE);
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_keys_snapshot_route(
                &checked,
                &missing_witness,
                &resealed,
            )
            .is_err(),
            "resealing a graph without its exact index-zero witness cannot authorize Keys"
        );
    }

    fn values_producer_caller_program() -> (crate::core::CheckedProgram, MirProgram) {
        let source = include_str!(
            "../../tests/fixtures/mir_ordinary_map_owned_any_v2_values_producer_caller.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex Values bridge");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse Values bridge");
        let checked = crate::core::check_program(&file).expect("check Values bridge");
        let program = MirProgram::from_checked_program(&checked).expect("lower Values bridge");
        (checked, program)
    }

    fn map_root_producer_caller_program() -> (crate::core::CheckedProgram, MirProgram) {
        let source = include_str!(
            "../../tests/fixtures/mir_ordinary_map_owned_any_v2_map_root_producer_caller.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex Map-root bridge");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse Map-root bridge");
        let checked = crate::core::check_program(&file).expect("check Map-root bridge");
        let program = MirProgram::from_checked_program(&checked).expect("lower Map-root bridge");
        (checked, program)
    }

    fn map_root_get_any_caller_program() -> (crate::core::CheckedProgram, MirProgram) {
        let source = include_str!(
            "../../tests/fixtures/mir_ordinary_map_owned_any_v2_map_root_get_any_caller.mimi"
        );
        let tokens = Lexer::new(source)
            .tokenize()
            .expect("lex Map-root GetAny bridge");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse Map-root GetAny bridge");
        let checked = crate::core::check_program(&file).expect("check Map-root GetAny bridge");
        let program =
            MirProgram::from_checked_program(&checked).expect("lower Map-root GetAny bridge");
        (checked, program)
    }

    #[test]
    fn values_producer_caller_capability_binds_len_clone_and_call_site_drop() {
        let (checked, program) = values_producer_caller_program();
        let receipts = program
            .ordinary_map_owned_any_v2_values_producer_caller_receipts()
            .expect("Core Values bridge receipt");
        let [receipt] = receipts.as_slice() else {
            panic!("fixture must have one Values bridge");
        };
        let caller = &program.functions()[&receipt.caller_owner];
        let producer = &program.functions()[&receipt.producer_owner];
        let receiver = receipt.len_receiver.as_ref().expect("Len receiver");
        let len = receipt
            .checker_caller
            .len_observation
            .as_ref()
            .expect("Checker Len observation");
        assert_ne!(len.call_node, len.receiver_node);
        assert!(
            ordinary_map_owned_any_v2_values_producer_caller_bridge_for_producer(
                &program, producer,
            )
            .is_some()
        );
        assert!(
            ordinary_map_owned_any_v2_values_producer_caller_bridge_for_caller(&program, caller,)
                .is_some()
        );
        assert!(ordinary_map_owned_any_v2_values_borrow_clone_is_receipted(
            &program,
            caller,
            receiver,
            &receipt.caller_binding,
            true,
        ));
        assert!(
            ordinary_map_owned_any_v2_values_producer_caller_drop_is_receipted(
                &program,
                caller,
                &format!("inst:drop.list-borrow:{}", len.call_node.0),
                receiver,
            )
        );
        assert!(
            !ordinary_map_owned_any_v2_values_producer_caller_drop_is_receipted(
                &program,
                caller,
                &format!("inst:drop.list-borrow:{}", len.receiver_node.0),
                receiver,
            )
        );
        assert!(
            ordinary_map_owned_any_v2_values_producer_caller_drop_is_receipted(
                &program,
                caller,
                receipt.owner_drop_instruction.as_str(),
                &receipt.owner_drop_value,
            )
        );
        assert!(!caller
            .ownership
            .has_checker_ordinary_map_owned_any_v2_actions());
        let route =
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_route(
                &checked, &program,
            )
            .expect("Checker-bound Values route");
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_producer_caller_route(
            &checked, &program, &route,
        )
        .expect("exact producer, caller and both Drop sites are admitted");
        assert!(validate_mir_capabilities(&program).is_err());

        let broad_route = program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_producer_caller_route(
                &checked,
                &program,
                &broad_route,
            )
            .is_err(),
            "a broad owner-v2 route cannot authorize this bridge"
        );
    }

    #[test]
    fn values_producer_caller_capability_rejects_resealed_len_borrow_drift() {
        let (checked, program) = values_producer_caller_program();
        let receipts = program
            .ordinary_map_owned_any_v2_values_producer_caller_receipts()
            .expect("Core Values bridge receipt");
        let [receipt] = receipts.as_slice() else {
            panic!("fixture must have one Values bridge");
        };
        let len = receipt
            .checker_caller
            .len_observation
            .as_ref()
            .expect("Checker Len observation");
        let drop_id = format!("inst:drop.list-borrow:{}", len.call_node.0);
        for mutation in 0..3 {
            let mut forged = program.clone();
            let mut caller = program.functions()[&receipt.caller_owner].clone();
            let instruction = caller
                .blocks
                .values_mut()
                .flat_map(|block| &mut block.instructions)
                .find(|instruction| instruction.id.as_str() == drop_id)
                .expect("exact Len temporary Drop");
            match mutation {
                0 => {
                    instruction.id = crate::core::mir::MirInstructionId::new(format!(
                        "inst:drop.list-borrow:{}",
                        len.receiver_node.0,
                    ))
                    .expect("syntactically valid forged receiver-site Drop identity");
                }
                1 => {
                    instruction.kind = MirInstructionKind::Drop {
                        value: receipt.caller_binding.clone(),
                    };
                }
                _ => {
                    caller.blocks.values_mut().for_each(|block| {
                        block
                            .instructions
                            .retain(|instruction| instruction.id.as_str() != drop_id);
                    });
                }
            }
            forged.replace_function_for_test_only(caller);
            assert!(
                !ordinary_map_owned_any_v2_values_producer_caller_bridge_is_exact(&forged, receipt),
                "mutation {mutation} must lose exact bridge coverage"
            );
            let route = forged.route_receipt(
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_PRODUCER_CALLER_PROFILE,
            );
            assert!(
                validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_producer_caller_route(
                    &checked, &forged, &route,
                )
                .is_err(),
                "resealing mutation {mutation} cannot authorize a different temporary settlement"
            );
        }
    }

    #[test]
    fn map_root_producer_caller_capability_admits_only_the_checker_sealed_bridge() {
        let (checked, program) = map_root_producer_caller_program();
        let route =
            crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_route(
                &checked, &program,
            )
            .expect("Checker-bound Map-root producer/caller route");
        let receipts =
            crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_mir_route(
                &program,
            )
            .expect("exact Map-root producer/caller MIR bridge");
        let [receipt] = receipts.as_slice() else {
            panic!("fixture must have one Map-root producer/caller bridge");
        };
        assert_eq!(
            receipt.root_type,
            crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
        );
        assert_eq!(
            receipt.scalar_type,
            crate::core::ir::primitive_type_id(PrimitiveType::I32)
        );
        assert_eq!(
            receipt.producer_owner, receipt.call_target,
            "the receipt must bind the returned root to its direct producer"
        );
        assert!(validate_mir_capabilities(&program).is_err());
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
            &checked, &program, &route,
        )
        .expect("only the exact Checker-bound producer/caller route admits the root return");

        let broad_route = program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
                &checked,
                &program,
                &broad_route,
            )
            .is_err(),
            "a broad owner-v2 profile cannot authorize the cross-function root"
        );
    }

    #[test]
    fn map_root_producer_caller_capability_rejects_wrong_caller_and_missing_drop() {
        let (checked, canonical) = map_root_producer_caller_program();
        let receipts = canonical
            .ordinary_map_owned_any_v2_map_root_producer_caller_receipts()
            .expect("Map-root producer/caller receipt");
        let [receipt] = receipts.as_slice() else {
            panic!("fixture must have one Map-root producer/caller bridge");
        };

        let mut wrong_caller = canonical.clone();
        let mut main = wrong_caller.functions()[&NodeId("function:main".into())].clone();
        let call = main
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Call {
                        callee: ResolvedCallee::Function(target),
                        ..
                    } if target == &receipt.caller_owner
                )
            })
            .expect("main's direct call to the receipted caller");
        let MirInstructionKind::Call { callee, .. } = &mut call.kind else {
            unreachable!("selected instruction is a direct call")
        };
        *callee = ResolvedCallee::Function(receipt.producer_owner.clone());
        wrong_caller.replace_function_for_test_only(main);
        let wrong_caller_route = wrong_caller.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_PRODUCER_CALLER_PROFILE,
        );
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
                &checked,
                &wrong_caller,
                &wrong_caller_route,
            )
            .is_err(),
            "a second caller cannot borrow the unique producer receipt"
        );

        let mut missing_drop = canonical.clone();
        let mut caller = missing_drop.functions()[&receipt.caller_owner].clone();
        caller.blocks.values_mut().for_each(|block| {
            block
                .instructions
                .retain(|instruction| instruction.id != receipt.caller_root_drop_instruction);
        });
        missing_drop.replace_function_for_test_only(caller);
        let missing_drop_route = missing_drop.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_PRODUCER_CALLER_PROFILE,
        );
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
                &checked,
                &missing_drop,
                &missing_drop_route,
            )
            .is_err(),
            "the caller must drop the exact transferred root before scalar return"
        );
    }

    #[test]
    fn map_root_get_any_caller_capability_requires_its_exact_three_settlement_receipt() {
        let (checked, program) = map_root_get_any_caller_program();
        let route =
            crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
                &checked, &program,
            )
            .expect("Checker-bound Map-root GetAny/String caller route");
        assert_eq!(
            route.profile,
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_GET_ANY_CALLER_PROFILE
        );
        let receipts =
            crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_mir_route(
                &program,
            )
            .expect("exact Map-root GetAny/String MIR bridge");
        let [receipt] = receipts.as_slice() else {
            panic!("fixture must have one Map-root GetAny caller bridge");
        };
        assert_eq!(
            receipt.root_type,
            crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
        );
        assert_eq!(
            receipt.string_type,
            crate::core::ir::primitive_type_id(PrimitiveType::String)
        );
        assert_eq!(
            receipt.scalar_type,
            crate::core::ir::primitive_type_id(PrimitiveType::I32)
        );
        assert!(validate_mir_capabilities(&program).is_err());
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
            &checked, &program, &route,
        )
        .expect("exact Checker, GetAny, checked String and three Drop sites are admitted");

        let broad_route = program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        assert!(
            validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
                &checked,
                &program,
                &broad_route,
            )
            .is_err(),
            "a broad owner-v2 profile cannot authorize the cross-function root and String unpack"
        );
    }

    #[test]
    fn map_root_get_any_caller_capability_rejects_missing_root_or_string_drop() {
        let (checked, canonical) = map_root_get_any_caller_program();
        let receipts = canonical
            .ordinary_map_owned_any_v2_map_root_get_any_caller_receipts()
            .expect("Map-root GetAny caller receipt");
        let [receipt] = receipts.as_slice() else {
            panic!("fixture must have one Map-root GetAny caller bridge");
        };

        for missing in [
            receipt.caller_root_drop_instruction.clone(),
            receipt.caller_string_drop_instruction.clone(),
        ] {
            let mut forged = canonical.clone();
            let mut caller = forged.functions()[&receipt.caller_owner].clone();
            caller.blocks.values_mut().for_each(|block| {
                block
                    .instructions
                    .retain(|instruction| instruction.id != missing);
            });
            forged.replace_function_for_test_only(caller);
            let route = forged.route_receipt(
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_GET_ANY_CALLER_PROFILE,
            );
            assert!(
                validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
                    &checked, &forged, &route,
                )
                .is_err(),
                "the bridge must reject missing settlement {missing}"
            );
        }
    }

    #[test]
    fn verifier_capability_preflights_nested_callable_scope_receipts() {
        let program = canonical(
            r#"
                func parent(seed: i32) -> i32 {
                    let offset = seed
                    func nested(value: i32) -> i32 { value + offset }
                    nested(41)
                }
                func main() -> i32 { parent(1) }
            "#,
        );
        assert_eq!(program.nested_callable_scopes().len(), 1);
        validate_mir_capabilities(&program)
            .expect("complete declaration-time nested scope receipt is admitted");

        let mut forged = program;
        forged.clear_nested_callable_scopes_for_test_only();
        let errors = validate_mir_capabilities(&forged)
            .expect_err("a nested callable cannot enter verifier admission without its receipt");
        assert!(!errors.is_empty());
    }

    fn ordinary_map_owned_any_v2_values_program() -> MirProgram {
        canonical(
            r#"
                type Label { text: string }

                func collect() -> i32 {
                    ensures: result == 1
                    let label = Label { text: "snapshot" }
                    let empty = map_new()
                    let with_label = map_set(empty, "label", label)
                    let snapshot = values(with_label)
                    let count = snapshot.len()
                    drop(snapshot)
                    drop(empty)
                    drop(with_label)
                    drop(label)
                    count
                }
                func main() -> i32 { 0 }
            "#,
        )
    }

    fn ordinary_map_owned_any_v2_zero_miss_i32_program() -> MirProgram {
        canonical(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mir_ordinary_map_any_owner_v2_zero_miss_i32.mimi"
        )))
    }

    #[test]
    fn zero_miss_i32_census_admits_only_the_exact_two_owner_flow() {
        let program = ordinary_map_owned_any_v2_zero_miss_i32_program();
        validate_ordinary_map_owned_any_v2_zero_miss_i32_census(&program)
            .expect("the canonical hit/miss route has the exact closed owner census");
    }

    #[test]
    fn zero_miss_i32_census_rejects_operation_count_and_result_type_drift() {
        let program = ordinary_map_owned_any_v2_zero_miss_i32_program();
        let (mut duplicated_get_any, get_any_block) = program
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
                                MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
                            )
                        })
                        .map(|instruction| {
                            (function.clone(), (block_id.clone(), instruction.clone()))
                        })
                })
            })
            .expect("fixture contains GetAny");
        duplicated_get_any
            .blocks
            .get_mut(&get_any_block.0)
            .expect("GetAny block")
            .instructions
            .push(get_any_block.1);
        let mut extra_get_any = program.clone();
        extra_get_any.replace_function_for_test_only(duplicated_get_any);
        let errors = validate_ordinary_map_owned_any_v2_zero_miss_i32_census(&extra_get_any)
            .expect_err("a third GetAny cannot enter the closed census");
        assert!(errors.contains("GetAny=3"), "{errors}");

        let (mut wrong_result_type, unpack_result) = program
            .functions()
            .values()
            .find_map(|function| {
                function.blocks.values().find_map(|block| {
                    block.instructions.iter().find_map(|instruction| {
                        if let MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { result, .. } =
                            &instruction.kind
                        {
                            Some((function.clone(), result.clone()))
                        } else {
                            None
                        }
                    })
                })
            })
            .expect("fixture contains a DynamicAnyUnpack result");
        wrong_result_type
            .values
            .get_mut(&unpack_result)
            .expect("unpack result is in the function value table")
            .ty = crate::core::ir::primitive_type_id(PrimitiveType::I64);
        let mut wrong_type = program;
        wrong_type.replace_function_for_test_only(wrong_result_type);
        assert!(
            validate_ordinary_map_owned_any_v2_zero_miss_i32_census(&wrong_type).is_err(),
            "both unpack values must retain their canonical i32 TypeDesc"
        );
    }

    #[test]
    fn zero_miss_i32_census_rejects_extra_map_operations_and_duplicate_pair_drops() {
        let program = ordinary_map_owned_any_v2_zero_miss_i32_program();
        let (mut extra_map_function, get_any) = program
            .functions()
            .values()
            .find_map(|function| {
                function.blocks.iter().find_map(|(block_id, block)| {
                    block.instructions.iter().find_map(|instruction| {
                        if let MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny {
                            result,
                            root,
                            ..
                        } = &instruction.kind
                        {
                            Some((
                                function.clone(),
                                (
                                    block_id.clone(),
                                    result.clone(),
                                    root.clone(),
                                    instruction.clone(),
                                ),
                            ))
                        } else {
                            None
                        }
                    })
                })
            })
            .expect("fixture contains GetAny");
        let mut unexpected = get_any.3;
        unexpected.kind = MirInstructionKind::OrdinaryMapOwnedAnyV2Size {
            result: get_any.1,
            root: get_any.2,
        };
        extra_map_function
            .blocks
            .get_mut(&get_any.0)
            .expect("GetAny block")
            .instructions
            .push(unexpected);
        let mut extra_map = program.clone();
        extra_map.replace_function_for_test_only(extra_map_function);
        assert!(
            validate_ordinary_map_owned_any_v2_zero_miss_i32_census(&extra_map).is_err(),
            "any neighboring Map operation must be excluded from the closed census"
        );

        let pair_type = crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
        let (mut duplicate_drop_function, drop_site) = program
            .functions()
            .values()
            .find_map(|function| {
                function.blocks.iter().find_map(|(block_id, block)| {
                    block.instructions.iter().find_map(|instruction| {
                        if let MirInstructionKind::Drop { value } = &instruction.kind {
                            (function.values.get(value).map(|value| &value.ty) == Some(&pair_type))
                                .then(|| {
                                    (function.clone(), (block_id.clone(), instruction.clone()))
                                })
                        } else {
                            None
                        }
                    })
                })
            })
            .expect("fixture contains a GetAny pair Drop");
        duplicate_drop_function
            .blocks
            .get_mut(&drop_site.0)
            .expect("pair Drop block")
            .instructions
            .push(drop_site.1);
        let mut duplicate_pair_drop = program;
        duplicate_pair_drop.replace_function_for_test_only(duplicate_drop_function);
        let errors = validate_ordinary_map_owned_any_v2_zero_miss_i32_census(&duplicate_pair_drop)
            .expect_err("duplicating one pair Drop must fail the exact owner census");
        assert!(errors.contains("pair Drop=3"), "{errors}");
    }

    #[test]
    fn admits_only_typed_signed_i32_i64_power_across_the_full_mir_graph() {
        let program = canonical(
            r#"
                // These helpers are intentionally uncalled and contract-free:
                // capability is a whole-program property, not a reachable-call
                // or obligation-only property.
                func pow_i32(base: i32, exponent: i32) -> i32 { base ** exponent }
                func pow_i64(base: i64, exponent: i64) -> i64 { base ** exponent }
                func main() -> i32 { 0 }
            "#,
        );

        let mut saw_i32 = false;
        let mut saw_i64 = false;
        let mut power_count = 0;
        for function in program.functions().values() {
            for instruction in function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
            {
                let MirInstructionKind::Binary {
                    result,
                    op: ResolvedBinaryOp::Power,
                    ..
                } = &instruction.kind
                else {
                    continue;
                };
                power_count += 1;
                let value = function
                    .values
                    .get(result)
                    .expect("Power result is in the value table");
                let descriptor = program
                    .type_catalog()
                    .get(&value.ty)
                    .expect("Power result TypeDesc");
                match descriptor.abi {
                    MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    } => saw_i32 = true,
                    MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    } => saw_i64 = true,
                    other => panic!("Power result has unsupported ABI {other:?}"),
                }
            }
        }
        assert_eq!(
            power_count, 2,
            "the uncalled helpers remain in the MIR graph"
        );
        assert!(
            saw_i32 && saw_i64,
            "both supported source widths are present"
        );
        assert!(matches!(
            POWER_PROFILE_0_1_11_COMPAT,
            CheckedPowerProfile::VmNativeI64ThenI32Wrap
        ));
        validate_mir_capabilities(&program)
            .expect("signed i32/i64 Power must be admitted across every function");
    }

    #[test]
    fn rejects_float_unsigned_and_narrow_integer_power_profiles() {
        let float_program = canonical(
            "func pow_float(base: f64, exponent: f64) -> f64 { base ** exponent } func main() -> i32 { 0 }",
        );
        let float_errors = validate_mir_capabilities(&float_program)
            .expect_err("float Power remains outside the verifier capability");
        assert!(
            float_errors
                .iter()
                .any(|error| error.contains("float binary rejected")),
            "{float_errors:?}"
        );

        let signed_i32 = canonical(
            "func pow_i32(base: i32, exponent: i32) -> i32 { base ** exponent } func main() -> i32 { 0 }",
        );
        validate_mir_capabilities(&signed_i32)
            .expect("the real signed i32 source remains admitted");

        let mut power_type = None;
        for function in signed_i32.functions().values() {
            for instruction in function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
            {
                let MirInstructionKind::Binary {
                    result,
                    op: ResolvedBinaryOp::Power,
                    left,
                    right,
                } = &instruction.kind
                else {
                    continue;
                };
                let result_ty = function
                    .values
                    .get(result)
                    .expect("Power result value")
                    .ty
                    .clone();
                for operand in [left, right] {
                    assert_eq!(
                        function
                            .values
                            .get(operand)
                            .expect("Power operand value")
                            .ty,
                        result_ty,
                        "the source Power operands and result share one canonical TypeDesc ID"
                    );
                }
                assert!(
                    power_type.replace(result_ty).is_none(),
                    "single-function signed i32 source has exactly one Power result type"
                );
            }
        }
        let power_type = power_type.expect("signed i32 Power instruction exists");
        let canonical_descriptor = signed_i32
            .type_catalog()
            .get(&power_type)
            .expect("canonical signed i32 descriptor")
            .clone();
        assert_eq!(canonical_descriptor.id, power_type);
        assert_eq!(
            canonical_descriptor.abi,
            MirAbiClass::Integer {
                bits: 32,
                signed: true
            }
        );

        for forged_abi in [
            MirAbiClass::Integer {
                bits: 32,
                signed: false,
            },
            MirAbiClass::Integer {
                bits: 8,
                signed: true,
            },
            MirAbiClass::Integer {
                bits: 16,
                signed: true,
            },
        ] {
            let mut catalog = signed_i32.type_catalog().clone();
            let mut forged_descriptor = canonical_descriptor.clone();
            forged_descriptor.abi = forged_abi;
            catalog.replace_for_test_only(power_type.clone(), forged_descriptor);
            let forged_program =
                MirProgram::with_type_catalog(signed_i32.functions().clone(), catalog).expect(
                    "ABI-only mutation keeps the same canonical TypeDesc identity and MIR graph",
                );
            assert_eq!(
                forged_program.functions(),
                signed_i32.functions(),
                "the negative control must forge only the TypeDesc ABI metadata"
            );
            assert_eq!(
                forged_program
                    .type_catalog()
                    .get(&power_type)
                    .expect("forged descriptor keeps canonical ID")
                    .id,
                power_type
            );
            let errors = validate_mir_capabilities(&forged_program)
                .expect_err("unsigned and narrow integer profiles must fail the capability gate");
            assert!(
                errors.iter().any(|error| error
                    .contains("scalar TypeDesc is outside the verifier scalar contract")),
                "forged ABI {forged_abi:?} must be rejected by the MIR verifier capability gate, got {errors:?}"
            );
        }
    }

    #[test]
    fn rejects_non_copy_record_update_without_transfer_contract() {
        let program = canonical(
            r#"
                type Box { text: string, count: i32 }

                func rewrite(value: Box, next_count: i32) -> Box {
                    ensures: result.count == next_count
                    let updated = Box { count: next_count, ..value };
                    updated
                }

                func main() -> i32 { 0 }
            "#,
        );
        let errors = validate_mir_capabilities(&program)
            .expect_err("unreceipted non-Copy update must stay outside the proof island");
        assert!(errors.iter().any(|error| {
            error.contains("non-Copy record update requires a validated move receipt")
        }));
    }

    #[test]
    fn actor_fast_path_rejects_forged_any_body_before_neutral_return() {
        let source = r#"
            actor Counter {
                count: i32 = 0;
                func get() -> i32 { self.count }
            }

            func carry<T>(value: T) -> T { value }

            // This ordinary Map/ManagedAny helper stays outside the scalar
            // Actor execution graph until the test adds its canonical MIR.
            func any_probe() -> i32 {
                let empty = map_new()
                let root = map_set(empty, "zero", 0)
                let pair = map_get(root, "zero")
                drop(empty)
                drop(root)
                let found = pair.0
                let payload = pair.1
                let carried = carry(payload)
                let result = if found { carried } else { 0 }
                drop(pair)
                drop(payload)
                drop(carried)
                result
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mut program = MirProgram::from_checked_program(&checked)
            .expect("canonical scalar Actor graph materializes independently of any_probe");
        assert!(program.actor_execution_plan().is_some());
        validate_mir_capabilities(&program)
            .expect("the unextended finite scalar Actor graph reaches its neutral result");

        let any_owner = NodeId("function:any_probe".into());
        let any_callable = checked.callable(&any_owner).expect("checked Any helper");
        let any_probe = crate::core::mir::lower::lower_callable_with_type_catalog(
            any_callable,
            program.type_catalog(),
        )
        .expect("ordinary Any helper lowers against the shared checked TypeDesc catalog");
        assert!(any_probe.values.values().any(|value| {
            program
                .type_catalog()
                .contains_dynamic_or_managed_any(&value.ty)
        }));
        let mut injected_functions =
            std::collections::BTreeMap::from([(any_owner.clone(), any_probe)]);
        let injected_instances = crate::core::mir::lower::materialize_concrete_generic_instances(
            &checked,
            program.type_catalog(),
            &mut injected_functions,
        )
        .expect("materialize the exact generic ManagedAny identity specialization");
        let identity = injected_instances
            .values()
            .find(|instance| instance.contract == MirGenericInstanceContract::ManagedAnyIdentity)
            .expect("Any helper has one Checker-sealed identity instance");
        let identity_owner = identity.function.clone();
        let any_probe = injected_functions
            .get(&any_owner)
            .expect("injected ordinary Any helper");
        let nested_any_type = any_probe
            .values
            .values()
            .map(|value| value.ty.clone())
            .find(|ty| {
                matches!(
                    program.type_catalog().get(ty).map(|descriptor| &descriptor.layout),
                    Some(crate::core::mir::types::MirLayout::Tuple(elements))
                        if elements.iter().any(|element| {
                            program.type_catalog().contains_dynamic_or_managed_any(element)
                        })
                )
            })
            .expect("Map GetAny creates a recursive Any-bearing tuple type");
        program
            .append_unreachable_functions_and_instances_for_test_only(
                &checked,
                injected_functions,
                injected_instances,
            )
            .expect("unreachable Any helper preserves the checked Actor graph and receipts");
        let identity_call_id = program
            .forge_managed_any_identity_call_arguments_for_test_only(
                &checked,
                &any_owner,
                &identity_owner,
                vec![nested_any_type],
            )
            .expect("forge only the Any identity call's whole-program instance binding");
        program
            .validate_actor_contracts()
            .expect("forged unreachable helper does not alter the finite Actor graph");
        program
            .validate_managed_any_trap_cleanup_instances()
            .expect("the Checker-sealed ManagedAny identity body itself remains valid");

        let errors = validate_mir_capabilities(&program).expect_err(
            "Actor's neutral fast path must reject the forged Any identity call binding",
        );
        assert!(
            errors.iter().any(|error| {
                error.contains("ManagedAny identity call")
                    && error.contains(identity_call_id.as_str())
                    && error.contains("type arguments disagree with instance")
            }),
            "the Actor fast path must reach its ManagedAny identity gate before neutral NoContracts: {errors:?}"
        );
    }

    #[test]
    fn admits_only_the_separately_receipted_map_any_owner_v1_type() {
        let program = canonical(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let inserted = map_set(root, "answer", 42)
                    let size = map_size(inserted)
                    drop(inserted)
                    size
                }
            "#,
        );
        assert!(program
            .functions()
            .values()
            .any(|function| { function.ownership.has_checker_map_any_owner_actions() }));
        let owner_ty = crate::core::mir::types::map_any_owner_v1_type_id();
        assert_eq!(
            program
                .type_catalog()
                .get(&owner_ty)
                .expect("MapAnyOwner v1 TypeDesc")
                .kind,
            MirTypeKind::MapAnyOwnerV1
        );
        assert!(program
            .type_catalog()
            .get(&crate::core::mir::types::map_root_type_id())
            .is_none());
        validate_mir_capabilities(&program)
            .expect("the independent MapAnyOwner v1 receipt and TypeDesc admit the profile");
    }

    #[test]
    fn rejects_orphan_or_forged_map_any_owner_v1_descriptors() {
        let owner_program = canonical(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let inserted = map_set(root, "answer", 42)
                    drop(inserted)
                    0
                }
            "#,
        );
        let owner_ty = crate::core::mir::types::map_any_owner_v1_type_id();
        let mut descriptor = owner_program
            .type_catalog()
            .get(&owner_ty)
            .expect("MapAnyOwner v1 TypeDesc")
            .clone();
        descriptor.glue.clone = MirGlueKind::MapAnyOwnerV1;

        let plain_program = canonical("func main() -> i32 { 0 }");
        let mut catalog = plain_program.type_catalog().clone();
        catalog.replace_for_test_only(owner_ty, descriptor);
        let orphan = MirProgram::with_type_catalog(plain_program.functions().clone(), catalog)
            .expect("unused synthetic TypeDesc remains structurally representable");
        let errors = validate_mir_capabilities(&orphan)
            .expect_err("an orphan owner descriptor cannot authorize the profile");
        assert!(errors.iter().any(|error| {
            error.contains(
                "MapAnyOwner v1 TypeDesc presence must match independent Checker owner receipts",
            )
        }));
        assert!(errors
            .iter()
            .any(|error| error.contains("MapAnyOwner v1 TypeDesc rejected")));
    }

    #[test]
    fn admits_ordinary_map_owned_any_v2_with_core_payload_selection_receipt() {
        let program = canonical(
            r#"
                type Label { text: string }

                func main() -> i32 {
                    let label = Label { text: "owned label" }
                    let empty = map_new()
                    let one = map_set(empty, "label", label)
                    drop(empty)
                    drop(one)
                    drop(label)
                    0
                }
            "#,
        );
        let receipt = program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .expect(
                "Core validates V2 Set entries against Checker actions, MIR values, and TypeDescs",
            )
            .expect("the concrete Label Set selects the V2 owner profile");
        assert_eq!(receipt.entries.len(), 1);
        assert_eq!(receipt.selected_types.len(), 1);
        assert_eq!(receipt.entries[0].checker_type, receipt.entries[0].mir_type);

        validate_mir_capabilities(&program)
            .expect("the verifier consumes the Core-owned V2 payload selection receipt");
    }

    #[test]
    fn ordinary_map_owned_any_v2_get_any_requires_exact_result_receipt_operands() {
        let mut program = canonical(
            r#"
                type Label { text: string }

                func observe() -> bool {
                    ensures: result == false
                    let label = Label { text: "owned" }
                    let empty = map_new()
                    let one = map_set(empty, "present", label)
                    let pair = map_get(one, "missing")
                    let found = pair.0
                    drop(pair)
                    drop(empty)
                    drop(one)
                    drop(label)
                    found
                }
                func main() -> i32 { 0 }
            "#,
        );
        validate_ordinary_map_owned_any_v2_get_any_receipts(&program)
            .expect("the exact GetAny result receipt is derived from Checker and MIR");

        let owner = NodeId("function:observe".into());
        let mut function = program
            .functions()
            .get(&owner)
            .expect("observe MIR function")
            .clone();
        let set_key = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::OrdinaryMapOwnedAnyV2Set { key, .. } => Some(key.clone()),
                _ => None,
            })
            .expect("Set key value");
        let get_any = function
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
                )
            })
            .expect("GetAny instruction");
        let MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { key, .. } = &mut get_any.kind else {
            unreachable!("selected GetAny instruction")
        };
        *key = set_key;
        program.replace_function_for_test_only(function);

        let error = validate_ordinary_map_owned_any_v2_get_any_receipts(&program)
            .expect_err("a GetAny key changed after Checker receipt must be rejected");
        assert!(
            error.contains("GetAny") || error.contains("result receipt"),
            "unexpected rejection: {error}"
        );
    }

    #[test]
    fn accepts_ordinary_map_owned_any_v2_get_any_without_values_descriptors() {
        let program = canonical(
            r#"
                type Label { text: string }

                func observe() -> bool {
                    ensures: result == false
                    let label = Label { text: "owned" }
                    let empty = map_new()
                    let one = map_set(empty, "present", label)
                    let pair = map_get(one, "missing")
                    let found = pair.0
                    drop(pair)
                    drop(empty)
                    drop(one)
                    drop(label)
                    found
                }
                func main() -> i32 { 0 }
            "#,
        );
        let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        let element_ty =
            crate::core::mir::types::ordinary_map_owned_any_v2_values_element_type_id();
        assert!(program.functions().values().all(|function| {
            !function
                .ownership
                .has_checker_ordinary_map_owned_any_v2_values_actions()
        }));
        assert!(program.type_catalog().get(&values_ty).is_none());
        assert!(program.type_catalog().get(&element_ty).is_none());
        validate_mir_capabilities(&program)
            .expect("GetAny-only V2 graphs do not require Values TypeDescs");
    }

    #[test]
    fn rejects_ordinary_map_owned_any_v2_values_action_without_canonical_descriptors() {
        let program = ordinary_map_owned_any_v2_values_program();
        assert!(program.functions().values().any(|function| {
            function
                .ownership
                .has_checker_ordinary_map_owned_any_v2_values_actions()
        }));

        let error = validate_ordinary_map_owned_any_v2_values_type_catalog(
            &program,
            &crate::core::mir::types::MirTypeCatalog::default(),
        )
        .expect_err("a Values action requires its canonical Values and element descriptors");
        assert!(error.contains("requires canonical Values and element TypeDescs"));
    }

    #[test]
    fn rejects_orphan_ordinary_map_owned_any_v2_values_descriptors_without_action() {
        let values_program = ordinary_map_owned_any_v2_values_program();
        let plain_program = canonical("func main() -> i32 { 0 }");
        let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        let element_ty =
            crate::core::mir::types::ordinary_map_owned_any_v2_values_element_type_id();
        let mut catalog = plain_program.type_catalog().clone();
        for ty in [&values_ty, &element_ty] {
            let descriptor = values_program
                .type_catalog()
                .get(ty)
                .expect("canonical Values TypeDesc pair")
                .clone();
            catalog.replace_for_test_only(ty.clone(), descriptor);
        }

        let error =
            validate_ordinary_map_owned_any_v2_values_type_catalog(&plain_program, &catalog)
                .expect_err(
                    "canonical Values descriptors cannot be orphaned from a Checker action",
                );
        assert!(error.contains("orphan canonical AnyOwner-v2 Values/element TypeDesc"));
    }

    #[test]
    fn ordinary_map_owned_any_v2_values_requires_exact_owner_root_binding_drop_and_selection() {
        let program = canonical(
            r#"
                type Label { text: string }

                func collect() -> i32 {
                    ensures: result == 2
                    let number = 9
                    let label = Label { text: "snapshot" }
                    let empty = map_new()
                    let with_z = map_set(empty, "z", number)
                    let with_a = map_set(with_z, "a", label)
                    let snapshot = values(with_a)
                    let changed = map_remove(with_a, "z")
                    let count = snapshot.len()
                    drop(snapshot)
                    drop(empty)
                    drop(with_z)
                    drop(with_a)
                    drop(changed)
                    drop(label)
                    count
                }
                func main() -> i32 { 0 }
            "#,
        );
        validate_ordinary_map_owned_any_v2_values_receipts(&program)
            .expect("the Core receipt binds every Values result boundary");
        let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        let receipts = program
            .ordinary_map_owned_any_v2_values_result_receipts()
            .expect("validated Checker Values result receipts");
        let source_values_ty = receipts
            .first()
            .expect("collect has a Values result")
            .checker_result_type
            .clone();
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            assert!(
                program
                    .type_catalog()
                    .validate_list_glue(&source_values_ty, operation)
                    .is_err(),
                "Checker List<DynamicAny> remains outside executable List glue for {operation:?}"
            );
        }
        let collect = program
            .functions()
            .get(&NodeId("function:collect".into()))
            .expect("collect MIR function");
        let receipt = program
            .ordinary_map_owned_any_v2_values_result_receipts()
            .expect("Core receipt table")
            .into_iter()
            .find(|receipt| receipt.owner == NodeId("function:collect".into()))
            .expect("collect Values result");
        let source_binding = receipt.binding.clone();
        let len_list = collect
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::ListOp {
                    operation: crate::core::mir::MirListOperation::Len,
                    list,
                    list_operation_contract: Some(_),
                    ..
                } => Some(list),
                _ => None,
            })
            .expect("snapshot.len has an exact List operation receipt");
        assert_eq!(
            collect.values.get(len_list).map(|value| &value.ty),
            Some(&values_ty),
            "the executable Len receiver uses the synthetic V2 Values TypeDesc"
        );
        assert!(
            collect
                .blocks
                .values()
                .flat_map(|block| block.instructions.iter())
                .any(|instruction| matches!(
                    &instruction.kind,
                    MirInstructionKind::Clone { result, source }
                        if result == len_list && source == &source_binding
                )),
            "the borrow clone preserves the synthetic V2 Values TypeDesc"
        );
        let assert_forged_receipt_rejected =
            |forged: crate::core::mir::OrdinaryMapOwnedAnyV2ValuesResultReceipt| {
                let error = forged
                    .validate_against_program(&program)
                    .expect_err("forged Values receipt identity must be rejected");
                assert!(
                    error.contains("Values") || error.contains("result"),
                    "unexpected receipt rejection: {error}"
                );
            };
        let mut forged_owner = receipt.clone();
        forged_owner.owner = NodeId("function:forged".into());
        assert_forged_receipt_rejected(forged_owner);
        let mut forged_instruction = receipt.clone();
        forged_instruction.instruction = crate::core::mir::MirInstructionId::new("inst:forged")
            .expect("well-formed forged instruction identity");
        assert_forged_receipt_rejected(forged_instruction);
        let mut forged_root = receipt.clone();
        forged_root.root = crate::core::mir::MirValueId::new("local:forged_root")
            .expect("well-formed forged root identity");
        assert_forged_receipt_rejected(forged_root);
        let mut forged_result = receipt.clone();
        forged_result.result = crate::core::mir::MirValueId::new("expr:forged_result")
            .expect("well-formed forged result identity");
        assert_forged_receipt_rejected(forged_result);
        let mut forged_binding = receipt.clone();
        forged_binding.binding = crate::core::mir::MirValueId::new("local:forged_binding")
            .expect("well-formed forged binding identity");
        assert_forged_receipt_rejected(forged_binding);
        let mut forged_drop = receipt.clone();
        forged_drop.settlement_instruction =
            crate::core::mir::MirInstructionId::new("inst:drop.forged")
                .expect("well-formed forged settlement identity");
        assert_forged_receipt_rejected(forged_drop);
        let mut forged_selection = receipt.clone();
        forged_selection.payload_selection_digest = "0".repeat(64);
        assert_forged_receipt_rejected(forged_selection);

        let mut forged_program = program.clone();
        let mut function = forged_program
            .functions()
            .get(&receipt.owner)
            .expect("collect MIR function")
            .clone();
        let alternate_root = function
            .values
            .iter()
            .find_map(|(value, info)| {
                (info.ty == crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
                    && value != &receipt.root)
                    .then(|| value.clone())
            })
            .expect("another V2 root for root-tamper test");
        let values_instruction = function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| instruction.id == receipt.instruction)
            .expect("the receipt-bound Values instruction");
        let crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Values { root, .. } =
            &mut values_instruction.kind
        else {
            panic!("receipt instruction must be Values")
        };
        *root = alternate_root;
        forged_program.replace_function_for_test_only(function);
        let error = validate_ordinary_map_owned_any_v2_values_receipts(&forged_program)
            .expect_err("a changed Values source root must fail the exact Checker receipt");
        assert!(
            error.contains("Values") || error.contains("root"),
            "{error}"
        );

        let mut forged_program = program.clone();
        let mut function = forged_program
            .functions()
            .get(&receipt.owner)
            .expect("collect MIR function")
            .clone();
        let set_keys = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Set { key, .. } => {
                    Some(key.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            set_keys.len() >= 2,
            "fixture has two independently keyed Set sites"
        );
        let forged_set = function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. }
                )
            })
            .expect("first V2 Set");
        let crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Set { key, .. } =
            &mut forged_set.kind
        else {
            unreachable!("selected V2 Set")
        };
        *key = set_keys[1].clone();
        forged_program.replace_function_for_test_only(function);
        let error = validate_ordinary_map_owned_any_v2_values_receipts(&forged_program)
            .expect_err("a Checker-selected Map key changed after lowering must be rejected");
        assert!(
            error.contains("Set") || error.contains("payload selection"),
            "unexpected key-tamper rejection: {error}"
        );

        let mut forged_selection = program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .expect("payload selection receipt")
            .expect("Set entries select the payload profile");
        forged_selection.selection_digest = "f".repeat(64);
        assert!(forged_selection.validate_against_program(&program).is_err());
    }

    #[test]
    fn rejects_ordinary_map_owned_any_v2_values_invalid_clone_and_binding_settlement_paths() {
        let program = ordinary_map_owned_any_v2_values_program();
        let receipt = program
            .ordinary_map_owned_any_v2_values_result_receipts()
            .expect("Core Values receipt")
            .into_iter()
            .find(|receipt| receipt.owner == NodeId("function:collect".into()))
            .expect("collect Values result");
        let values_ty = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        let source_function = program
            .functions()
            .get(&receipt.owner)
            .expect("collect MIR function");
        let (source_block_id, clone_value) = source_function
            .blocks
            .iter()
            .find_map(|(block_id, block)| {
                block
                    .instructions
                    .iter()
                    .find_map(|instruction| match &instruction.kind {
                        MirInstructionKind::Clone { result, source }
                            if source == &receipt.binding
                                && source_function
                                    .values
                                    .get(result)
                                    .is_some_and(|value| value.ty == values_ty) =>
                        {
                            Some((block_id.clone(), result.clone()))
                        }
                        _ => None,
                    })
            })
            .expect("snapshot.len has one receipt-bound Values borrow clone");

        let mut drop_before_len = program.clone();
        let mut function = drop_before_len
            .functions()
            .get(&receipt.owner)
            .expect("collect MIR function")
            .clone();
        let block = function
            .blocks
            .get_mut(&source_block_id)
            .expect("Values borrow clone block");
        let len_index = block
            .instructions
            .iter()
            .position(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::ListOp {
                        operation: crate::core::mir::MirListOperation::Len,
                        list,
                        ..
                    } if list == &clone_value
                )
            })
            .expect("borrow clone Len");
        let drop_index = block
            .instructions
            .iter()
            .position(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Drop { value } if value == &clone_value
                )
            })
            .expect("borrow clone Drop");
        assert!(len_index < drop_index, "fixture emits Len before Drop");
        let drop = block.instructions.remove(drop_index);
        block.instructions.insert(len_index, drop);
        drop_before_len.replace_function_for_test_only(function);
        let error = drop_before_len
            .ordinary_map_owned_any_v2_values_result_receipts()
            .expect_err("a Values borrow clone cannot be read after it is dropped");
        assert!(
            error.contains("in that order"),
            "unexpected rejection: {error}"
        );

        let mut bypassed_binding_drop = program.clone();
        let mut function = bypassed_binding_drop
            .functions()
            .get(&receipt.owner)
            .expect("collect MIR function")
            .clone();
        let binding_drop_site = function.blocks.iter().find_map(|(block_id, block)| {
            block
                .instructions
                .iter()
                .position(|instruction| {
                    instruction.id == receipt.settlement_instruction
                        && matches!(
                            &instruction.kind,
                            MirInstructionKind::Drop { value } if value == &receipt.binding
                        )
                })
                .map(|index| (block_id.clone(), index))
        });
        let (binding_drop_block, binding_drop_index) =
            binding_drop_site.expect("explicit Values binding Drop");
        let binding_drop = function
            .blocks
            .get_mut(&binding_drop_block)
            .expect("binding Drop block")
            .instructions
            .remove(binding_drop_index);
        let bypass_block_id =
            crate::core::mir::MirBlockId::new("block:forged_values_binding_drop_bypass")
                .expect("canonical test block id");
        let drop_block_id =
            crate::core::mir::MirBlockId::new("block:forged_values_binding_drop_branch")
                .expect("canonical test block id");
        let condition = crate::core::mir::MirValueId::new("value:forged_drop_branch")
            .expect("canonical test value id");
        function.values.insert(
            condition.clone(),
            crate::core::mir::MirValue {
                id: condition.clone(),
                ty: crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool),
            },
        );
        function
            .blocks
            .get_mut(&source_block_id)
            .expect("Values borrow clone block")
            .terminator = crate::core::mir::MirTerminator::Branch {
            condition,
            then_edge: crate::core::mir::MirEdgeId::new("edge:forged_values_drop")
                .expect("canonical test edge id"),
            then_target: drop_block_id.clone(),
            then_arguments: Vec::new(),
            else_edge: crate::core::mir::MirEdgeId::new("edge:forged_values_drop_bypass")
                .expect("canonical test edge id"),
            else_target: bypass_block_id.clone(),
            else_arguments: Vec::new(),
        };
        function.blocks.insert(
            drop_block_id.clone(),
            crate::core::mir::MirBlock {
                id: drop_block_id,
                parameters: Vec::new(),
                instructions: vec![binding_drop],
                terminator: crate::core::mir::MirTerminator::Unreachable,
            },
        );
        function.blocks.insert(
            bypass_block_id.clone(),
            crate::core::mir::MirBlock {
                id: bypass_block_id,
                parameters: Vec::new(),
                instructions: Vec::new(),
                terminator: crate::core::mir::MirTerminator::Unreachable,
            },
        );
        bypassed_binding_drop.replace_function_for_test_only(function);
        let error = bypassed_binding_drop
            .ordinary_map_owned_any_v2_values_result_receipts()
            .expect_err("a branch cannot bypass Values binding settlement");
        assert!(
            error.contains("postdominating"),
            "unexpected rejection: {error}"
        );

        let mut split_blocks = program.clone();
        let mut function = split_blocks
            .functions()
            .get(&receipt.owner)
            .expect("collect MIR function")
            .clone();
        let block = function
            .blocks
            .get_mut(&source_block_id)
            .expect("Values borrow clone block");
        let drop_index = block
            .instructions
            .iter()
            .position(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Drop { value } if value == &clone_value
                )
            })
            .expect("borrow clone Drop");
        let drop = block.instructions.remove(drop_index);
        let forged_block_id = crate::core::mir::MirBlockId::new("block:forged_values_borrow_drop")
            .expect("canonical test block id");
        function.blocks.insert(
            forged_block_id.clone(),
            crate::core::mir::MirBlock {
                id: forged_block_id.clone(),
                parameters: Vec::new(),
                instructions: vec![drop],
                terminator: crate::core::mir::MirTerminator::Unreachable,
            },
        );
        split_blocks.replace_function_for_test_only(function);
        let error = split_blocks
            .ordinary_map_owned_any_v2_values_result_receipts()
            .expect_err("a Values borrow clone settlement cannot be split across MIR blocks");
        assert!(
            error.contains("in that order"),
            "unexpected rejection: {error}"
        );

        let mut use_after_drop = program.clone();
        let mut function = use_after_drop
            .functions()
            .get(&receipt.owner)
            .expect("collect MIR function")
            .clone();
        let binding_drop_site = function.blocks.iter().find_map(|(block_id, block)| {
            block
                .instructions
                .iter()
                .position(|instruction| {
                    instruction.id == receipt.settlement_instruction
                        && matches!(
                            &instruction.kind,
                            MirInstructionKind::Drop { value } if value == &receipt.binding
                        )
                })
                .map(|index| (block_id.clone(), index))
        });
        let (binding_drop_block, binding_drop_index) =
            binding_drop_site.expect("explicit Values binding Drop");
        let binding_drop = function
            .blocks
            .get_mut(&binding_drop_block)
            .expect("binding Drop block")
            .instructions
            .remove(binding_drop_index);
        let use_block_id =
            crate::core::mir::MirBlockId::new("block:forged_values_reuse_after_drop")
                .expect("canonical test block id");
        let after_drop_block_id =
            crate::core::mir::MirBlockId::new("block:forged_values_after_drop")
                .expect("canonical test block id");
        let use_instructions = {
            let source_block = function
                .blocks
                .get_mut(&source_block_id)
                .expect("Values borrow clone block");
            let clone_index = source_block
                .instructions
                .iter()
                .position(|instruction| {
                    matches!(
                        &instruction.kind,
                        MirInstructionKind::Clone { result, source }
                            if result == &clone_value && source == &receipt.binding
                    )
                })
                .expect("Values borrow clone instruction");
            source_block.instructions.split_off(clone_index)
        };
        function
            .blocks
            .get_mut(&source_block_id)
            .expect("Values borrow clone block")
            .terminator = crate::core::mir::MirTerminator::Goto {
            edge: crate::core::mir::MirEdgeId::new("edge:forged_values_to_reuse")
                .expect("canonical test edge id"),
            target: use_block_id.clone(),
            arguments: Vec::new(),
        };
        function.blocks.insert(
            use_block_id.clone(),
            crate::core::mir::MirBlock {
                id: use_block_id.clone(),
                parameters: Vec::new(),
                instructions: use_instructions,
                terminator: crate::core::mir::MirTerminator::Goto {
                    edge: crate::core::mir::MirEdgeId::new("edge:forged_values_to_drop")
                        .expect("canonical test edge id"),
                    target: after_drop_block_id.clone(),
                    arguments: Vec::new(),
                },
            },
        );
        function.blocks.insert(
            after_drop_block_id.clone(),
            crate::core::mir::MirBlock {
                id: after_drop_block_id,
                parameters: Vec::new(),
                instructions: vec![binding_drop],
                terminator: crate::core::mir::MirTerminator::Goto {
                    edge: crate::core::mir::MirEdgeId::new("edge:forged_values_backedge")
                        .expect("canonical test edge id"),
                    target: use_block_id,
                    arguments: Vec::new(),
                },
            },
        );
        use_after_drop.replace_function_for_test_only(function);
        let error = use_after_drop
            .ordinary_map_owned_any_v2_values_result_receipts()
            .expect_err("a backedge cannot reuse a Values binding after settlement");
        assert!(
            error.contains("reachable after its explicit Drop"),
            "unexpected rejection: {error}"
        );
    }

    #[test]
    fn rejects_ordinary_map_owned_any_v2_type_without_selection_receipt() {
        let v2_program = canonical(
            r#"
                type Label { text: string }

                func main() -> i32 {
                    let label = Label { text: "owned label" }
                    let empty = map_new()
                    let one = map_set(empty, "label", label)
                    drop(empty)
                    drop(one)
                    drop(label)
                    0
                }
            "#,
        );
        let ordinary_program = canonical("func main() -> i32 { 0 }");
        let mut forged = v2_program;
        let ordinary_main = ordinary_program
            .functions()
            .values()
            .find(|function| function.owner == NodeId("function:main".into()))
            .expect("ordinary main function")
            .clone();
        // Preserve the V2 catalog while replacing its action-bearing body, as
        // a focused test of the verifier preflight's missing-receipt branch.
        forged.replace_function_for_test_only(ordinary_main);

        let error = validate_ordinary_map_owned_any_v2_payload_selection(&forged)
            .expect_err("a V2 TypeDesc without a Checker-derived selection must fail closed");
        assert!(error.contains("V2 signal has no Core-owned Checker payload selection receipt"));
    }

    #[test]
    fn accepts_copy_record_and_scalar_call_graph() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_record_copy.mimi"
        ));
        validate_mir_capabilities(&program).expect("copy record island capability");
    }

    #[test]
    fn rejects_forged_scalar_descriptor_before_capability_value_checks() {
        let source = "func main() -> i64 { 0 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let i64_id = checked
            .resolved_types()
            .iter()
            .find_map(|(id, ty)| {
                matches!(ty, ResolvedType::Primitive(PrimitiveType::I64)).then_some(id.clone())
            })
            .expect("i64 TypeDesc identity");
        let descriptor = program
            .type_catalog()
            .get(&i64_id)
            .expect("i64 TypeDesc")
            .clone();
        assert!(descriptor.is_canonical_copy_scalar(false));
        let mut forged = descriptor;
        forged.kind = MirTypeKind::Nominal;
        let mut catalog = program.type_catalog().clone();
        catalog.replace_for_test_only(i64_id, forged);
        let scalar_id = checked
            .resolved_types()
            .iter()
            .find_map(|(id, ty)| {
                matches!(ty, ResolvedType::Primitive(PrimitiveType::I64)).then_some(id.clone())
            })
            .expect("forged scalar TypeDesc identity");
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            assert!(
                !value_operation_is_supported(&catalog, &scalar_id, operation),
                "malformed Copy scalar must not fall back to {:?} glue",
                operation
            );
        }
        let forged_program = MirProgram::with_type_catalog(program.functions().clone(), catalog)
            .expect("forged catalog remains structurally valid");

        let errors = validate_mir_capabilities(&forged_program)
            .expect_err("forged scalar TypeDesc must fail capability admission");
        assert!(errors.iter().any(|error| {
            error.contains("scalar TypeDesc is outside the verifier scalar contract")
        }));
    }

    #[test]
    fn rejects_forged_copy_aggregate_metadata_before_capability_consumption() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_record_copy.mimi"
        ));
        let record_id = program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (matches!(descriptor.layout, MirLayout::Record { .. })
                    && descriptor.ownership == MirOwnership::Copy)
                    .then(|| ty.clone())
            })
            .expect("Copy record TypeDesc");
        let descriptor = program
            .type_catalog()
            .get(&record_id)
            .expect("Copy record descriptor")
            .clone();
        assert!(descriptor.has_canonical_copy_noop_metadata());

        for mutation in 0..4 {
            let mut catalog = program.type_catalog().clone();
            let mut forged = descriptor.clone();
            match mutation {
                0 => forged.session_protocol = Some(record_id.clone()),
                1 => forged.needs_drop_glue = true,
                2 => {
                    forged.drop_plan =
                        Some(crate::core::mir::types::MirDropGluePlan { fields: Vec::new() })
                }
                3 => forged.variant_drop_plan = Some(Vec::new()),
                _ => unreachable!(),
            }
            catalog.replace_for_test_only(record_id.clone(), forged);
            let forged_program =
                MirProgram::with_type_catalog(program.functions().clone(), catalog)
                    .expect("forged catalog remains structurally valid");
            let errors = validate_mir_capabilities(&forged_program)
                .expect_err("forged Copy aggregate metadata must fail capability admission");
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("is not Copy with canonical no-op glue")),
                "unexpected errors: {errors:?}"
            );
        }
    }

    #[test]
    fn accepts_non_copy_option_string_variant_contract() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_option_string.mimi"
        ));
        validate_mir_capabilities(&program).expect("Option<string> verifier capability");
    }

    #[test]
    fn accepts_recursive_owned_tuple_contract() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_recursive_tuple.mimi"
        ));
        validate_mir_capabilities(&program).expect("recursive tuple verifier capability");
        let tuple = program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (matches!(descriptor.layout, MirLayout::Tuple(ref fields) if fields.len() == 2)
                    && descriptor.ownership == MirOwnership::Move)
                    .then(|| ty.clone())
            })
            .expect("recursive Move tuple TypeDesc");
        program
            .type_catalog()
            .validate_recursive_tuple_abi(&tuple)
            .expect("shared recursive tuple TypeDesc contract");
    }

    #[test]
    fn rejects_recursive_tuple_with_list_child_before_verifier_consumption() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_recursive_tuple_rejected.mimi"
        ));
        let errors = validate_mir_capabilities(&program)
            .expect_err("tuple with List child must remain outside verifier capability");
        assert!(errors
            .iter()
            .any(|error| { error.contains("scalar/String/tuple ABI") || error.contains("List") }));
    }

    #[test]
    fn rejects_unsupported_non_copy_variant_before_default_route() {
        let tokens = Lexer::new(
            "func main() -> i32 { let value: Option<List<string>> = Some([\"owned\"]); drop(value); 42 }",
        )
        .tokenize()
        .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("Option<List<string>> lowers before verifier capability check");
        let errors = validate_mir_capabilities(&program)
            .expect_err("unsupported Option<List<string>> must fail verifier capability gate");
        assert!(errors.iter().any(|error| error.contains("List")));
    }

    #[test]
    fn accepts_indexed_list_projection_with_materialized_receipt() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_list_index.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("List index receipt must satisfy verifier capability gate");
    }

    #[test]
    fn accepts_list_operation_with_materialized_receipt() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_list_len.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("List operation receipt must satisfy verifier capability gate");
    }

    #[test]
    fn accepts_one_level_nested_list_construct_and_recursive_glue() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_generic_list_nested_owned.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("nested List construction must satisfy verifier capability gate");
        let nested = program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                matches!(descriptor.layout, MirLayout::List { ref element }
                if program.type_catalog().get(element).is_some_and(|child| {
                    matches!(child.layout, MirLayout::List { .. })
                }))
                .then(|| ty.clone())
            })
            .expect("nested List TypeDesc");
        program
            .type_catalog()
            .validate_nested_list_payload(&nested)
            .expect("nested List TypeDesc glue");
    }

    #[test]
    fn accepts_nested_list_len_without_transfer_or_projection() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_nested_list_len.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("nested List.len must satisfy verifier capability gate");
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ListOp {
                            operation: crate::core::mir::MirListOperation::Len,
                            ..
                        }
                    )
                })
            })
        }));
    }

    #[test]
    fn accepts_nested_list_index_with_materialized_clone_receipt() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_nested_list_index.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("nested List index clone receipt must satisfy verifier capability gate");
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::Project {
                            projection: crate::core::mir::MirProjection::Index(_),
                            list_index_contract: Some(ref receipt),
                            ..
                        } if receipt.mode
                            == crate::core::mir::types::MirListIndexProjectionMode::CloneNestedList
                    )
                })
            })
        }));
    }

    #[test]
    fn accepts_nested_list_reverse_with_materialized_recursive_clone_receipt() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_nested_list_reverse.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("nested List.reverse clone receipt must satisfy verifier capability gate");
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ListOp {
                            operation: crate::core::mir::MirListOperation::Reverse,
                            list_operation_contract: Some(ref receipt),
                            ..
                        } if receipt.mode
                            == crate::core::mir::types::MirListOperationMode::Nested
                    )
                })
            })
        }));
    }

    #[test]
    fn accepts_nested_list_concat_with_materialized_child_move_receipt() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_nested_list_concat.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("nested List.concat move receipt must satisfy verifier capability gate");
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ListOp {
                            operation: crate::core::mir::MirListOperation::Concat,
                            list_operation_contract: Some(ref receipt),
                            ..
                        } if receipt.mode
                            == crate::core::mir::types::MirListOperationMode::Nested
                    )
                })
            })
        }));
    }
}
