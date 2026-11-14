//! Canonical MIR parity checks for verdicts formerly owned by the Flow/VIR
//! surface-AST verifier. Keep this module isolated from the historical AST
//! tests so its removal can be audited independently.

use super::{ProofArtifact, VerifStatus, VerificationResult};
use crate::core::mir::verification::{
    prepare_verification_request, MirVerificationPreparation, MirVerificationRequest,
};
use crate::core::CheckedProgram;

fn parse_checked(source: &str, label: &str) -> CheckedProgram {
    let file = super::parse_memory_source(source, label).expect("parse Flow retirement source");
    crate::core::check_program(&file).expect("typecheck Flow retirement source")
}

fn require_z3() -> bool {
    if super::is_z3_available() {
        true
    } else {
        eprintln!("    └─ skipped (Z3 not available)");
        false
    }
}

fn exact_mir_results(
    checked: &CheckedProgram,
    source_hash: &str,
) -> (
    Vec<VerificationResult>,
    crate::core::mir::CanonicalMirRouteReceipt,
) {
    let request = match prepare_verification_request(checked) {
        MirVerificationPreparation::Materialized(request) => request,
        MirVerificationPreparation::Deferred(boundaries) => {
            panic!("Flow retirement request deferred unexpectedly: {boundaries:?}")
        }
        MirVerificationPreparation::NotRequested => {
            panic!("Flow retirement source has no canonical proof request")
        }
    };
    verify_exact_request(&request, source_hash)
}

fn verify_exact_request(
    request: &MirVerificationRequest,
    source_hash: &str,
) -> (
    Vec<VerificationResult>,
    crate::core::mir::CanonicalMirRouteReceipt,
) {
    let canonical = request.program();
    super::validate_mir_capabilities(canonical)
        .expect("the exact canonical Flow request passes the public capability gate");
    let receipt = canonical.route_receipt("canonical-contract-request-v1");
    receipt
        .validate_against_program(canonical)
        .expect("the exact canonical Flow request receipt validates");
    let mut results =
        super::verify_mir_with_route_receipt(canonical, &receipt, source_hash.to_owned())
            .expect("exact canonical MIR Flow request");
    for result in &mut results {
        for (owner, name) in request.names() {
            if result.func_name == owner.0 || result.func_name == format!("{owner:?}") {
                result.func_name = name.clone();
                break;
            }
        }
        if let Some(artifact) = result.artifact.as_ref() {
            assert_eq!(
                artifact.engine,
                ProofArtifact::ENGINE_MIR,
                "{}",
                result.func_name
            );
            assert_eq!(
                artifact.proof_profile,
                ProofArtifact::PROFILE_MIR_V1,
                "{}",
                result.func_name
            );
            assert_eq!(
                artifact.obligation_scope,
                ProofArtifact::SCOPE_MIR_FUNCTION_CONTRACTS_V1,
                "{}",
                result.func_name
            );
            assert_eq!(artifact.source_hash, source_hash, "{}", result.func_name);
            assert_eq!(
                artifact.mir_hash, receipt.mir_digest,
                "{}",
                result.func_name
            );
            assert_eq!(
                artifact.mir_route_receipt.as_ref(),
                Some(&receipt),
                "{}",
                result.func_name
            );
        }
    }
    (results, receipt)
}

fn result_for<'a>(results: &'a [VerificationResult], name: &str) -> &'a VerificationResult {
    results
        .iter()
        .find(|result| result.func_name == name)
        .unwrap_or_else(|| panic!("verifier result for {name} is missing: {results:?}"))
}

fn assert_public_routes_match_exact_mir(
    checked: &CheckedProgram,
    source_hash: &str,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    exact: &[VerificationResult],
    expected: &[(&str, VerifStatus)],
) {
    for dual in [false, true] {
        let public = if dual {
            super::verify_checked_dual(checked, source_hash.to_owned())
        } else {
            super::verify_checked(checked, source_hash.to_owned())
        }
        .unwrap_or_else(|error| {
            panic!(
                "{} Flow retirement route: {error}",
                if dual { "dual" } else { "single" }
            )
        });

        for (name, status) in expected {
            let exact_result = result_for(exact, name);
            let public_result = result_for(&public, name);
            assert_eq!(
                exact_result.status, *status,
                "exact MIR {name}: {}",
                exact_result.message
            );
            assert_eq!(
                public_result.status, *status,
                "public {name}: {}",
                public_result.message
            );
            let artifact = public_result
                .artifact
                .as_ref()
                .unwrap_or_else(|| panic!("public {name} result has no proof artifact"));
            assert_eq!(artifact.source_hash, source_hash, "{name}");
            match artifact.engine.as_str() {
                ProofArtifact::ENGINE_MIR => {
                    assert_eq!(artifact.mir_hash, receipt.mir_digest, "{name}");
                    assert_eq!(artifact.mir_route_receipt.as_ref(), Some(receipt), "{name}");
                }
                ProofArtifact::ENGINE_RESOLVED if dual => {
                    let (profile, scope) = if name.contains("::") {
                        (
                            ProofArtifact::PROFILE_RESOLVED_FLOW_TRANSITION_V1,
                            ProofArtifact::SCOPE_FLOW_TRANSITION_CONTRACTS_V1,
                        )
                    } else {
                        (
                            ProofArtifact::PROFILE_VERIFIED_CORE_1,
                            ProofArtifact::SCOPE_FUNCTION_CONTRACTS_V1,
                        )
                    };
                    assert_eq!(
                        artifact.proof_profile, profile,
                        "dual Resolved profile for {name}"
                    );
                    assert_eq!(
                        artifact.obligation_scope, scope,
                        "dual Resolved scope for {name}"
                    );
                    assert!(
                        !artifact.resolved_ir_hash.is_empty(),
                        "dual Resolved result {name} carries its IR identity"
                    );
                    assert!(artifact.mir_hash.is_empty(), "{name}");
                    assert!(artifact.mir_route_receipt.is_none(), "{name}");
                }
                engine => panic!("unexpected {engine:?} artifact for {name}"),
            }
        }
    }
}

fn assert_resolved_primary_matches_exact_mir(
    checked: &CheckedProgram,
    source_hash: &str,
    exact: &[VerificationResult],
    expected: &[(&str, VerifStatus)],
) {
    let mut resolved = super::Verifier::new().expect("Resolved primary verifier");
    resolved.set_source_hash(source_hash.to_owned());
    let primary = resolved.verify_checked(checked);

    for (name, status) in expected {
        let exact_result = result_for(exact, name);
        let primary_result = result_for(&primary, name);
        assert_eq!(exact_result.status, *status, "exact MIR {name}");
        assert_eq!(
            primary_result.status, *status,
            "Resolved primary {name}: {}",
            primary_result.message
        );
        let artifact = primary_result
            .artifact
            .as_ref()
            .unwrap_or_else(|| panic!("Resolved primary {name} has no proof artifact"));
        assert_eq!(artifact.engine, ProofArtifact::ENGINE_RESOLVED, "{name}");
        assert_eq!(
            artifact.proof_profile,
            ProofArtifact::PROFILE_RESOLVED_FLOW_TRANSITION_V1,
            "Resolved primary Flow transition proof profile for {name}"
        );
        assert_eq!(
            artifact.obligation_scope,
            ProofArtifact::SCOPE_FLOW_TRANSITION_CONTRACTS_V1,
            "Resolved primary Flow transition obligation scope for {name}"
        );
        assert_eq!(artifact.source_hash, source_hash, "{name}");
        assert!(
            !artifact.resolved_ir_hash.is_empty(),
            "Resolved primary {name} carries its checker-owned IR identity"
        );
        assert!(artifact.mir_hash.is_empty(), "{name}");
        assert!(artifact.mir_route_receipt.is_none(), "{name}");
    }
}

fn mir_contract_projects_field(
    expression: &crate::core::mir::MirContractExpr,
    field: &crate::core::NodeId,
) -> bool {
    use crate::core::mir::{MirContractExpr, MirProjection};

    match expression {
        MirContractExpr::Project { base, projection } => {
            matches!(projection, MirProjection::Field(projected) if projected == field)
                || mir_contract_projects_field(base, field)
        }
        MirContractExpr::Unary { operand, .. } => mir_contract_projects_field(operand, field),
        MirContractExpr::Binary { left, right, .. } => {
            mir_contract_projects_field(left, field) || mir_contract_projects_field(right, field)
        }
        MirContractExpr::StringLength(value) | MirContractExpr::StringCharAt0(value) => {
            mir_contract_projects_field(value, field)
        }
        MirContractExpr::Value(_)
        | MirContractExpr::Result
        | MirContractExpr::Old(_)
        | MirContractExpr::Int(_)
        | MirContractExpr::Bool(_)
        | MirContractExpr::Float(_)
        | MirContractExpr::String(_)
        | MirContractExpr::Unsupported(_) => false,
    }
}

#[test]
fn flow_helper_call_chain_source_order_keeps_mir_and_dual_verdicts() {
    if !require_z3() {
        return;
    }
    let sources = [
        "func c(x: i32) -> i32 { requires: x >= 0; ensures: result == x; b(x) }
         func b(x: i32) -> i32 { requires: x >= 0; ensures: result == x; a(x) }
         func a(x: i32) -> i32 { requires: x >= 0; ensures: result == x; x }",
        "func a(x: i32) -> i32 { requires: x >= 0; ensures: result == x; x }
         func b(x: i32) -> i32 { requires: x >= 0; ensures: result == x; a(x) }
         func c(x: i32) -> i32 { requires: x >= 0; ensures: result == x; b(x) }",
    ];
    for (index, source) in sources.into_iter().enumerate() {
        let checked = parse_checked(source, &format!("flow-helper-chain-{index}"));
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let (exact, receipt) = exact_mir_results(&checked, &source_hash);
        let expected = [("c", VerifStatus::Proven)];
        assert_public_routes_match_exact_mir(&checked, &source_hash, &receipt, &exact, &expected);
    }
}

#[test]
fn unsupported_s8_flow_checked_and_dual_routes_fail_closed_without_results() {
    let source =
        include_str!("../../tests/fixtures/mir_native_flow_transition_rejected_builtin.mimi");
    let checked = parse_checked(source, "unsupported-s8-flow-retirement-boundary");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    for (label, route) in [
        (
            "single checked verifier",
            super::verify_checked(&checked, source_hash.clone()),
        ),
        (
            "dual checked verifier",
            super::verify_checked_dual(&checked, source_hash),
        ),
    ] {
        let error = route.expect_err(label);
        assert!(
            error.contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
            "{label} must expose the typed canonical coverage boundary: {error}"
        );
        assert!(
            error.contains("S8 Flow transition candidate"),
            "{label} must identify the unsupported Flow owner: {error}"
        );
    }
}

#[test]
fn flow_transition_invariant_and_field_projection_owner_parity() {
    if !require_z3() {
        return;
    }
    for (postcondition, expected_status) in [
        ("self.count + n > 0", VerifStatus::Proven),
        ("self.count + n > 1", VerifStatus::Disproven),
    ] {
        let source = format!(
            "flow Counter {{\n\
                state Zero {{ count: i32 }}\n\
                state Positive {{ count: i32 }}\n\
                transition inc(Zero, n: i32) -> Positive {{\n\
                    invariant: self.count >= 0\n\
                    requires: self.count <= 2147483646 && n == 1\n\
                    ensures: {postcondition}\n\
                    return Positive {{ count: self.count + n }}\n\
                }}\n\
            }}"
        );
        let checked = parse_checked(&source, "flow-transition-verdict");
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let transition = checked
            .transitions()
            .values()
            .find(|transition| transition.id.event == "inc")
            .expect("checker-owned Counter::inc transition");
        let owner = transition.node_id.clone();
        assert_eq!(transition.id.flow.0, "Counter");
        let checked_callable = checked.callables().get(&owner).expect("transition owner");
        assert_eq!(checked_callable.owner, owner);
        let flow = checked
            .flows()
            .values()
            .find(|flow| flow.id == transition.id.flow)
            .expect("checker-owned Flow");
        let source_state = flow.states.get("Zero").expect("checker-owned source state");
        let target_state = flow
            .states
            .get("Positive")
            .expect("checker-owned target state");
        assert_eq!(source_state.id, transition.id.source);
        assert!(transition.targets.contains(&target_state.id));
        let count_field = source_state
            .field_ids
            .get("count")
            .expect("checker-owned count field identity");
        let count_type = checked
            .resolved_field_type(count_field)
            .expect("checker catalog resolves count field type");
        assert!(matches!(
            checked.resolved_types().get(count_type),
            Some(crate::core::ir::ResolvedType::Primitive(
                crate::core::ir::PrimitiveType::I32
            ))
        ));
        let resolved_fields = super::resolved_expr::resolved_flow_state_field_catalog(&checked)
            .expect("checker-owned Flow field catalog");
        assert_eq!(
            resolved_fields
                .get(&source_state.node_id)
                .and_then(|fields| fields.get(count_field)),
            Some(count_type),
            "Resolved verifier catalog preserves the Flow field's stable identity and type"
        );

        let request = match prepare_verification_request(&checked) {
            MirVerificationPreparation::Materialized(request) => request,
            MirVerificationPreparation::Deferred(boundaries) => {
                panic!("Flow transition owner request deferred: {boundaries:?}")
            }
            MirVerificationPreparation::NotRequested => {
                panic!("Flow transition has no canonical proof request")
            }
        };
        let canonical = request.program();
        let mir_transition = canonical
            .transitions()
            .get(&owner)
            .expect("MIR transition preserves checker owner");
        assert_eq!(mir_transition.owner, owner);
        let mir_function = canonical
            .functions()
            .get(&owner)
            .expect("MIR callable preserves transition owner");
        let source_parameter = mir_function
            .parameters
            .first()
            .and_then(|parameter| mir_function.values.get(parameter))
            .expect("MIR transition has its source-state parameter");
        assert_eq!(source_parameter.ty, mir_transition.source);
        let source_type = canonical
            .type_catalog()
            .get(&mir_transition.source)
            .expect("MIR TypeDesc for source state");
        let crate::core::mir::types::MirLayout::Record { fields, .. } = &source_type.layout else {
            panic!("Flow source-state TypeDesc must retain its record fields");
        };
        let mir_count = fields
            .iter()
            .find(|field| field.id == *count_field)
            .expect("MIR TypeDesc preserves checker field identity");
        assert_eq!(
            mir_count.ty, *count_type,
            "MIR field TypeDesc preserves the checker's scalar type"
        );
        let invariant = mir_function
            .contracts
            .iter()
            .find(|contract| contract.kind == crate::core::mir::MirContractKind::Invariant)
            .expect("MIR carries the transition's own invariant fact");
        let requires = mir_function
            .contracts
            .iter()
            .find(|contract| contract.kind == crate::core::mir::MirContractKind::Requires)
            .expect("MIR carries the transition guard");
        let ensures = mir_function
            .contracts
            .iter()
            .find(|contract| contract.kind == crate::core::mir::MirContractKind::Ensures)
            .expect("MIR carries the transition postcondition");
        assert!(mir_contract_projects_field(
            &invariant.condition,
            count_field
        ));
        assert!(mir_contract_projects_field(
            &requires.condition,
            count_field
        ));
        assert!(mir_contract_projects_field(&ensures.condition, count_field));

        let (exact, receipt) = verify_exact_request(&request, &source_hash);
        assert_eq!(receipt.flow_transition_digest.len(), 64);
        let expected = [("Counter::inc", expected_status.clone())];

        // Flow state identity, contract kinds, field projections, and proof
        // status are checked against canonical MIR above and below. The
        // retired Flow/VIR AST engine is not an oracle for this route.
        let mir_result = result_for(&exact, "Counter::inc");
        let mir_artifact = mir_result
            .artifact
            .as_ref()
            .expect("exact MIR transition result retains its proof artifact");
        assert_eq!(mir_artifact.engine, ProofArtifact::ENGINE_MIR);
        assert_eq!(mir_artifact.proof_profile, ProofArtifact::PROFILE_MIR_V1);
        assert_eq!(
            mir_artifact.obligation_scope,
            ProofArtifact::SCOPE_MIR_FUNCTION_CONTRACTS_V1
        );
        assert_eq!(mir_artifact.source_hash, source_hash);
        assert_eq!(mir_artifact.mir_hash, receipt.mir_digest);
        assert_eq!(mir_artifact.mir_route_receipt.as_ref(), Some(&receipt));

        assert_eq!(mir_result.status, expected_status);
        if expected_status == VerifStatus::Proven {
            assert!(
                mir_result
                    .message
                    .contains("canonical MIR ensures contract proven"),
                "the field-projection contract must be proved by canonical MIR: {}",
                mir_result.message
            );
        } else {
            assert!(
                mir_result
                    .message
                    .contains("canonical MIR ensures contract is disproven"),
                "the retained false-ensures counterexample must remain a real ensures failure: {}",
                mir_result.message
            );
        }
        assert_resolved_primary_matches_exact_mir(&checked, &source_hash, &exact, &expected);
        assert_public_routes_match_exact_mir(&checked, &source_hash, &receipt, &exact, &expected);
    }
}

#[test]
fn canonical_mir_transition_preserves_contract_kinds() {
    let source = r#"
        flow ContractKinds {
            state Source { value: i32 }
            state Target { value: i32 }
            transition advance(Source, n: i32) -> Target {
                ensures: n > 0
                requires: n == 1
                invariant: n >= 0
                return Target { value: n }
            }
        }
    "#;
    let checked = parse_checked(source, "mir-flow-contract-kind-order");
    let request = match prepare_verification_request(&checked) {
        MirVerificationPreparation::Materialized(request) => request,
        MirVerificationPreparation::Deferred(boundaries) => {
            panic!("Flow transition contract-kind request deferred: {boundaries:?}")
        }
        MirVerificationPreparation::NotRequested => {
            panic!("Flow transition contract-kind source has no proof request")
        }
    };
    let canonical = request.program();
    let owner = checked
        .transitions()
        .values()
        .find(|transition| transition.id.event == "advance")
        .expect("checker-owned advance transition")
        .node_id
        .clone();
    let function = canonical
        .functions()
        .get(&owner)
        .expect("canonical MIR transition function");
    let kinds = function
        .contracts
        .iter()
        .map(|contract| contract.kind)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        kinds,
        [
            crate::core::mir::MirContractKind::Requires,
            crate::core::mir::MirContractKind::Ensures,
            crate::core::mir::MirContractKind::Invariant,
        ]
        .into_iter()
        .collect(),
        "canonical MIR retains every contract category independent of source order"
    );
}

#[test]
fn resolved_ordinary_callable_invariant_does_not_guard_requires_definedness() {
    if !require_z3() {
        return;
    }
    let source = r#"
        func ordinary(x: i32) -> i32 {
            invariant: x != 0
            requires: 10 / x == 1
            ensures: result == x
            x
        }
    "#;
    let checked = parse_checked(source, "ordinary-invariant-deferred-boundary");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let mut verifier = super::Verifier::new().expect("Resolved primary verifier");
    verifier.set_source_hash(source_hash.clone());
    let results = verifier.verify_checked(&checked);
    let result = result_for(&results, "ordinary");
    assert_eq!(
        result.status,
        VerifStatus::Disproven,
        "ordinary invariant cannot assume x != 0 to make its Requires division defined: {}",
        result.message
    );
    assert!(
        result
            .message
            .contains("requires contract expression is undefined"),
        "the failure must be the unguarded Requires division: {}",
        result.message
    );
    let artifact = result
        .artifact
        .as_ref()
        .expect("ordinary callable definedness result has a proof artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_RESOLVED);
    assert_eq!(
        artifact.proof_profile,
        ProofArtifact::PROFILE_VERIFIED_CORE_1
    );
    assert_eq!(
        artifact.obligation_scope,
        ProofArtifact::SCOPE_FUNCTION_CONTRACTS_V1
    );
    assert_eq!(artifact.source_hash, source_hash);
    assert!(!artifact.resolved_ir_hash.is_empty());
    assert!(artifact.mir_hash.is_empty());
    assert!(artifact.mir_route_receipt.is_none());
}

#[test]
fn flow_scalar_migration_keeps_mir_and_dual_verdicts() {
    if !require_z3() {
        return;
    }
    let source = r#"
        func true_value() -> bool { ensures: result == true
            true
        }
        func false_claim() -> bool { ensures: result == true
            false
        }
        func not_false() -> bool { ensures: result != false
            true
        }
        func same(flag: bool) -> bool { ensures: result == flag
            flag
        }
        func flip(flag: bool) -> bool { ensures: result != flag
            !flag
        }
        func integer_claim() -> i32 { ensures: result == 2
            1
        }
        func main() -> i32 { 0 }
    "#;
    let checked = parse_checked(source, "flow-scalar-migration");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let expected = [
        ("true_value", VerifStatus::Proven),
        ("false_claim", VerifStatus::Disproven),
        ("not_false", VerifStatus::Proven),
        ("same", VerifStatus::Proven),
        ("flip", VerifStatus::Proven),
        ("integer_claim", VerifStatus::Disproven),
    ];
    let (exact, receipt) = exact_mir_results(&checked, &source_hash);
    assert_public_routes_match_exact_mir(&checked, &source_hash, &receipt, &exact, &expected);
}
