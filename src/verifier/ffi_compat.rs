//! Checker-bound verifier-only MIR projection for FFI compatibility callers.
//!
//! This module owns the compatibility boundary between checked FFI roots and
//! per-caller MIR proofs. It never reads surface AST bodies and it never
//! creates an execution route. Checker declaration-root handles, body call
//! sites, the source-only census, and the attached MIR census must agree
//! before a proof consumer runs.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::lower::{ResolvedDeclarationExpressionKind, ResolvedDeclarationRootIdentity};
use crate::core::ir::{ResolvedCallee, ResolvedExprKind, ResolvedFfiContractRoot};
use crate::core::mir::reference::{MirProgram, MirProgramBuildError, MirProgramPurpose};
use crate::core::mir::{CanonicalMirRouteReceipt, MirInstructionId, MirProofSourceCensus};
use crate::core::{CheckedProgram, NodeId, ResolvedCallKind, ResolvedCallSite};
use crate::verifier::VerificationResult;

const MIR_FFI_OWNER_SLICE_ROUTE_PROFILE: &str = "verifier-ffi-owner-slice-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
struct FfiRootCallIdentity {
    root: NodeId,
    call: NodeId,
    declaration: NodeId,
    owner: NodeId,
    symbol: String,
    origin: crate::core::Origin,
    fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FfiRootIdentity {
    root: NodeId,
    owner: NodeId,
    kind: ResolvedDeclarationExpressionKind,
    identity: Option<ResolvedDeclarationRootIdentity>,
    origin: Option<crate::core::Origin>,
    body_fingerprint: String,
    call_census_valid: bool,
    calls: Vec<FfiRootCallIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckerFfiSourceIdentity {
    roots: Vec<FfiRootIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FfiBodyCallIdentity {
    owner: NodeId,
    call: NodeId,
    declaration: NodeId,
    symbol: String,
    argc: usize,
    span: crate::span::Span,
}

/// Which already-admitted Checker body shape is projected into verifier-only
/// MIR. These variants do not grant eligibility; callers must first establish
/// the matching isolated-owner shape.
pub(crate) enum FfiCallerProjectionShape<'a> {
    TopLevel,
    TopLevelWithHelper {
        helper_owner: &'a NodeId,
        helper_call: &'a NodeId,
    },
    FlowTransition {
        call: &'a NodeId,
    },
}

/// A sealed non-executable FFI caller projection. The original owner and
/// Checker call identities stay attached through proof consumption.
pub(crate) struct FfiVerifierOnlyProjection {
    owner: NodeId,
    checker_source: CheckerFfiSourceIdentity,
    body_calls: Vec<FfiBodyCallIdentity>,
    mir: MirProgram,
    receipt: CanonicalMirRouteReceipt,
}

/// Check that the exact FFI caller owners in a typed construction boundary
/// all materialize as verifier-only checker-bound slices. This is a
/// construction-shape predicate only and never admits native or bytecode MIR.
pub(crate) fn all_top_level_callers_project(
    program: &CheckedProgram,
    owners: &BTreeSet<NodeId>,
    called_contract_names: &BTreeSet<String>,
) -> bool {
    if owners.is_empty() {
        return false;
    }
    let declaration_root_calls = program
        .checked_ffi_source_roots()
        .iter()
        .flat_map(|root| root.calls().iter().map(|call| call.call_node().clone()))
        .collect::<BTreeSet<_>>();
    for owner in owners {
        let sites = program
            .call_sites_sorted()
            .into_iter()
            .filter(|site| {
                site.owner == owner.0
                    && site.kind == ResolvedCallKind::Extern
                    && called_contract_names.contains(&site.callee)
                    && !declaration_root_calls.contains(&site.node_id)
            })
            .map(|site| (*site).clone())
            .collect::<Vec<_>>();
        let Ok(projection) = FfiVerifierOnlyProjection::from_checked_caller(
            program,
            owner,
            &sites,
            FfiCallerProjectionShape::TopLevel,
        ) else {
            return false;
        };
        if projection.mir().purpose() != MirProgramPurpose::VerifierOnly {
            return false;
        }
    }
    true
}

impl CheckerFfiSourceIdentity {
    fn capture(program: &CheckedProgram) -> Result<Self, String> {
        let mut roots = Vec::new();
        let mut seen_roots = BTreeSet::new();
        let mut seen_calls = BTreeMap::<NodeId, NodeId>::new();

        for root in program.checked_ffi_source_roots() {
            if !seen_roots.insert(root.root_node().clone()) {
                return Err(format!(
                    "TOOL-RESOLUTION-001: duplicate Checker FFI source-root identity '{}'",
                    root.root_node().0
                ));
            }
            program.checked_ffi_source_root_body(root).map_err(|_| {
                format!(
                    "TOOL-RESOLUTION-001: FFI source root '{}' no longer resolves through its Checker handle",
                    root.root_node().0
                )
            })?;

            let mut calls = Vec::new();
            for call in root.calls() {
                if let Some(previous) =
                    seen_calls.insert(call.call_node().clone(), root.root_node().clone())
                {
                    return Err(format!(
                        "TOOL-RESOLUTION-001: FFI source call '{}' is cross-bound between roots '{}' and '{}'",
                        call.call_node().0,
                        previous.0,
                        root.root_node().0
                    ));
                }
                let context = program
                    .checked_ffi_source_call_context(root, call)
                    .map_err(|_| {
                        format!(
                            "TOOL-RESOLUTION-001: FFI source call '{}' no longer resolves through its Checker root/call handles",
                            call.call_node().0
                        )
                    })?;
                let site = program.call_sites().get(call.call_node()).ok_or_else(|| {
                    format!(
                        "TOOL-RESOLUTION-001: FFI source call '{}' has no checked call-site identity",
                        call.call_node().0
                    )
                })?;
                let site_count = program
                    .call_sites()
                    .values()
                    .filter(|candidate| candidate.node_id == *call.call_node())
                    .count();
                let ResolvedExprKind::Call(typed_call) = &context.call.kind else {
                    return Err(format!(
                        "TOOL-RESOLUTION-001: FFI source call '{}' no longer resolves to a typed Call",
                        call.call_node().0
                    ));
                };
                let declaration = program
                    .ffi_declarations()
                    .get(call.external_declaration())
                    .ok_or_else(|| {
                        format!(
                            "TOOL-RESOLUTION-001: FFI source call '{}' lost Checker declaration '{}'",
                            call.call_node().0,
                            call.external_declaration().0
                        )
                    })?;
                declaration
                    .validate(program)
                    .map_err(|errors| format!("TOOL-RESOLUTION-001: {errors:?}"))?;
                if site_count != 1
                    || site.node_id != *call.call_node()
                    || site.owner != root.owner().0
                    || site.kind != ResolvedCallKind::Extern
                    || site.callee != context.declaration.symbol
                    || site.target_function.is_some()
                    || site.resolved_callee.as_ref().is_some_and(|resolved| {
                        resolved != &ResolvedCallee::Extern(call.external_declaration().clone())
                    })
                    || context.declaration.declaration != *call.external_declaration()
                    || typed_call.callee
                        != ResolvedCallee::Extern(call.external_declaration().clone())
                    || site.origin != context.call.origin
                    || !site.checked_ffi_source_arity_matches(
                        typed_call.arguments.len(),
                        context.declaration,
                    )
                {
                    return Err(format!(
                        "TOOL-RESOLUTION-001: FFI root '{}' call '{}' disagrees with its Checker declaration/call/span identity",
                        root.root_node().0,
                        call.call_node().0
                    ));
                }
                calls.push(FfiRootCallIdentity {
                    root: root.root_node().clone(),
                    call: call.call_node().clone(),
                    declaration: call.external_declaration().clone(),
                    owner: root.owner().clone(),
                    symbol: context.declaration.symbol.clone(),
                    origin: context.call.origin.clone(),
                    fingerprint: blake3::hash(format!("{:#?}", context.call).as_bytes())
                        .to_hex()
                        .to_string(),
                });
            }
            calls.sort_by(|left, right| left.call.cmp(&right.call));
            roots.push(FfiRootIdentity {
                root: root.root_node().clone(),
                owner: root.owner().clone(),
                kind: root.kind(),
                identity: root.identity().cloned(),
                origin: root.origin().cloned(),
                body_fingerprint: root.body_fingerprint().to_hex().to_string(),
                call_census_valid: root.has_valid_call_census(),
                calls,
            });
        }
        roots.sort_by(|left, right| left.root.cmp(&right.root));

        let identity = Self { roots };
        let source_census = MirProofSourceCensus::checked_ffi_declaration_roots(program);
        source_census
            .validate_ffi_source_roots()
            .map_err(|errors| {
                format!(
                "MIR-COVERAGE-001: Checker FFI declaration-root census is incomplete: {errors:?}"
            )
            })?;
        let census_identity = Self::from_census(&source_census);
        if identity != census_identity {
            return Err(
                "MIR-COVERAGE-001: Checker FFI source roots disagree with their independent source census"
                    .into(),
            );
        }
        Ok(identity)
    }

    fn from_census(census: &MirProofSourceCensus) -> Self {
        let mut roots = census
            .ffi_source_roots()
            .iter()
            .map(|root| {
                let calls = root
                    .calls()
                    .iter()
                    .map(|call| FfiRootCallIdentity {
                        root: call.root_node().clone(),
                        call: call.call_node().clone(),
                        declaration: call.declaration().clone(),
                        owner: root.owner().clone(),
                        symbol: call.symbol().to_owned(),
                        origin: call.call_origin().clone(),
                        fingerprint: call.call_fingerprint().to_owned(),
                    })
                    .collect();
                FfiRootIdentity {
                    root: root.root_node().clone(),
                    owner: root.owner().clone(),
                    kind: root.kind(),
                    identity: root.identity().cloned(),
                    origin: root.origin().cloned(),
                    body_fingerprint: root.body_fingerprint().to_owned(),
                    call_census_valid: root.call_census_valid(),
                    calls,
                }
            })
            .collect::<Vec<_>>();
        for root in &mut roots {
            root.calls.sort_by(|left, right| left.call.cmp(&right.call));
        }
        roots.sort_by(|left, right| left.root.cmp(&right.root));
        Self { roots }
    }

    fn declaration_root_calls(&self) -> BTreeSet<NodeId> {
        self.roots
            .iter()
            .flat_map(|root| root.calls.iter().map(|call| call.call.clone()))
            .collect()
    }

    fn validate_mir(&self, mir: &MirProgram) -> Result<(), String> {
        mir.validate_proof_source_census().map_err(|error| {
            format!("MIR-COVERAGE-001: FFI MIR source census is invalid: {error:?}")
        })?;
        mir.validate_ffi_source_census().map_err(|errors| {
            format!("MIR-COVERAGE-001: FFI MIR source/call census is invalid: {errors:?}")
        })?;
        let mir_identity = Self::from_census(mir.proof_source_census());
        if &mir_identity != self {
            return Err(
                "MIR-COVERAGE-001: canonical FFI MIR does not preserve the exact Checker root/call census"
                    .into(),
            );
        }
        Ok(())
    }
}

impl FfiVerifierOnlyProjection {
    pub(crate) fn from_checked_caller(
        program: &CheckedProgram,
        owner: &NodeId,
        requested_sites: &[ResolvedCallSite],
        shape: FfiCallerProjectionShape<'_>,
    ) -> Result<Self, String> {
        let checker_source = CheckerFfiSourceIdentity::capture(program)?;
        let body_calls = checked_body_calls(program, owner, &checker_source)?;
        validate_requested_sites(program, owner, requested_sites, &body_calls)?;

        let mir = match shape {
            FfiCallerProjectionShape::TopLevel => {
                MirProgram::from_checked_program_ffi_caller(program, owner)
            }
            FfiCallerProjectionShape::TopLevelWithHelper {
                helper_owner,
                helper_call,
            } => MirProgram::from_checked_program_ffi_caller_with_helper(
                program,
                owner,
                helper_owner,
                helper_call,
            ),
            FfiCallerProjectionShape::FlowTransition { call } => {
                MirProgram::from_checked_program_ffi_flow_transition(program, owner, call)
            }
        }
        .map_err(|error| format_mir_build_error("caller", error))?;
        if mir.purpose() != MirProgramPurpose::VerifierOnly {
            return Err(
                "MIR-COVERAGE-001: isolated FFI caller projection is not verifier-only".into(),
            );
        }
        checker_source.validate_mir(&mir)?;
        validate_mir_body_calls(&mir, owner, &body_calls)?;

        let receipt = mir.route_receipt(MIR_FFI_OWNER_SLICE_ROUTE_PROFILE);
        receipt.validate_against_program(&mir).map_err(|message| {
            format!("MIR-COVERAGE-001: verifier-only FFI route receipt is invalid: {message}")
        })?;
        Ok(Self {
            owner: owner.clone(),
            checker_source,
            body_calls,
            mir,
            receipt,
        })
    }

    pub(crate) fn mir(&self) -> &MirProgram {
        &self.mir
    }

    #[cfg(test)]
    pub(crate) fn owner(&self) -> &NodeId {
        &self.owner
    }

    #[cfg(test)]
    pub(crate) fn receipt(&self) -> &CanonicalMirRouteReceipt {
        &self.receipt
    }

    pub(crate) fn verify(
        &self,
        program: &CheckedProgram,
        source_hash: String,
        selected_ensures: &BTreeSet<MirInstructionId>,
    ) -> Result<Vec<VerificationResult>, String> {
        let current_source = CheckerFfiSourceIdentity::capture(program)?;
        if current_source != self.checker_source {
            return Err(
                "MIR-COVERAGE-001: checked FFI source identity changed after verifier-only projection"
                    .into(),
            );
        }
        current_source.validate_mir(&self.mir)?;
        let current_body_calls = checked_body_calls(program, &self.owner, &current_source)?;
        if current_body_calls != self.body_calls {
            return Err(format!(
                "MIR-COVERAGE-001: FFI caller '{}' call identity changed after verifier-only projection",
                self.owner.0
            ));
        }
        validate_selected_ensures(program, &self.mir, selected_ensures)?;
        self.receipt
            .validate_against_program(&self.mir)
            .map_err(|message| {
                format!("MIR-COVERAGE-001: verifier-only FFI route receipt is invalid: {message}")
            })?;
        super::verify_ffi_mir_with_selected_ensures_route_receipt(
            &self.mir,
            &self.receipt,
            source_hash,
            selected_ensures,
        )
    }
}

fn checked_body_calls(
    program: &CheckedProgram,
    owner: &NodeId,
    source: &CheckerFfiSourceIdentity,
) -> Result<Vec<FfiBodyCallIdentity>, String> {
    let declaration_root_calls = source.declaration_root_calls();
    let mut calls = Vec::new();
    let mut seen = BTreeSet::new();
    for site in program.call_sites_sorted().into_iter().filter(|site| {
        site.owner == owner.0
            && site.kind == ResolvedCallKind::Extern
            && !declaration_root_calls.contains(&site.node_id)
    }) {
        if !seen.insert(site.node_id.clone()) {
            return Err(format!(
                "TOOL-RESOLUTION-001: duplicate FFI body call identity '{}' for owner '{}'",
                site.node_id.0, owner.0
            ));
        }
        let site_count = program
            .call_sites()
            .values()
            .filter(|candidate| candidate.node_id == site.node_id)
            .count();
        let Some(ResolvedCallee::Extern(declaration_id)) = site.resolved_callee.as_ref() else {
            return Err(format!(
                "TOOL-RESOLUTION-001: FFI body call '{}' has no exact Checker declaration identity",
                site.node_id.0
            ));
        };
        let declaration = program
            .ffi_declarations()
            .get(declaration_id)
            .ok_or_else(|| {
                format!(
                    "TOOL-RESOLUTION-001: FFI body call '{}' names an absent Checker declaration",
                    site.node_id.0
                )
            })?;
        declaration
            .validate(program)
            .map_err(|errors| format!("TOOL-RESOLUTION-001: {errors:?}"))?;
        if site_count != 1
            || site.node_id.0.trim().is_empty()
            || site.owner != owner.0
            || site.callee != declaration.symbol
            || site.target_function.is_some()
            || !site.arity_matches()
        {
            return Err(format!(
                "TOOL-RESOLUTION-001: FFI body call '{}' disagrees with its exact Checker owner/declaration/arity identity",
                site.node_id.0
            ));
        }
        calls.push(FfiBodyCallIdentity {
            owner: owner.clone(),
            call: site.node_id.clone(),
            declaration: declaration_id.clone(),
            symbol: declaration.symbol.clone(),
            argc: site.argc,
            span: site.origin.user_span(),
        });
    }
    calls.sort_by(|left, right| left.call.cmp(&right.call));
    Ok(calls)
}

fn validate_requested_sites(
    program: &CheckedProgram,
    owner: &NodeId,
    requested: &[ResolvedCallSite],
    expected: &[FfiBodyCallIdentity],
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for supplied in requested {
        let Some(current) = program.call_sites().get(&supplied.node_id) else {
            return Err(format!(
                "TOOL-RESOLUTION-001: requested FFI call '{}' disappeared from Checker call sites",
                supplied.node_id.0
            ));
        };
        if current.node_id != supplied.node_id
            || current.owner != supplied.owner
            || current.callee != supplied.callee
            || current.method_receiver != supplied.method_receiver
            || current.argc != supplied.argc
            || current.expected_argc != supplied.expected_argc
            || current.effects != supplied.effects
            || current.ret != supplied.ret
            || current.kind != supplied.kind
            || current.target_function != supplied.target_function
            || current.resolved_callee != supplied.resolved_callee
            || current.origin != supplied.origin
            || current.owner != owner.0
        {
            return Err(format!(
                "TOOL-RESOLUTION-001: requested FFI call '{}' is detached from caller '{}'",
                supplied.node_id.0, owner.0
            ));
        }
        let identity = FfiBodyCallIdentity {
            owner: owner.clone(),
            call: current.node_id.clone(),
            declaration: match current.resolved_callee.as_ref() {
                Some(ResolvedCallee::Extern(declaration)) => declaration.clone(),
                _ => {
                    return Err(format!(
                        "TOOL-RESOLUTION-001: requested FFI call '{}' has no exact extern declaration identity",
                        current.node_id.0
                    ));
                }
            },
            symbol: current.callee.clone(),
            argc: current.argc,
            span: current.origin.user_span(),
        };
        if !expected.iter().any(|candidate| candidate == &identity)
            || !seen.insert(identity.call.clone())
        {
            return Err(format!(
                "TOOL-RESOLUTION-001: requested FFI call '{}' is duplicated or outside the checked caller census",
                current.node_id.0
            ));
        }
    }
    Ok(())
}

fn validate_mir_body_calls(
    mir: &MirProgram,
    owner: &NodeId,
    expected: &[FfiBodyCallIdentity],
) -> Result<(), String> {
    let mut actual = mir
        .ffi_call_entries_in_source_order()
        .into_iter()
        .filter(|(_, contract)| &contract.caller == owner)
        .map(|(_, contract)| FfiBodyCallIdentity {
            owner: contract.caller.clone(),
            call: contract.call_node.clone(),
            declaration: contract.declaration.clone(),
            symbol: contract.symbol.clone(),
            argc: contract.arguments.len(),
            span: contract.span,
        })
        .collect::<Vec<_>>();
    actual.sort_by(|left, right| left.call.cmp(&right.call));
    if actual != expected {
        return Err(format!(
            "MIR-COVERAGE-001: verifier-only FFI MIR caller '{}' has call identities different from Checker (checked {}, MIR {})",
            owner.0,
            expected.len(),
            actual.len()
        ));
    }
    Ok(())
}

fn validate_selected_ensures(
    program: &CheckedProgram,
    mir: &MirProgram,
    selected: &BTreeSet<MirInstructionId>,
) -> Result<(), String> {
    for instruction in selected {
        let Some(contract) = mir.ffi_calls().get(instruction) else {
            return Err(format!(
                "MIR-COVERAGE-001: selected FFI Ensures instruction '{}' is absent from the verifier-only MIR census",
                instruction.as_str()
            ));
        };
        let Some(declaration) = program.ffi_declarations().get(&contract.declaration) else {
            return Err(format!(
                "TOOL-RESOLUTION-001: selected FFI Ensures instruction '{}' lost its Checker declaration",
                instruction.as_str()
            ));
        };
        let ResolvedFfiContractRoot::Ready { node_id } = &declaration.ensures else {
            return Err(format!(
                "MIR-COVERAGE-001: unsupported or deferred foreign Ensures root at call '{}' cannot be selected for proof",
                contract.call_node.0
            ));
        };
        if contract.ensures_root.as_ref() != Some(node_id)
            || contract.ensures.is_none()
            || contract.ensures_deferred
            || contract.ensures_types.is_empty()
        {
            return Err(format!(
                "MIR-COVERAGE-001: selected FFI Ensures at call '{}' does not match its Ready Checker root",
                contract.call_node.0
            ));
        }
    }
    Ok(())
}

/// Verify a complete ScalarFfi route after the canonical route selector has
/// admitted it. The consumer rebinds it to current Checker roots and the
/// exact call/declaration source census before issuing scalar proofs.
pub(crate) fn verify_scalar_route(
    program: &CheckedProgram,
    mir: &MirProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let source = CheckerFfiSourceIdentity::capture(program)?;
    source.validate_mir(mir)?;
    let receipt = mir.route_receipt("verify-ffi-v1");
    receipt.validate_against_program(mir).map_err(|message| {
        format!("MIR-COVERAGE-001: ScalarFfi route receipt is invalid: {message}")
    })?;
    super::verify_ffi_mir_results_with_route_receipt(mir, source_hash.clone(), &receipt).and_then(
        |results| {
            super::validate_mir_result_provenance(
                &results,
                &receipt,
                &source_hash,
                "verify_ffi_checked_scalar_route",
            )?;
            Ok(results)
        },
    )
}

/// Verify the literal declaration compatibility profile only after binding
/// the profile to the complete current Checker root/call inventory.
pub(crate) fn verify_literal_contracts(
    program: &CheckedProgram,
    source_hash: &str,
) -> Option<Result<Vec<VerificationResult>, String>> {
    let source = match CheckerFfiSourceIdentity::capture(program) {
        Ok(source) => source,
        Err(error) => return Some(Err(error)),
    };
    let profile =
        match crate::core::mir::reference::from_checked_program_ffi_literal_contracts(program) {
            Ok(Some(profile)) => profile,
            Ok(None) => return None,
            Err(error) => return Some(Err(error)),
        };
    if let Err(error) = profile.validate() {
        return Some(Err(format!("MIR-COVERAGE-001: {error}")));
    }
    if let Err(error) = validate_literal_profile_calls(program, &source, &profile) {
        return Some(Err(error));
    }
    Some(super::ffi_literal_profile::verify_profile(
        &profile,
        source_hash,
    ))
}

fn validate_literal_profile_calls(
    program: &CheckedProgram,
    source: &CheckerFfiSourceIdentity,
    profile: &crate::core::mir::reference::MirFfiLiteralContractProfileV1,
) -> Result<(), String> {
    let declaration_root_calls = source.declaration_root_calls();
    let expected_declarations = program
        .call_sites_sorted()
        .into_iter()
        .filter(|site| {
            site.kind == ResolvedCallKind::Extern && !declaration_root_calls.contains(&site.node_id)
        })
        .filter_map(|site| match site.resolved_callee.as_ref() {
            Some(ResolvedCallee::Extern(declaration_id)) => program
                .ffi_declarations()
                .get(declaration_id)
                .filter(|declaration| declaration.has_declared_contract())
                .map(|_| declaration_id.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let profile_declarations = profile
        .declarations()
        .iter()
        .map(|declaration| declaration.declaration.clone())
        .collect::<BTreeSet<_>>();
    if expected_declarations != profile_declarations {
        return Err(
            "MIR-COVERAGE-001: literal FFI profile omitted or added a called Checker declaration"
                .into(),
        );
    }
    for profile_declaration in profile.declarations() {
        let declaration = program
            .ffi_declarations()
            .get(&profile_declaration.declaration)
            .ok_or_else(|| {
                format!(
                    "TOOL-RESOLUTION-001: literal FFI profile lost Checker declaration '{}'",
                    profile_declaration.declaration.0
                )
            })?;
        if declaration.symbol != profile_declaration.symbol {
            return Err(format!(
                "MIR-COVERAGE-001: literal FFI profile symbol '{}' disagrees with Checker declaration '{}'",
                profile_declaration.symbol, declaration.symbol
            ));
        }
        for (root, profile_root) in [
            (&declaration.requires, profile_declaration.requires.as_ref()),
            (&declaration.ensures, profile_declaration.ensures.as_ref()),
        ] {
            let checker_node = match root {
                ResolvedFfiContractRoot::Ready { node_id } => Some(node_id),
                ResolvedFfiContractRoot::Absent | ResolvedFfiContractRoot::Deferred { .. } => None,
            };
            if checker_node != profile_root.map(|root| &root.node) {
                return Err(format!(
                    "MIR-COVERAGE-001: literal FFI profile roots disagree with Checker declaration '{}'",
                    declaration.symbol
                ));
            }
        }

        let mut checker_calls = program
            .call_sites_sorted()
            .into_iter()
            .filter(|site| {
                site.kind == ResolvedCallKind::Extern
                    && site.callee == declaration.symbol
                    && site.resolved_callee
                        == Some(ResolvedCallee::Extern(
                            profile_declaration.declaration.clone(),
                        ))
                    && !declaration_root_calls.contains(&site.node_id)
            })
            .map(|site| {
                (
                    site.node_id.clone(),
                    NodeId(site.owner.clone()),
                    site.origin.user_span(),
                )
            })
            .collect::<Vec<_>>();
        checker_calls.sort_by(|left, right| left.0.cmp(&right.0));
        let mut profile_calls = profile_declaration
            .callsites
            .iter()
            .map(|site| (site.node.clone(), site.owner.clone(), site.span))
            .collect::<Vec<_>>();
        profile_calls.sort_by(|left, right| left.0.cmp(&right.0));
        if checker_calls != profile_calls {
            return Err(format!(
                "MIR-COVERAGE-001: literal FFI profile callsites disagree with Checker declaration '{}'",
                declaration.symbol
            ));
        }
    }
    Ok(())
}

fn format_mir_build_error(owner: &str, error: MirProgramBuildError) -> String {
    format!("MIR-COVERAGE-001: verifier-only FFI {owner} projection failed: {error:?}")
}

#[cfg(test)]
mod tests {
    use super::{FfiCallerProjectionShape, FfiVerifierOnlyProjection};
    use crate::core::NodeId;

    #[test]
    fn checker_bound_projection_keeps_the_exact_caller_and_declaration_identity() {
        let source = r#"
            extern "C" {
                func positive(value: i64) -> i64 requires: value > 0;
            }
            func caller(value: i64) -> i64 {
                requires: value >= 0
                positive(value)
            }
        "#;
        let file = super::super::parse_memory_source(source, "ffi-compat-projection")
            .expect("source parses");
        let checked = crate::core::check_program(&file).expect("source checks");
        let site = checked
            .call_sites_sorted()
            .into_iter()
            .find(|site| site.kind == crate::core::ResolvedCallKind::Extern)
            .expect("one checked extern call");
        let owner = NodeId(site.owner.clone());
        let projection = FfiVerifierOnlyProjection::from_checked_caller(
            &checked,
            &owner,
            std::slice::from_ref(site),
            FfiCallerProjectionShape::TopLevel,
        )
        .expect("exact Checker-bound verifier-only projection");

        assert_eq!(projection.owner(), &owner);
        assert_eq!(
            projection.mir().purpose(),
            crate::core::mir::reference::MirProgramPurpose::VerifierOnly
        );
        assert_eq!(projection.mir().ffi_calls().len(), 1);
        assert_eq!(
            projection.receipt().profile,
            super::MIR_FFI_OWNER_SLICE_ROUTE_PROFILE
        );
        projection
            .receipt()
            .validate_against_program(projection.mir())
            .expect("receipt remains bound to the projected graph");
    }

    #[test]
    fn checker_bound_projection_rejects_a_detached_call_site_record() {
        let source = r#"
            extern "C" {
                func positive(value: i64) -> i64 requires: value > 0;
            }
            func caller(value: i64) -> i64 {
                requires: value >= 0
                positive(value)
            }
        "#;
        let file = super::super::parse_memory_source(source, "ffi-compat-detached-site")
            .expect("source parses");
        let checked = crate::core::check_program(&file).expect("source checks");
        let site = checked
            .call_sites_sorted()
            .into_iter()
            .find(|site| site.kind == crate::core::ResolvedCallKind::Extern)
            .expect("one checked extern call");
        let owner = NodeId(site.owner.clone());
        let mut detached = site.clone();
        detached.callee.push_str("_forged");

        let error = FfiVerifierOnlyProjection::from_checked_caller(
            &checked,
            &owner,
            std::slice::from_ref(&detached),
            FfiCallerProjectionShape::TopLevel,
        )
        .err()
        .expect("detached caller-site records fail before MIR proof consumption");
        assert!(error.contains("TOOL-RESOLUTION-001"), "{error}");
    }

    #[test]
    fn deferred_foreign_ensures_cannot_be_selected_for_verifier_only_proof() {
        let source = r#"
            extern "C" {
                func foreign(value: string) -> i32
                    requires: value != ""
                    ensures: unsupported_contract(result);
            }
            func caller(value: string) -> i32 {
                requires: value != ""
                foreign(value)
            }
        "#;
        let file = super::super::parse_memory_source(source, "ffi-compat-deferred-ensures")
            .expect("source parses");
        let checked = crate::core::check_program(&file).expect("Deferred Ensures checks");
        let owner = NodeId("function:caller".into());
        let sites = checked
            .call_sites_sorted()
            .into_iter()
            .filter(|site| {
                site.owner == owner.0 && site.kind == crate::core::ResolvedCallKind::Extern
            })
            .map(|site| (*site).clone())
            .collect::<Vec<_>>();
        let projection = FfiVerifierOnlyProjection::from_checked_caller(
            &checked,
            &owner,
            &sites,
            FfiCallerProjectionShape::TopLevel,
        )
        .expect("caller still projects for its supported Requires obligation");
        let (instruction, contract) = projection
            .mir()
            .ffi_call_entries_in_source_order()
            .into_iter()
            .find(|(_, contract)| contract.caller == owner && contract.ensures_deferred)
            .expect("Deferred foreign Ensures remains present in MIR census");
        assert!(contract.ensures.is_none());
        assert!(matches!(
            &checked.ffi_declarations()[&contract.declaration].ensures,
            crate::core::ir::ResolvedFfiContractRoot::Deferred { .. }
        ));
        let selected =
            std::iter::once(instruction.clone()).collect::<std::collections::BTreeSet<_>>();
        let error = super::validate_selected_ensures(&checked, projection.mir(), &selected)
            .expect_err("Deferred Ensures is never silently upgraded into a proof");
        assert!(
            error.contains("unsupported or deferred foreign Ensures"),
            "{error}"
        );
    }
}
