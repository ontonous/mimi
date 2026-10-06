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
            return Preparation::Deferred(boundaries)
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
    let canonical = request.program();
    let receipt = canonical.route_receipt("canonical-contract-request-v1");
    receipt.validate_against_program(canonical)?;
    // A solver Unknown result is final for this typed request. Do not retry
    // the retained AST after a completed canonical encoding attempt.
    let mut results =
        super::mir::verify_program_with_route_receipt(canonical, source_hash.clone(), &receipt)?;
    super::validate_mir_result_provenance(
        &results,
        &receipt,
        &source_hash,
        "canonical contract request",
    )?;
    for result in &mut results {
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

#[cfg(test)]
mod tests;
