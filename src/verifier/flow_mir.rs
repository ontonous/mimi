//! Checker-bound Flow proof projection used only to replace the Flow owners
//! inside the Actor compatibility verifier.  The MIR graph is built by the
//! canonical ResolvedBody lowerer; this adapter never reconstructs source AST.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::mir::verification::CheckerBoundActorFlowProjectionV1;
use crate::core::NodeId;

pub(crate) const ACTOR_FLOW_VERIFIER_PROJECTION_V1: &str = "actor-flow-verifier-projection-v1";

/// One complete, owner-scoped result batch from the actor Flow MIR slice.
/// `projection` records Checker call closure, transition roots and disjoint
/// actor method scope; `route_receipt` binds those lowered bodies, transition
/// contracts and the full checked-source census to every MIR proof artifact.
#[derive(Clone)]
pub(crate) struct ActorFlowMirBatchV1 {
    pub(crate) projection: CheckerBoundActorFlowProjectionV1,
    #[cfg(test)]
    pub(crate) route_receipt: crate::core::mir::CanonicalMirRouteReceipt,
    pub(crate) owner_names: BTreeMap<NodeId, String>,
    pub(crate) results: Vec<super::VerificationResult>,
}

/// Verify the exact Checker-sealed Flow portion of an Actor compatibility
/// request through the canonical proof-request consumer. `Ok(None)` means the
/// Checker did not select this slice, so the existing compatibility route
/// still handles the request. Once selected, materialization, capability, or
/// proof-coverage failures are explicit route errors and never reopen the
/// Resolved fallback for the selected owners.
pub(crate) fn verify_actor_flow_projection_v1(
    checked: &crate::core::CheckedProgram,
    source_hash: &str,
) -> Result<Option<ActorFlowMirBatchV1>, String> {
    if !super::is_z3_available() {
        return Ok(None);
    }
    let Some((projection, request)) =
        crate::core::mir::verification::prepare_actor_flow_verification_request_v1(checked)?
    else {
        return Ok(None);
    };
    let canonical = request.program();
    super::validate_mir_capabilities(canonical).map_err(|errors| {
        coverage_error(format!(
            "Checker-selected MIR projection failed the whole-program capability gate: {errors:?}"
        ))
    })?;

    // The ordinary MIR digest intentionally omits the independent checked
    // source census. Bind its canonical form into this projection receipt so
    // equal lowered MIR cannot detach the selected callsite/root identities
    // from the source request that authorized them.
    let source_census_digest =
        blake3::hash(canonical.proof_source_census().canonical_text().as_bytes())
            .to_hex()
            .to_string();
    let receipt_profile = format!("{ACTOR_FLOW_VERIFIER_PROJECTION_V1}:{source_census_digest}");
    let route_receipt = canonical.route_receipt(receipt_profile.clone());
    route_receipt
        .validate_against_program(canonical)
        .map_err(|message| {
            coverage_error(format!("actor Flow projection receipt rejected: {message}"))
        })?;
    let mut results = super::mir_request::verify_with_receipt_profile(
        &request,
        checked,
        source_hash.to_owned(),
        &receipt_profile,
    )?;
    super::validate_mir_result_provenance(&results, &route_receipt, source_hash, &receipt_profile)?;

    let names = request.names();
    let owner_by_name = names
        .iter()
        .map(|(owner, name)| (name.as_str(), owner.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut result_owners = BTreeSet::new();
    results.retain_mut(|result| {
        let Some(owner) = owner_by_name.get(result.func_name.as_str()).cloned() else {
            return false;
        };
        if !projection.owner_scope.contains(&owner) {
            return false;
        }
        result_owners.insert(owner);
        true
    });

    let required_owners = projection
        .flow_transition_roots
        .union(&projection.transition_callers)
        .cloned()
        .chain(projection.owner_scope.iter().filter_map(|owner| {
            checked
                .callable(owner)
                .is_some_and(|callable| callable.has_proof_obligations())
                .then_some(owner.clone())
        }))
        .collect::<BTreeSet<_>>();
    if !required_owners.is_subset(&result_owners)
        || results.iter().any(|result| {
            matches!(
                result.status,
                super::VerifStatus::NotInTrustedSubset
                    | super::VerifStatus::RuntimeOnlyContract
                    | super::VerifStatus::SolverUnknown
                    | super::VerifStatus::Timeout
                    | super::VerifStatus::InfrastructureError
            ) || result.artifact.as_ref().is_some_and(|artifact| {
                artifact.engine != super::ProofArtifact::ENGINE_MIR
                    || artifact.proof_profile != super::ProofArtifact::PROFILE_MIR_V1
                    || artifact.mir_route_receipt.as_ref() != Some(&route_receipt)
            })
        })
        || required_owners.iter().any(|owner| {
            results
                .iter()
                .find(|result| result.func_name == names[owner])
                .is_none_or(|result| {
                    !matches!(
                        result.status,
                        super::VerifStatus::Proven | super::VerifStatus::Disproven
                    ) || result.artifact.as_ref().is_none_or(|artifact| {
                        artifact.engine != super::ProofArtifact::ENGINE_MIR
                            || artifact.proof_profile != super::ProofArtifact::PROFILE_MIR_V1
                            || artifact.mir_route_receipt.as_ref() != Some(&route_receipt)
                    })
                })
        })
    {
        return Err(coverage_error(
            "Checker-selected MIR proof request did not return a complete Proven/Disproven result for every Flow root and transition call edge".into(),
        ));
    }

    Ok(Some(ActorFlowMirBatchV1 {
        projection,
        #[cfg(test)]
        route_receipt,
        owner_names: names.to_owned(),
        results,
    }))
}

fn coverage_error(reason: String) -> String {
    format!(
        "{}: actor Flow canonical request coverage failure: {reason}",
        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
    )
}
