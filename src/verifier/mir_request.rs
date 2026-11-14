//! Typed production replacement for the covered Flow verification queue.
//!
//! The core plan owns body/contract coverage. This adapter adds the MIR proof
//! capability gate and consumes one immutable graph. It does not construct a
//! FuncDef, call an AST encoder, or substitute a same-engine second run for
//! the independent Resolved/MIR protocol at the dual entry point.

use crate::core::mir::verification::{
    MirVerificationBoundary, MirVerificationPreparation, MirVerificationRequest,
};
use crate::core::{CheckedProgram, NodeId};

use super::{ProofArtifact, VerificationResult};

pub(super) enum Preparation {
    Complete(MirVerificationRequest),
    Deferred(Vec<MirVerificationBoundary>),
}

pub(super) fn prepare(program: &CheckedProgram) -> Preparation {
    let request = match crate::core::mir::verification::prepare_verification_request(program) {
        MirVerificationPreparation::Materialized(request) => request,
        MirVerificationPreparation::Deferred(boundaries) => {
            return Preparation::Deferred(boundaries);
        }
        MirVerificationPreparation::NotRequested => return Preparation::Deferred(Vec::new()),
    };
    if let Err(messages) = super::validate_mir_capabilities(request.program()) {
        return Preparation::Deferred(
            messages
                .into_iter()
                .map(|reason| MirVerificationBoundary {
                    owner: NodeId("verification:consumer".into()),
                    point: None,
                    reason,
                })
                .collect(),
        );
    }
    Preparation::Complete(request)
}

pub(super) fn verify(
    request: &MirVerificationRequest,
    checked: &CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_with_receipt_profile(
        request,
        checked,
        source_hash,
        "canonical-contract-request-v1",
    )
}

/// Consume a canonical request under its exact verifier-only route profile.
/// The request constructor owns the selected Checker identities; the profile
/// additionally binds this particular consumer's proof semantics into the
/// MIR receipt.
pub(super) fn verify_with_receipt_profile(
    request: &MirVerificationRequest,
    checked: &CheckedProgram,
    source_hash: String,
    receipt_profile: &str,
) -> Result<Vec<VerificationResult>, String> {
    let canonical = request.program();
    let receipt = canonical.route_receipt(receipt_profile);
    receipt.validate_against_program(canonical)?;
    // A solver Unknown result is final for this typed request. Do not retry
    // the retained AST after a completed canonical encoding attempt.
    let mut results =
        super::mir::verify_program_with_route_receipt(canonical, source_hash.clone(), &receipt)?;
    super::validate_mir_result_provenance(&results, &receipt, &source_hash, receipt_profile)?;
    for result in &mut results {
        // A materialized request can still expose an owner-scoped proof
        // census gap when a nested/deferred contract was lowered into the
        // scalar graph.  Keep that observation as the public checker-owned
        // coverage diagnostic; leaking the internal proof-source wording
        // makes callers mistake a deliberate unsupported shape for verifier
        // infrastructure failure.
        if result
            .message
            .contains("proof worklist coverage gap: DeferredBoundary")
        {
            let message = format!(
                "{}: {}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                result.message
            );
            let span = checked
                .functions()
                .values()
                .find(|function| {
                    function.qualified_name == result.func_name
                        || function.node_id.0 == result.func_name
                })
                .map(|function| function.origin.user_span())
                .or_else(|| checked.entry_span())
                .unwrap_or(crate::span::Span::UNKNOWN);
            result.status = super::VerifStatus::NotInTrustedSubset;
            result.message = message.clone();
            result.diagnostic = Some(crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                message,
                span,
            ));
            result.constraint_count = 0;
            result.artifact = None;
            result.trusted_subset_domain = Some(super::TrustedSubsetDomain::Contract);
        }
        let owner = NodeId(result.func_name.clone());
        if let Some(diagnostic) = &mut result.diagnostic {
            let point = diagnostic
                .origin
                .as_ref()
                .and_then(|origin| origin.parent_node_id.as_ref())
                .map(|point| NodeId(point.clone()));
            if let Some(meta) = point
                .as_ref()
                .and_then(|point| checked.node_meta().get(point))
                .or_else(|| checked.node_meta().get(&owner))
            {
                diagnostic.span = meta.origin.user_span();
            }
        }
        if let Some(name) = request.names().get(&owner) {
            result.func_name = name.clone();
        }
        if let Some(artifact) = &result.artifact {
            if artifact.engine != ProofArtifact::ENGINE_MIR {
                return Err(
                    "MIR-VERIFY-001: typed request result has a non-MIR proof artifact".into(),
                );
            }
        }
    }
    Ok(results)
}

/// The old Resolved entry uses `Debug(NodeId)` for non-function labels while
/// the Flow queue uses `Flow::event`. Match canonical owners before merging,
/// so a transition disagreement cannot evade E0439 merely through labels.
pub(super) fn normalize_primary_names(
    results: &mut [VerificationResult],
    request: &MirVerificationRequest,
) {
    for result in results {
        for (owner, name) in request.names() {
            if result.func_name == owner.0 || result.func_name == format!("{owner:?}") {
                result.func_name = name.clone();
                break;
            }
        }
    }
}

/// Return owner-scoped boundaries when either proof-producing engine result
/// lacks a peer result from the other engine. A present `NoObligations`
/// result is still a peer: the merge algebra deliberately lets the other
/// engine's verdict stand when one side had no proof obligation.
pub(super) fn peer_coverage_boundaries(
    primary: &[VerificationResult],
    secondary: &[VerificationResult],
) -> Vec<MirVerificationBoundary> {
    let primary_names = primary
        .iter()
        .map(|result| result.func_name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let secondary_names = secondary
        .iter()
        .map(|result| result.func_name.as_str())
        .collect::<std::collections::BTreeSet<_>>();

    let mut boundaries = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for result in primary
        .iter()
        .filter(|result| !matches!(result.status, super::VerifStatus::NoObligations))
    {
        if !secondary_names.contains(result.func_name.as_str())
            && seen.insert(result.func_name.clone())
        {
            boundaries.push(MirVerificationBoundary {
                owner: NodeId(result.func_name.clone()),
                point: None,
                reason: "Resolved proof result has no canonical MIR peer result".to_string(),
            });
        }
    }
    for result in secondary
        .iter()
        .filter(|result| !matches!(result.status, super::VerifStatus::NoObligations))
    {
        if !primary_names.contains(result.func_name.as_str())
            && seen.insert(result.func_name.clone())
        {
            boundaries.push(MirVerificationBoundary {
                owner: NodeId(result.func_name.clone()),
                point: None,
                reason: "canonical MIR proof result has no Resolved peer result".to_string(),
            });
        }
    }
    boundaries
}

#[cfg(test)]
mod tests;
