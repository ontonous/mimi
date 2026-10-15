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
        CheckedProgram::reset_test_legacy_body_access();
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
        assert!(
            CheckedProgram::test_legacy_body_access().is_empty(),
            "{} route must not access a retained AST body",
            if dual { "dual" } else { "single" }
        );

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
        CheckedProgram::reset_test_legacy_body_access();
    }
}

#[test]
fn flow_helper_call_chain_source_order_keeps_mir_and_dual_verdicts() {
    if !require_z3() {
        return;
    }
    let sources = [
        "func c(x: int) -> int { requires: x >= 0; ensures: result == x; b(x) }
         func b(x: int) -> int { requires: x >= 0; ensures: result == x; a(x) }
         func a(x: int) -> int { requires: x >= 0; ensures: result == x; x }",
        "func a(x: int) -> int { requires: x >= 0; ensures: result == x; x }
         func b(x: int) -> int { requires: x >= 0; ensures: result == x; a(x) }
         func c(x: int) -> int { requires: x >= 0; ensures: result == x; b(x) }",
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
fn flow_transition_contracts_keep_proven_and_disproven_mir_statuses() {
    if !require_z3() {
        return;
    }
    for (postcondition, expected_status) in [
        ("n > 0", VerifStatus::Proven),
        ("n < 0", VerifStatus::Disproven),
    ] {
        let source = format!(
            "flow Counter {{\n\
                state Zero {{ count: i32 }}\n\
                state Positive {{ count: i32 }}\n\
                transition inc(Zero, n: i32) -> Positive {{\n\
                    requires: n > 0\n\
                    ensures: {postcondition}\n\
                    return Positive {{ count: n }}\n\
                }}\n\
            }}"
        );
        let checked = parse_checked(&source, "flow-transition-verdict");
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let (exact, receipt) = exact_mir_results(&checked, &source_hash);
        let expected = [("Counter::inc", expected_status)];
        assert_public_routes_match_exact_mir(&checked, &source_hash, &receipt, &exact, &expected);
    }
}
