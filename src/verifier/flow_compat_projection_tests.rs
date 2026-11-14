//! Production-edge coverage for the bounded Actor/Flow MIR verifier slice.
//!
//! These tests exercise the checked verifier adapters that replace exact Flow
//! transition/caller owners. The ordinary `Verifier::verify_checked` API and
//! proof roots outside that closed call component remain on the Resolved
//! compatibility consumer.

use super::{ProofArtifact, VerifStatus, VerificationResult};
use crate::core::mir::verification::checker_bound_actor_flow_projection_v1;
use crate::core::CheckedProgram;
use std::collections::BTreeMap;

fn parse_checked(source: &str, label: &str) -> CheckedProgram {
    let file = super::parse_memory_source(source, label).expect("parse Actor/Flow source");
    crate::core::check_program(&file).expect("typecheck Actor/Flow source")
}

fn result_for<'a>(results: &'a [VerificationResult], name: &str) -> &'a VerificationResult {
    results
        .iter()
        .find(|result| result.func_name == name)
        .unwrap_or_else(|| panic!("verification result for {name} is missing: {results:#?}"))
}

fn assert_mir_result(
    results: &[VerificationResult],
    name: &str,
    expected: VerifStatus,
    source_hash: &str,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) {
    let result = result_for(results, name);
    assert_eq!(result.status, expected, "{name}: {}", result.message);
    let artifact = result
        .artifact
        .as_ref()
        .unwrap_or_else(|| panic!("{name} has no proof artifact: {result:#?}"));
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR, "{name}");
    assert_eq!(
        artifact.proof_profile,
        ProofArtifact::PROFILE_MIR_V1,
        "{name}"
    );
    assert_eq!(artifact.source_hash, source_hash, "{name}");
    assert_eq!(artifact.mir_hash, receipt.mir_digest, "{name}");
    assert_eq!(artifact.mir_route_receipt.as_ref(), Some(receipt), "{name}");
}

fn name_multiset(results: &[VerificationResult]) -> BTreeMap<String, usize> {
    let mut names = BTreeMap::new();
    for result in results {
        *names.entry(result.func_name.clone()).or_insert(0) += 1;
    }
    names
}

#[test]
fn actor_flow_projection_dual_merges_same_source_proven_and_disproven_owners() {
    if !super::is_z3_available() {
        eprintln!("    └─ skipped (Z3 not available)");
        return;
    }

    let source = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero, n: i32) -> Positive {
        requires: self.count == 0 && n == 1
        ensures: self.count + n > 0
        return Positive { count: self.count + n }
    }
}

actor Dormant {
    marker: i32 = 0;
    func read() -> i32 {
        ensures: result == 7
        7
    }
}

func accepted() -> i32 {
    let next = Counter::inc(Zero { count: 0 }, 1)
    next.count
}

func rejected() -> i32 {
    let next = Counter::inc(Zero { count: 0 }, 2)
    next.count
}

func unrelated_claim() -> i32 {
    ensures: result == 0
    1
}

func main() -> i32 { accepted() }
"#;
    let checked = parse_checked(source, "actor-flow-checker-projection");
    assert!(super::is_actor_proof_compatibility(&checked));
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let projection = checker_bound_actor_flow_projection_v1(&checked)
        .expect("Checker-bound actor Flow projection");
    let transition_owner = checked
        .transitions()
        .values()
        .find(|transition| transition.id.flow.0 == "Counter" && transition.id.event == "inc")
        .expect("Checker-owned Counter::inc transition")
        .node_id
        .clone();
    let function_owner = |name: &str| {
        checked
            .functions()
            .values()
            .find(|function| function.qualified_name == name)
            .unwrap_or_else(|| panic!("Checker function {name} missing"))
            .node_id
            .clone()
    };
    let accepted_owner = function_owner("accepted");
    let rejected_owner = function_owner("rejected");
    let main_owner = function_owner("main");
    let actor_method_owner = checked
        .actor_descriptors()
        .values()
        .flat_map(|actor| actor.methods.iter())
        .next()
        .expect("Actor method census contains Dormant::read")
        .owner
        .clone();
    let unrelated_owner = function_owner("unrelated_claim");

    assert!(projection.flow_transition_roots.contains(&transition_owner));
    assert!(projection.transition_callers.contains(&accepted_owner));
    assert!(projection.transition_callers.contains(&rejected_owner));
    assert!(projection.actor_scope.contains(&actor_method_owner));
    assert!(projection.source_callable_owners.contains(&main_owner));
    assert!(projection
        .source_callable_owners
        .contains(&actor_method_owner));
    assert!(projection.source_proof_owners.contains(&transition_owner));
    assert!(projection.source_proof_owners.contains(&actor_method_owner));
    assert!(projection.source_proof_owners.contains(&unrelated_owner));
    assert!(projection.source_proof_declaration_roots.is_empty());
    assert!(!projection.source_declaration_roots.is_empty());
    assert!(projection.foreign_declaration_roots.is_empty());
    assert!(projection.owner_scope.contains(&main_owner));
    assert!(projection.owner_scope.contains(&accepted_owner));
    assert!(projection.owner_scope.contains(&rejected_owner));
    assert!(!projection.owner_scope.contains(&actor_method_owner));
    assert!(!projection.owner_scope.contains(&unrelated_owner));
    assert!(projection
        .call_edges
        .contains(&(accepted_owner.clone(), transition_owner.clone())));
    assert!(projection
        .call_edges
        .contains(&(rejected_owner.clone(), transition_owner.clone())));
    assert!(projection
        .call_edges
        .contains(&(main_owner.clone(), accepted_owner.clone())));

    let batch = super::flow_mir::verify_actor_flow_projection_v1(&checked, &source_hash)
        .expect("MIR actor Flow projection adapter")
        .expect("supported checked Flow slice materializes through canonical MIR request");
    assert_eq!(batch.projection, projection);
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program_actor_flow_projection_v1(
            &checked,
            &projection,
        )
        .expect("same Checker-bound projection rematerializes");
    let source_census_digest =
        blake3::hash(canonical.proof_source_census().canonical_text().as_bytes())
            .to_hex()
            .to_string();
    assert_eq!(
        batch.route_receipt.profile,
        format!(
            "{}:{source_census_digest}",
            super::flow_mir::ACTOR_FLOW_VERIFIER_PROJECTION_V1
        )
    );
    assert_eq!(
        batch.owner_names.get(&transition_owner).map(String::as_str),
        Some("Counter::inc")
    );
    batch
        .route_receipt
        .validate_against_program(&canonical)
        .expect("MIR receipt carries the projection's transition and source census");
    for caller in [&accepted_owner, &rejected_owner] {
        let source_call = checked
            .call_sites_sorted()
            .into_iter()
            .find(|site| {
                site.owner == caller.0
                    && matches!(
                        site.resolved_callee.as_ref(),
                        Some(crate::core::ir::ResolvedCallee::Transition(_))
                    )
            })
            .expect("Checker source callsite to the Flow transition");
        let source_census = canonical
            .proof_source_census()
            .owner(caller)
            .expect("full source census retains the caller owner");
        let census_calls = source_census
            .body_calls()
            .filter(|call| call.call_node() == &source_call.node_id)
            .collect::<Vec<_>>();
        let [census_call] = census_calls.as_slice() else {
            panic!(
                "source census binds one exact callsite: {}",
                source_call.node_id.0
            );
        };
        assert_eq!(census_call.body_owner(), caller);
        assert_eq!(
            Some(census_call.callee()),
            source_call.resolved_callee.as_ref(),
            "the census callsite preserves the Checker callee identity"
        );
    }

    // Both positive and negative callsite obligations live in this one
    // CheckedProgram and therefore share one canonical request receipt.
    for (name, status) in [
        ("Counter::inc", VerifStatus::Proven),
        ("accepted", VerifStatus::Proven),
        ("rejected", VerifStatus::Disproven),
    ] {
        assert_mir_result(
            &batch.results,
            name,
            status,
            &source_hash,
            &batch.route_receipt,
        );
        assert_eq!(
            batch
                .results
                .iter()
                .filter(|result| result.func_name == name)
                .count(),
            1,
            "the MIR request returns one result for {name}"
        );
    }

    let single_results = super::verify_checked(&checked, source_hash.clone())
        .expect("single checked Actor/Flow route");
    for (name, status) in [
        ("Counter::inc", VerifStatus::Proven),
        ("accepted", VerifStatus::Proven),
        ("rejected", VerifStatus::Disproven),
    ] {
        assert_mir_result(
            &single_results,
            name,
            status,
            &source_hash,
            &batch.route_receipt,
        );
    }

    let mut direct = super::Verifier::new().expect("direct Resolved verifier");
    direct.set_source_hash(source_hash.clone());
    let resolved_results = direct.verify_checked(&checked);
    let identity_merged =
        super::merge_actor_flow_dual_results(resolved_results.clone(), batch.clone())
            .expect("owner-keyed dual merge preserves non-projected Resolved roots");
    let dual_results = super::verify_checked_dual(&checked, source_hash.clone())
        .expect("dual checked Actor/Flow route merges the selected MIR owners");

    let mut expected_names = name_multiset(&resolved_results);
    let flow_callsite_owners = super::actor_flow_callsite_owner_closure(&batch.projection);
    for result in &batch.results {
        let owner = batch
            .owner_names
            .iter()
            .find_map(|(owner, name)| (name == &result.func_name).then_some(owner))
            .expect("MIR result retains its Checker owner identity");
        let aliases = [
            result.func_name.clone(),
            owner.0.clone(),
            format!("{owner:?}"),
        ];
        let has_resolved_peer = resolved_results
            .iter()
            .any(|resolved| aliases.iter().any(|alias| alias == &resolved.func_name));
        if !has_resolved_peer
            && result.status != VerifStatus::NoObligations
            && flow_callsite_owners.contains(owner)
        {
            *expected_names.entry(result.func_name.clone()).or_insert(0) += 1;
        }
    }
    assert_eq!(name_multiset(&identity_merged), expected_names);
    assert_eq!(name_multiset(&dual_results), expected_names);

    let actor_method_name = checked
        .functions()
        .get(&actor_method_owner)
        .expect("Actor method function entry")
        .qualified_name
        .clone();
    for (name, expected) in [
        ("Counter::inc", VerifStatus::Proven),
        ("accepted", VerifStatus::Proven),
        ("rejected", VerifStatus::Disproven),
        (actor_method_name.as_str(), VerifStatus::Proven),
        ("unrelated_claim", VerifStatus::Disproven),
    ] {
        let dual = result_for(&dual_results, name);
        assert_eq!(dual.status, expected, "dual {name}: {dual:?}");
        if let Some(old) = resolved_results
            .iter()
            .find(|result| result.func_name == name)
        {
            assert_eq!(
                dual.status, old.status,
                "dual preserves Resolved verdict for {name}"
            );
            assert_eq!(
                dual.artifact
                    .as_ref()
                    .map(|artifact| artifact.engine.as_str()),
                Some(ProofArtifact::ENGINE_RESOLVED),
                "matching dual verdict retains the primary Resolved artifact for {name}"
            );
        } else {
            let artifact = dual
                .artifact
                .as_ref()
                .unwrap_or_else(|| panic!("Flow callsite {name} has no artifact: {dual:?}"));
            assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR, "{name}");
            assert_eq!(artifact.source_hash, source_hash, "{name}");
            assert_eq!(
                artifact.mir_route_receipt.as_ref(),
                Some(&batch.route_receipt),
                "{name}"
            );
        }
        assert_eq!(
            dual.artifact
                .as_ref()
                .map(|artifact| artifact.source_hash.as_str()),
            Some(source_hash.as_str()),
            "the returned proof artifact is bound to the same source root for {name}"
        );
        assert!(
            !dual.message.contains("E0439"),
            "same-source proof pair agrees for {name}: {}",
            dual.message
        );
        assert_eq!(
            dual_results
                .iter()
                .filter(|result| result.func_name == name)
                .count(),
            1,
            "the dual merge returns exactly one result for {name}"
        );
    }
}
