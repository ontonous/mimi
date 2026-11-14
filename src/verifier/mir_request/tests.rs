use super::*;
use crate::verifier::{verify_checked, verify_checked_dual, VerifStatus, Verifier};

fn build_checked(source: &str) -> (crate::ast::File, CheckedProgram, String) {
    let file =
        crate::verifier::parse_memory_source(source, "typed-flow-contract-request").expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    (
        file,
        program,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
}

fn require_solver() -> bool {
    crate::verifier::is_z3_available()
}

fn result<'a>(results: &'a [VerificationResult], name: &str) -> &'a VerificationResult {
    results
        .iter()
        .find(|result| result.func_name == name)
        .unwrap_or_else(|| panic!("missing {name}: {results:?}"))
}

fn synthetic_result(func_name: &str, status: VerifStatus) -> VerificationResult {
    VerificationResult {
        func_name: func_name.to_string(),
        status,
        message: String::new(),
        diagnostic: None,
        duration_us: 0,
        constraint_count: 0,
        artifact: None,
        trusted_subset_domain: None,
    }
}

#[test]
fn dual_peer_coverage_requires_proof_results_from_both_engines() {
    let resolved_only = vec![synthetic_result("resolved_owner", VerifStatus::Proven)];
    let mir_empty = Vec::new();
    let gaps = peer_coverage_boundaries(&resolved_only, &mir_empty);
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].owner.0, "resolved_owner");
    assert!(gaps[0].reason.contains("Resolved"));

    let resolved_empty = Vec::new();
    let mir_only = vec![synthetic_result("mir_owner", VerifStatus::Proven)];
    let gaps = peer_coverage_boundaries(&resolved_empty, &mir_only);
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].owner.0, "mir_owner");
    assert!(gaps[0].reason.contains("Resolved peer"));

    let resolved_without_obligation =
        vec![synthetic_result("shared_owner", VerifStatus::NoObligations)];
    let mir_proof = vec![synthetic_result("shared_owner", VerifStatus::Proven)];
    assert!(peer_coverage_boundaries(&resolved_without_obligation, &mir_proof).is_empty());

    let no_obligations = vec![synthetic_result(
        "resolved_only",
        VerifStatus::NoObligations,
    )];
    assert!(peer_coverage_boundaries(&no_obligations, &mir_empty).is_empty());
}

fn assert_extern_declaration_boundary(
    source: &str,
    symbol: &str,
    expected_contract: bool,
    expected_exact_reference: bool,
) {
    let (_, checked, source_hash) = build_checked(source);
    let declaration = checked
        .ffi_declarations()
        .values()
        .find(|declaration| declaration.symbol == symbol)
        .expect("checker-owned extern declaration descriptor");
    assert_eq!(
        declaration.has_declared_contract(),
        expected_contract,
        "extern contract census for {symbol}"
    );
    let exact_reference = checked.call_sites_sorted().into_iter().any(|site| {
        matches!(
            site.resolved_callee.as_ref(),
            Some(crate::core::ResolvedCallee::Extern(owner))
                if owner == &declaration.declaration
        )
    });
    assert_eq!(
        exact_reference, expected_exact_reference,
        "exact typed extern reference census for {symbol}"
    );

    let crate::core::mir::verification::MirVerificationPreparation::Deferred(boundaries) =
        crate::core::mir::verification::prepare_verification_request(&checked)
    else {
        panic!("extern declaration obligation must defer the whole request");
    };
    assert!(boundaries.iter().any(|boundary| {
        boundary.owner == declaration.declaration
            && boundary.reason.contains("FFI compatibility owner")
    }));

    if require_solver() {
        let results = verify_checked(&checked, source_hash).expect("deferred extern request");
        let positive = result(&results, "positive");
        assert_eq!(positive.status, VerifStatus::NotInTrustedSubset);
        assert!(
            positive.artifact.is_none(),
            "deferred request has no artifact"
        );
    }
}

const GUARDED_FLOW: &str = r#"
flow Account {
    state Open { count: i32 }
    state Closed { count: i32 }
    transition close(Open, amount: i32) -> Closed {
        requires: amount >= 1 && amount <= 100
        invariant: amount >= 0
        ensures: amount > 0
        return Closed { count: self.count }
    }
}
"#;

#[test]
fn typed_flow_guard_and_entry_invariant_prove_without_legacy_owner_access() {
    if !require_solver() {
        return;
    }
    let (_, checked, source_hash) = build_checked(GUARDED_FLOW);
    let actual = verify_checked(&checked, source_hash.clone()).expect("typed verification");
    let actual = result(&actual, "Account::close");
    assert_eq!(actual.status, VerifStatus::Proven);
    let artifact = actual.artifact.as_ref().expect("real MIR proof artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.source_hash, source_hash);
    let Preparation::Complete(request) = prepare(&checked) else {
        panic!("guarded Flow request incomplete");
    };
    assert_eq!(artifact.mir_hash, request.program().canonical_digest());
    assert!(actual.constraint_count > 0);
}

#[test]
fn typed_ordinary_scalar_contracts_use_mir_for_single_and_dual_entries() {
    if !require_solver() {
        return;
    }
    let source = r#"
func positive(value: i32) -> i32 {
    requires: value > 0
    ensures: result > 0
    value
}
func false_claim() -> bool {
    ensures: result == true
    false
}
func main() -> i32 { 0 }
"#;
    let (_, checked, source_hash) = build_checked(source);
    let Preparation::Complete(request) = prepare(&checked) else {
        panic!("ordinary scalar contract request must have complete typed MIR coverage");
    };
    let mir_digest = request.program().canonical_digest();

    for dual in [false, true] {
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("ordinary scalar contract verification");
        for (name, status) in [
            ("positive", VerifStatus::Proven),
            ("false_claim", VerifStatus::Disproven),
        ] {
            let result = result(&results, name);
            assert_eq!(result.status, status, "{name}: {}", result.message);
            let artifact = result
                .artifact
                .as_ref()
                .expect("ordinary scalar MIR proof artifact");
            assert_eq!(artifact.source_hash, source_hash);
            if dual {
                assert!(matches!(
                    artifact.engine.as_str(),
                    ProofArtifact::ENGINE_RESOLVED | ProofArtifact::ENGINE_MIR
                ));
                if artifact.engine == ProofArtifact::ENGINE_MIR {
                    assert_eq!(artifact.mir_hash, mir_digest);
                } else {
                    assert!(!artifact.resolved_ir_hash.is_empty());
                }
            } else {
                assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
                assert_eq!(artifact.mir_hash, mir_digest);
            }
        }
    }
}

#[test]
fn typed_request_skips_unreferenced_contractless_extern_and_emits_mir_artifact() {
    let source = r#"
extern "C" {
    func unused(value: i32) -> i32;
}
func positive(value: i32) -> i32 {
    requires: value > 0
    ensures: result > 0
    value
}
"#;
    let (_, checked, source_hash) = build_checked(source);
    let declaration = checked
        .ffi_declarations()
        .values()
        .find(|declaration| declaration.symbol == "unused")
        .expect("checker-owned unused extern descriptor");
    assert!(!declaration.has_declared_contract());
    assert!(!checked.call_sites_sorted().into_iter().any(|site| {
        matches!(
            site.resolved_callee.as_ref(),
            Some(crate::core::ResolvedCallee::Extern(owner))
                if owner == &declaration.declaration
        )
    }));
    let Preparation::Complete(request) = prepare(&checked) else {
        panic!("unused contractless extern must not defer the ordinary proof root");
    };
    let mir_digest = request.program().canonical_digest();

    if !require_solver() {
        return;
    }
    let results = verify_checked(&checked, source_hash.clone()).expect("complete MIR request");
    let positive = result(&results, "positive");
    assert_eq!(positive.status, VerifStatus::Proven, "{positive:#?}");
    let artifact = positive.artifact.as_ref().expect("complete MIR artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.source_hash, source_hash);
    assert_eq!(artifact.mir_hash, mir_digest);
}

#[test]
fn typed_request_keeps_called_contractless_extern_deferred_without_artifact() {
    assert_extern_declaration_boundary(
        r#"
extern "C" {
    func foreign(value: i32) -> i32;
}
func positive(value: i32) -> i32 {
    requires: value > 0
    ensures: result > 0
    foreign(value)
}
"#,
        "foreign",
        false,
        true,
    );
}

#[test]
fn typed_request_keeps_uncalled_contract_bearing_extern_deferred_without_artifact() {
    assert_extern_declaration_boundary(
        r#"
extern "C" {
    func unused(value: i32) -> i32 requires: value >= 0 ensures: result >= value;
}
func positive(value: i32) -> i32 {
    requires: value > 0
    ensures: result > 0
    value
}
"#,
        "unused",
        true,
        false,
    );
}

#[test]
fn typed_flow_false_ensures_is_rejected_without_legacy_owner_access() {
    if !require_solver() {
        return;
    }
    let source = GUARDED_FLOW.replace("ensures: amount > 0", "ensures: amount < 0");
    let (_, checked, source_hash) = build_checked(&source);
    for dual in [false, true] {
        let actual = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("typed negative");
        assert_eq!(
            result(&actual, "Account::close").status,
            VerifStatus::Disproven
        );
    }
}

#[test]
fn typed_flow_actual_caller_must_establish_source_invariant_and_guard() {
    if !require_solver() {
        return;
    }
    let source = format!(
        "{GUARDED_FLOW}\nfunc main() -> i32 {{ let rejected = Account::close(Open {{ count: 3 }}, 0)\n drop(rejected)\n 0 }}"
    );
    let (_, checked, source_hash) = build_checked(&source);
    let actual = verify_checked(&checked, source_hash).expect("typed call-site obligation");
    let caller = result(&actual, "main");
    assert_eq!(
        caller.status,
        VerifStatus::Disproven,
        "typed caller result={caller:#?}; all results={actual:#?}"
    );
    assert!(caller.message.contains("source invariant or guard"));
}

#[test]
fn typed_transition_guard_cannot_assume_away_its_own_zero_division() {
    if !require_solver() {
        return;
    }
    let source = GUARDED_FLOW
        .replace(
            "requires: amount >= 1 && amount <= 100",
            "requires: 10 / amount > 0",
        )
        .replace("invariant: amount >= 0", "invariant: true");
    let (_, checked, source_hash) = build_checked(&source);
    let actual = verify_checked(&checked, source_hash).expect("typed guard definedness");
    assert_eq!(
        result(&actual, "Account::close").status,
        VerifStatus::Disproven
    );
    assert!(result(&actual, "Account::close")
        .message
        .contains("requires contract expression is undefined"));
}

#[test]
fn typed_transition_body_overflow_is_an_actual_obligation() {
    if !require_solver() {
        return;
    }
    let source = GUARDED_FLOW.replace("count: self.count }", "count: self.count + 1 }");
    let (_, checked, source_hash) = build_checked(&source);
    let actual = verify_checked(&checked, source_hash).expect("typed body trap");
    assert_eq!(
        result(&actual, "Account::close").status,
        VerifStatus::Disproven
    );
    assert!(result(&actual, "Account::close").message.contains("trap"));
}

#[test]
fn typed_flow_source_fields_bound_actual_state_arithmetic_and_caller_input() {
    if !require_solver() {
        return;
    }
    let source = GUARDED_FLOW
        .replace("invariant: amount >= 0", "invariant: self.count >= 0")
        .replace(
            "requires: amount >= 1 && amount <= 100",
            "requires: self.count >= 0 && self.count <= 100 && amount >= 1 && amount <= 100",
        )
        .replace("count: self.count }", "count: self.count + 1 }");
    let (_, checked, source_hash) = build_checked(&source);
    let actual = verify_checked(&checked, source_hash).expect("typed state-field arithmetic");
    assert_eq!(
        result(&actual, "Account::close").status,
        VerifStatus::Proven,
        "typed source-field result={:#?}; all results={actual:#?}",
        result(&actual, "Account::close")
    );
    let invalid = format!(
        "{source}\nfunc main() -> i32 {{ let rejected = Account::close(Open {{ count: -1 }}, 1)\n drop(rejected)\n 0 }}"
    );
    let (_, checked, source_hash) = build_checked(&invalid);
    let actual = verify_checked(&checked, source_hash).expect("typed invalid source");
    assert_eq!(
        result(&actual, "main").status,
        VerifStatus::Disproven,
        "typed invalid caller result={:#?}; all results={actual:#?}",
        result(&actual, "main")
    );
}

#[test]
fn typed_fails_return_preserves_source_payload_and_same_graph_execution() {
    if !require_solver() {
        return;
    }
    let source = include_str!("../../../tests/real_world/flow_state_match_fail_result_failure_dual_backend.mimi")
        .replace("transition go(A) -> B fails string {", "transition go(A) -> B fails string {\n invariant: self.n >= 0\n requires: self.n >= 0 && self.n <= 100\n ensures: true");
    let (_, checked, source_hash) = build_checked(&source);
    let request = match prepare(&checked) {
        Preparation::Complete(request) => request,
        Preparation::Deferred(boundaries) => {
            panic!("fails request incomplete; typed boundaries={boundaries:#?}")
        }
    };
    let canonical = request.program();
    let actual = verify_checked(&checked, source_hash).expect("typed fails source proof");
    assert_eq!(
        result(&actual, "F::go").status,
        VerifStatus::Proven,
        "typed fails transition={:#?}; all results={actual:#?}",
        result(&actual, "F::go")
    );
    assert_eq!(
        result(&actual, "main").status,
        VerifStatus::Proven,
        "typed fails caller={:#?}; all results={actual:#?}",
        result(&actual, "main")
    );
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(canonical)
        .execute(&NodeId("function:main".into()), &[])
        .expect("same-graph source-return reference");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(0)
    );
    let bytecode =
        crate::interp::bytecode::compile_mir_program(canonical).expect("same graph bytecode");
    assert_eq!(
        crate::interp::bytecode::BytecodeVM::new(bytecode)
            .run()
            .expect("source-return VM"),
        0
    );
    crate::codegen::mir::validate_mir_native(canonical).expect("same graph native capability");
    let context = inkwell::context::Context::create();
    crate::codegen::CodeGenerator::new(&context, "typed_fails_contract_request")
        .compile_mir_native(canonical)
        .expect("same graph native compilation");
}

#[test]
fn typed_ordinary_invariants_establish_before_assumption_on_resolved_route() {
    if !require_solver() {
        return;
    }
    let source = r#"
func established(x: i32) -> i32 {
    requires: x >= 0 && x <= 100
    invariant: x >= 0
    ensures: result > x
    x + 1
}
func forged(x: i32) -> i32 {
    requires: x == 0
    invariant: x > 100
    ensures: result > 0
    42
}
"#;
    let (_, checked, source_hash) = build_checked(source);
    let actual = verify_checked(&checked, source_hash).expect("typed invariants");
    assert_eq!(result(&actual, "established").status, VerifStatus::Proven);
    assert_eq!(result(&actual, "forged").status, VerifStatus::Disproven);
    assert!(result(&actual, "forged")
        .message
        .contains("not established"));
}

#[test]
fn typed_dual_uses_distinct_resolved_and_mir_consumers_and_retains_weaker_conclusion() {
    if !require_solver() {
        return;
    }
    let (_, checked, source_hash) = build_checked(GUARDED_FLOW);
    let Preparation::Complete(request) = prepare(&checked) else {
        panic!("complete typed request");
    };
    let mut independent_primary = Verifier::new().expect("Resolved solver");
    independent_primary.set_source_hash(source_hash.clone());
    let mut primary = independent_primary.verify_checked(&checked);
    normalize_primary_names(&mut primary, &request);
    let secondary = verify(&request, &checked, source_hash.clone()).expect("MIR solver");
    let left = result(&primary, "Account::close");
    let right = result(&secondary, "Account::close");
    assert_eq!(
        left.artifact.as_ref().expect("Resolved artifact").engine,
        ProofArtifact::ENGINE_RESOLVED
    );
    assert_eq!(
        right.artifact.as_ref().expect("MIR artifact").engine,
        ProofArtifact::ENGINE_MIR
    );
    let dual = verify_checked_dual(&checked, source_hash).expect("typed independent dual");
    let merged = result(&dual, "Account::close");
    if crate::verifier::verdict_class(&left.status) != crate::verifier::verdict_class(&right.status)
        && crate::verifier::verdict_class(&left.status) != crate::verifier::VerdictClass::NoOpinion
    {
        assert!(merged
            .diagnostic
            .as_ref()
            .is_some_and(|diagnostic| diagnostic.message.contains("E0439")));
        assert!(merged.message.contains("canonical_mir"));
        assert_ne!(merged.status, VerifStatus::Proven);
        assert!(merged.artifact.is_none());
    }
}

#[test]
fn typed_flow_math_points_reach_both_independent_proof_consumers() {
    if !require_solver() {
        return;
    }
    let source = GUARDED_FLOW.replace(
        "ensures: amount > 0",
        "math: { amount > 0 }\n ensures: amount > 0",
    );
    let (_, checked, source_hash) = build_checked(&source);
    let Preparation::Complete(request) = prepare(&checked) else {
        panic!("typed Flow Math request must be materialized");
    };
    let mut resolved_engine = Verifier::new().expect("Resolved solver");
    resolved_engine.set_source_hash(source_hash.clone());
    let mut resolved = resolved_engine.verify_checked(&checked);
    normalize_primary_names(&mut resolved, &request);
    let mir = verify(&request, &checked, source_hash.clone()).expect("independent MIR proof");
    let resolved_transition = result(&resolved, "Account::close");
    let mir_transition = result(&mir, "Account::close");
    assert_eq!(
        resolved_transition.status,
        VerifStatus::Proven,
        "{resolved_transition:#?}"
    );
    assert_eq!(
        mir_transition.status,
        VerifStatus::Proven,
        "{mir_transition:#?}"
    );
    assert_eq!(
        resolved_transition.artifact.as_ref().unwrap().engine,
        ProofArtifact::ENGINE_RESOLVED
    );
    assert_eq!(
        mir_transition.artifact.as_ref().unwrap().engine,
        ProofArtifact::ENGINE_MIR
    );

    let dual = verify_checked_dual(&checked, source_hash).expect("independent dual verification");
    assert_eq!(result(&dual, "Account::close").status, VerifStatus::Proven);
}

#[test]
fn typed_math_branch_facts_stay_lexical_and_loop_exit_is_scoped() {
    if !require_solver() {
        return;
    }
    let branch = r#"
func branch(x: i32) -> i32 {
    if x > 0 {
        math: { x > 0 }
    } else {
        math: { x <= 0 }
    }
    x
}
func leaked(x: i32) -> i32 {
    if x > 0 { math: { x >= 0 } } else { math: { x <= 0 } }
    math: { x >= 0 }
    x
}
"#;
    let (_, checked, source_hash) = build_checked(branch);
    let Preparation::Complete(request) = prepare(&checked) else {
        panic!("lexically scoped Math request must materialize");
    };
    let actual = verify(&request, &checked, source_hash.clone()).expect("branch-scoped MIR proof");
    assert_eq!(
        result(&actual, "branch").status,
        VerifStatus::Proven,
        "{actual:#?}"
    );
    assert_eq!(result(&actual, "leaked").status, VerifStatus::Disproven);
    let mut resolved = Verifier::new().expect("Resolved branch solver");
    let resolved_results = resolved.verify_checked(&checked);
    assert_eq!(
        result(&resolved_results, "branch").status,
        VerifStatus::Proven
    );
    assert_eq!(
        result(&resolved_results, "leaked").status,
        VerifStatus::Disproven
    );
    let dual = verify_checked_dual(&checked, source_hash).expect("independent branch consumers");
    assert_eq!(result(&dual, "branch").status, VerifStatus::Proven);
    assert_eq!(result(&dual, "leaked").status, VerifStatus::Disproven);

    let loop_source = r#"
func looped(x: i32) -> i32 {
    requires: x >= 0 && x <= 10
    while x > 0 {
        invariant: x >= 0
        math: { x >= 0 }
    }
    math: { x == 0 }
    x
}
"#;
    let (_, checked, source_hash) = build_checked(loop_source);
    let Preparation::Complete(request) = prepare(&checked) else {
        panic!("stationary typed loop request must materialize");
    };
    let function = request
        .program()
        .functions()
        .get(&NodeId("function:looped".into()))
        .expect("canonical loop owner");
    assert_eq!(
        function
            .contracts
            .iter()
            .map(|contract| contract.kind)
            .collect::<Vec<_>>(),
        vec![crate::core::mir::MirContractKind::Requires],
        "only direct callable-entry clauses belong in MIR function contracts"
    );
    assert_eq!(
        function
            .proof_worklist
            .as_ref()
            .expect("typed loop proof points")
            .expected_loop_invariants
            .len(),
        1
    );
    let actual = verify(&request, &checked, source_hash).expect("stationary MIR loop proof");
    assert_eq!(
        result(&actual, "looped").status,
        VerifStatus::Proven,
        "loop result={:#?}",
        result(&actual, "looped")
    );
    let mut resolved = Verifier::new().expect("Resolved loop solver");
    let resolved_results = resolved.verify_checked(&checked);
    assert_eq!(
        result(&resolved_results, "looped").status,
        VerifStatus::Proven,
        "Resolved loop result={:#?}",
        result(&resolved_results, "looped")
    );
    let dual = verify_checked_dual(&checked, "stationary-loop-dual".into())
        .expect("independent loop consumers");
    assert_eq!(result(&dual, "looped").status, VerifStatus::Proven);
}

#[test]
fn nested_requires_is_not_a_callable_entry_assumption() {
    if !require_solver() {
        return;
    }
    let source = r#"
func guarded(x: i32) -> i32 {
    ensures: x > 0
    x + {
        requires: x > 0
        0
    }
}
"#;
    let (_, checked, _) = build_checked(source);
    let callable = checked
        .callable(&NodeId("function:guarded".into()))
        .expect("checked callable retains the full proof census");
    assert!(callable.has_unmodeled_proof_roots());
    assert!(matches!(
        crate::core::mir::verification::prepare_verification_request(&checked),
        MirVerificationPreparation::Deferred(_)
    ));

    let mut resolved = Verifier::new().expect("Resolved solver");
    let results = resolved.verify_checked(&checked);
    let guarded = result(&results, "guarded");
    assert_eq!(guarded.status, VerifStatus::NotInTrustedSubset);
    assert!(guarded.artifact.is_none());

    let deferred = verify_checked(&checked, "nested-requires-entry-scope".into())
        .expect("typed coverage boundary remains callable");
    let guarded = result(&deferred, "guarded");
    assert_eq!(guarded.status, VerifStatus::NotInTrustedSubset);
    assert!(guarded
        .message
        .contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE));
}

#[test]
fn typed_request_does_not_hoist_nested_contracts_or_claim_loop_induction() {
    let sources = [
        "func identity(x: i32) -> i32 { x }\nfunc nested_call(x: i32) -> i32 { invariant: x >= 0\n identity({ ensures: false\n x }) }",
        "func nested_binary(x: i32) -> i32 { invariant: x >= 0\n x + { requires: false\n 0 } }",
        "func nested(x: i32) -> i32 { invariant: x >= 0\n if true { ensures: result > 0\n x } else { x } }",
        "func looped(x: i32) -> i32 { invariant: x >= 0\n while false { println(x) }\n x }",
    ];
    for source in sources {
        let (_, checked, _) = build_checked(source);
        let MirVerificationPreparation::Deferred(boundaries) =
            crate::core::mir::verification::prepare_verification_request(&checked)
        else {
            panic!("program-point/induction boundary must remain explicit");
        };
        assert!(!boundaries.is_empty());
        assert!(
            boundaries
                .iter()
                .any(|boundary| boundary.reason.contains("nested")
                    || boundary.reason.contains("loop"))
        );

        if source.contains("func looped") {
            let callable = checked
                .callable(&NodeId("function:looped".into()))
                .expect("root-invariant loop callable");
            assert!(callable
                .contracts
                .iter()
                .any(|contract| contract.kind == crate::core::ir::ContractKind::Invariant));

            let worklist = callable.proof_point_worklist();
            let loop_node = worklist
                .loop_nodes()
                .into_iter()
                .next()
                .expect("typed while point");
            assert!(
                worklist.loop_invariant_nodes().is_empty(),
                "the callable-entry invariant must not become a loop invariant"
            );
            assert!(
                worklist.boundaries.iter().any(|boundary| {
                    boundary.point == loop_node && boundary.reason.contains("loop needs")
                }),
                "effectful loop without its own induction receipt must be deferred"
            );

            if require_solver() {
                let results = verify_checked(&checked, "root-invariant-effectful-loop".into())
                    .expect("deferred loop verification");
                let looped = result(&results, "looped");
                assert_eq!(looped.status, VerifStatus::NotInTrustedSubset);
                assert!(
                    looped.artifact.is_none(),
                    "deferred loop must emit no artifact"
                );
            }
        }
    }
}

#[test]
fn resolved_flow_ignores_only_unused_nominal_result_and_checks_field_traps() {
    if !require_solver() {
        return;
    }
    let source = GUARDED_FLOW
        .replace("invariant: amount >= 0", "invariant: self.count >= 0")
        .replace(
            "requires: amount >= 1 && amount <= 100",
            "requires: self.count >= 0 && self.count <= 100 && amount >= 1 && amount <= 100",
        )
        .replace(
            "ensures: amount > 0",
            "math: { amount > 0 }\n        ensures: amount > 0",
        )
        .replace("count: self.count }", "count: self.count + 1 }");
    let (_, checked, source_hash) = build_checked(&source);
    let mut resolved = Verifier::new().expect("Resolved Flow solver");
    resolved.set_source_hash(source_hash);
    let actual = resolved.verify_checked(&checked);
    let transition = result(&actual, "Account::close");
    assert_eq!(transition.status, VerifStatus::Proven, "{transition:#?}");
    assert_eq!(
        transition
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_RESOLVED),
    );

    // The same input-only postcondition cannot hide checked arithmetic in the
    // nominal record body when the source field is no longer upper-bounded.
    let overflow = source.replace(" && self.count <= 100", "");
    let (_, checked, source_hash) = build_checked(&overflow);
    let mut resolved = Verifier::new().expect("Resolved Flow overflow solver");
    resolved.set_source_hash(source_hash);
    let actual = resolved.verify_checked(&checked);
    let transition = result(&actual, "Account::close");
    assert_eq!(transition.status, VerifStatus::Disproven, "{transition:#?}");
    assert!(transition.message.contains("overflow"), "{transition:#?}");

    // If a contract refers to the nominal result, no independent scalar
    // return binding exists yet; it must remain fail-closed.
    let result_contract = GUARDED_FLOW.replace("ensures: amount > 0", "ensures: result.count >= 0");
    let (_, checked, source_hash) = build_checked(&result_contract);
    let mut resolved = Verifier::new().expect("Resolved result-dependent Flow solver");
    resolved.set_source_hash(source_hash);
    let actual = resolved.verify_checked(&checked);
    let transition = result(&actual, "Account::close");
    assert_eq!(
        transition.status,
        VerifStatus::NotInTrustedSubset,
        "{transition:#?}"
    );
    assert!(transition.artifact.is_none(), "{transition:#?}");
}

#[test]
fn typed_mir_flow_contracts_check_root_definedness_with_short_circuit_order() {
    if !require_solver() {
        return;
    }
    const FLOW: &str = r#"
flow Account {
    state Open { count: i32 }
    state Closed { count: i32 }
    transition close(Open) -> Closed {
        requires: self.count == 0
        __ENSURES__
        return Closed { count: self.count }
    }
}
"#;

    // The left operand of `||` is evaluated first. Under `requires`, it is
    // exactly the division-by-zero expression, so the transition is invalid
    // even though the right operand would make the Boolean formula true.
    let trapping = FLOW.replace(
        "__ENSURES__",
        "ensures: 1 / self.count == 0 || self.count == 0",
    );
    let (_, checked, source_hash) = build_checked(&trapping);
    assert!(matches!(prepare(&checked), Preparation::Complete(_)));
    let actual = verify_checked(&checked, source_hash).expect("typed MIR contract check");
    let transition = result(&actual, "Account::close");
    assert_eq!(transition.status, VerifStatus::Disproven, "{transition:#?}");
    assert!(transition.message.contains("undefined"), "{transition:#?}");

    // Here the left operand is true under the requires, so the division on
    // the right is not evaluated. The valid short-circuit case stays Proven.
    let safe = FLOW.replace(
        "__ENSURES__",
        "ensures: self.count == 0 || 1 / self.count == 0",
    );
    let (_, checked, source_hash) = build_checked(&safe);
    assert!(matches!(prepare(&checked), Preparation::Complete(_)));
    let actual = verify_checked(&checked, source_hash).expect("typed MIR short-circuit proof");
    let transition = result(&actual, "Account::close");
    assert_eq!(transition.status, VerifStatus::Proven, "{transition:#?}");
    assert_eq!(
        transition
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR),
    );
}
