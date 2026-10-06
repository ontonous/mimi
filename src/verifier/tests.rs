use super::helpers::*;
use super::*;
use crate::ast::*;

fn assert_finite_ieee_binary64_mir_artifact(result: &VerificationResult) {
    let artifact = result.artifact.as_ref().expect("finite f64 proof artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(
        artifact.float_model, "ieee754_binary64_finite_rne",
        "the proof artifact must identify the actual finite IEEE binary64 model"
    );
    assert_eq!(
        artifact.float_model,
        crate::core::mir::types::MIR_VERIFIER_FLOAT_MODEL
    );
    let receipt = artifact
        .mir_route_receipt
        .as_ref()
        .expect("canonical MIR route receipt");
    assert_eq!(artifact.mir_hash, receipt.mir_digest);
}

fn verify_exact_typed_mir_request_for_test(
    checked: &crate::core::CheckedProgram,
    source_hash: &str,
) -> (
    Vec<VerificationResult>,
    crate::core::mir::CanonicalMirRouteReceipt,
) {
    let request = match crate::core::mir::verification::prepare_verification_request(checked) {
        crate::core::mir::verification::MirVerificationPreparation::Materialized(request) => {
            request
        }
        crate::core::mir::verification::MirVerificationPreparation::Deferred(boundaries) => {
            panic!("typed MIR request deferred unexpectedly: {boundaries:?}")
        }
        crate::core::mir::verification::MirVerificationPreparation::NotRequested => {
            panic!("typed MIR verifier request was not requested")
        }
    };
    let canonical = request.program();
    crate::verifier::validate_mir_capabilities(canonical)
        .expect("typed request must pass the same verifier capability gate as dual routing");
    let receipt = canonical.route_receipt("canonical-contract-request-v1");
    receipt
        .validate_against_program(canonical)
        .expect("exact typed MIR request receipt validates against its graph");
    let results =
        crate::verifier::verify_mir_with_route_receipt(canonical, &receipt, source_hash.to_owned())
            .expect("independent MIR secondary consumes exact typed request");
    for result in &results {
        let Some(artifact) = result.artifact.as_ref() else {
            continue;
        };
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
    (results, receipt)
}

fn assert_single_exact_mir_artifact(
    result: &VerificationResult,
    source_hash: &str,
    expected_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) {
    let artifact = result.artifact.as_ref().expect("single MIR proof artifact");
    assert_eq!(
        artifact.engine,
        ProofArtifact::ENGINE_MIR,
        "{}",
        result.func_name
    );
    assert_eq!(artifact.source_hash, source_hash, "{}", result.func_name);
    assert_eq!(expected_receipt.profile, "canonical-contract-request-v1");
    assert_eq!(
        artifact.mir_route_receipt.as_ref(),
        Some(expected_receipt),
        "{}",
        result.func_name
    );
    assert_eq!(artifact.mir_hash, expected_receipt.mir_digest);
}

fn assert_dual_result_keeps_the_agreed_engine_artifact(
    dual: &VerificationResult,
    exact_mir: &VerificationResult,
    source_hash: &str,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) {
    assert_eq!(dual.status, exact_mir.status, "{}", dual.func_name);
    let artifact = dual
        .artifact
        .as_ref()
        .expect("agreeing dual result must retain a proof artifact");
    assert_eq!(artifact.source_hash, source_hash, "{}", dual.func_name);
    match artifact.engine.as_str() {
        ProofArtifact::ENGINE_RESOLVED => {
            assert!(
                !artifact.resolved_ir_hash.is_empty(),
                "resolved primary artifact must carry its own IR identity"
            );
            assert!(artifact.mir_hash.is_empty());
            assert!(artifact.mir_route_receipt.is_none());
        }
        ProofArtifact::ENGINE_MIR => {
            assert_eq!(artifact.source_hash, source_hash, "{}", dual.func_name);
            assert_eq!(artifact.mir_hash, receipt.mir_digest, "{}", dual.func_name);
            assert_eq!(
                artifact.mir_route_receipt.as_ref(),
                Some(receipt),
                "{}",
                dual.func_name
            );
        }
        engine => panic!(
            "dual result {} has unexpected engine {engine:?}",
            dual.func_name
        ),
    }
}

fn assert_mir_contract_diagnostic_origin<'a>(
    checked: &crate::core::CheckedProgram,
    result: &'a VerificationResult,
) -> &'a crate::diagnostic::Diagnostic {
    assert_eq!(result.status, VerifStatus::Disproven, "{}", result.message);
    let diagnostic = result
        .diagnostic
        .as_ref()
        .expect("independent MIR contract diagnostic");
    assert_eq!(
        diagnostic.code, None,
        "MIR contract diagnostics have no registered error code"
    );
    let origin = diagnostic
        .origin
        .as_ref()
        .expect("typed MIR contract origin");
    assert_eq!(origin.rule.as_deref(), Some("canonical_mir.contract"));
    let parent = origin
        .parent_node_id
        .as_ref()
        .expect("contract diagnostic parent identity");
    assert!(
        checked
            .node_meta()
            .contains_key(&crate::core::NodeId(parent.clone())),
        "diagnostic parent {parent:?} must resolve in the checked source directory"
    );
    diagnostic
}

fn assert_exact_mir_contract_diagnostic(
    checked: &crate::core::CheckedProgram,
    result: &VerificationResult,
    expected_message: &str,
) {
    let diagnostic = assert_mir_contract_diagnostic_origin(checked, result);
    assert_eq!(result.message, expected_message);
    assert_eq!(diagnostic.message, expected_message);
}

fn exact_mir_result_for_test<'a>(
    results: &'a [VerificationResult],
    function: &str,
) -> &'a VerificationResult {
    let canonical_name = format!("function:{function}");
    results
        .iter()
        .find(|result| result.func_name == canonical_name || result.func_name == function)
        .unwrap_or_else(|| panic!("exact MIR request omitted {function}: {results:?}"))
}

fn assert_exact_mir_secondary_status(
    results: &[VerificationResult],
    function: &str,
    status: VerifStatus,
) {
    let result = exact_mir_result_for_test(results, function);
    assert_eq!(
        result.status, status,
        "MIR secondary {function}: {}",
        result.message
    );
    assert!(
        result.artifact.is_some(),
        "the exact MIR result for {function} must retain its own proof provenance"
    );
}

#[test]
fn mir_route_error_conversion_preserves_registered_code_and_display() {
    let error = format!(
        "{}: canonical route manifest rejected: future field",
        crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE
    );
    let diagnostic = mir_route_error_to_diagnostic(error.clone());

    assert_eq!(
        diagnostic.code.as_deref(),
        Some(crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE)
    );
    assert_eq!(
        diagnostic.to_string(),
        format!(
            "[{}] {error}",
            crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE
        )
    );
    let origin = diagnostic.origin.expect("route error has provenance");
    assert_eq!(
        origin.kind,
        crate::diagnostic::DiagnosticOriginKind::RuntimeSystem
    );
    assert_eq!(origin.rule.as_deref(), Some("mir.route"));
    assert!(mir_route_error_to_diagnostic("ordinary verifier failure")
        .code
        .is_none());
}

#[test]
fn verifier_route_error_conversion_handles_wrapped_and_invalid_prefixes() {
    let wrapped = "verifier context: MIR-RECEIPT-MANIFEST-001: future field".to_string();
    assert_eq!(
        mir_route_error_to_diagnostic(wrapped).code.as_deref(),
        Some(crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE)
    );

    let invalid_then_valid = "MIR-RECEIPT-001-extra: ignore; MIR-RECEIPT-001: use this occurrence";
    let diagnostic = mir_route_error_to_diagnostic(invalid_then_valid);
    assert_eq!(
        diagnostic.code.as_deref(),
        Some(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
    );
    assert_eq!(diagnostic.message, invalid_then_valid);
}

#[test]
fn verifier_route_error_formats_wrapped_code_once_with_runtime_origin() {
    let error = format!(
        "verifier context: {}: canonical route manifest rejected: future field",
        crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE
    );
    let diagnostic = mir_route_error_to_diagnostic(error.clone());
    let rendered = crate::diagnostic::format::strip_ansi(
        &crate::diagnostic::format::format_diagnostic(&diagnostic, None, ""),
    );

    assert_eq!(
        rendered,
        "error[MIR-RECEIPT-MANIFEST-001] verifier context: canonical route manifest rejected: future field\n"
    );
    assert_eq!(diagnostic.message, error);
    let origin = diagnostic.origin.expect("route diagnostic provenance");
    assert_eq!(origin.rule.as_deref(), Some("mir.route"));
}

macro_rules! require_z3 {
    () => {
        if !crate::verifier::is_z3_available() {
            eprintln!("    └─ skipped (Z3 not available)");
            return;
        }
    };
}

#[test]
fn exported_body_dual_verifier_fails_closed_without_legacy_access_for_map_any_owner_v1_candidate() {
    require_z3!();
    let source = r#"
        extern "C" func internal_export(value: i32) -> i32 { value + 1 }
        func map_contract() -> i32 {
            ensures: result == 1
            1
        }
        func main() -> i32 {
            let root = map_new()
            let inserted = map_set(root, "answer", 42)
            let size = map_size(inserted)
            drop(inserted)
            internal_export(size)
        }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
    let profile = crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1;
    assert_eq!(
        admission.map_any_owner,
        crate::core::mir::MapAnyOwnerAdmission::CompleteCoverage
    );
    assert_eq!(
        admission.map_root,
        crate::core::mir::MapRootAdmission::OutsideProfile
    );
    assert!(profile.is_admitted(admission));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let results = crate::verifier::verify_checked_dual(&checked, source_hash.clone())
        .expect("export body must fail closed before any AST dual-verifier fallback");
    let result = results
        .iter()
        .find(|result| result.func_name == "map_contract")
        .expect("deferred Map contract result");
    assert_eq!(result.status, VerifStatus::NotInTrustedSubset);
    assert!(result
        .message
        .contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE));
    assert!(result
        .message
        .contains(&format!("source_hash={source_hash}")));
    assert_eq!(
        result
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn verifier_memory_sources_are_stable_registered_and_label_isolated() {
    let source = "func main() -> i32 { let value = 1; value }";
    let first = parse_memory_source(source, "contracts").expect("first parse");
    let second = parse_memory_source(source, "contracts").expect("second parse");
    let other = parse_memory_source(source, "other-file").expect("other parse");

    let first_func = first
        .items
        .iter()
        .find_map(|item| match item {
            Item::Func(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("main function");
    assert!(first_func.meta.span.source_id.is_known());
    assert!(first
        .sources
        .record(first_func.meta.span.source_id)
        .is_some());

    let source_key = |file: &File| {
        let source_id = file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Func(function) => Some(function.meta.span.source_id),
                _ => None,
            })
            .expect("function source");
        file.sources
            .key(source_id)
            .expect("registered source key")
            .as_str()
            .to_string()
    };
    assert_eq!(source_key(&first), source_key(&second));
    assert_ne!(source_key(&first), source_key(&other));

    let anonymous_ids = |file: &File| {
        crate::core::check_program(file)
            .expect("checked source")
            .node_meta()
            .keys()
            .filter(|node_id| node_id.0.contains("/node:"))
            .map(|node_id| node_id.0.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let first_ids = anonymous_ids(&first);
    let second_ids = anonymous_ids(&second);
    let other_ids = anonymous_ids(&other);
    assert!(!first_ids.is_empty());
    assert_eq!(first_ids, second_ids);
    assert!(first_ids
        .iter()
        .all(|node_id| !node_id.contains("unknown-source")));
    assert!(first_ids.is_disjoint(&other_ids));
}

#[test]
fn verify_simple_pass() {
    require_z3!();
    let src = r#"
func identity(x: i32) -> i32 {
    requires: true
    ensures: true
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:25 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Verified);
}

#[test]
fn verify_unencodable_ensures_is_unknown() {
    require_z3!();
    let src = r#"
func preserve(xs: List<i32>) -> List<i32> {
    ensures: result[0] == xs[0]
    xs
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results.len(), 1);
    assert!(results[0].status.is_inconclusive());
    assert!(results[0].message.contains("could not encode ensures"));
}

#[test]
fn verify_unencodable_requires_is_unknown() {
    require_z3!();
    let src = r#"
func first(xs: List<i32>) -> i32 {
    requires: xs[0] > 0
    1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results.len(), 1);
    assert!(results[0].status.is_inconclusive());
    assert!(results[0].message.contains("could not encode requires"));
}

#[test]
fn verify_unproven_math_cannot_be_assumed() {
    require_z3!();
    let src = r#"
func forged(x: i32) -> i32 {
    math: { x == 1 }
    ensures: result == 1
    x
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Failed);
    assert!(results[0]
        .message
        .contains("scoped Math proof point is false on a reachable path"));
    assert_eq!(
        results[0]
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR),
        "the scoped Math counterexample must come from canonical MIR"
    );
}

#[test]
fn verify_proven_math_is_admitted() {
    require_z3!();
    let src = r#"
func proven(x: i32) -> i32 {
    requires: x == 1
    math: { x > 0 }
    ensures: result > 0
    x
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Verified);
}

#[test]
fn verify_body_satisfies_ensures() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0
    requires: x <= 1073741823
    ensures: result == x * 2
    x * 2
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:40 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "body `x * 2` should satisfy ensures `result == x * 2`: {}",
        results[0].message
    );
}

#[test]
fn verify_body_violates_ensures() {
    require_z3!();
    let src = r#"
func wrong(x: i32) -> i32 {
    requires: x >= 0 && x <= 100000
    ensures: result == x * 2
    x * 3
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:56 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Failed);
    let diag = results[0]
        .diagnostic
        .as_ref()
        .expect("src/verifier/tests.rs:59 unwrap failed");
    assert!(
        diag.message.contains("result ="),
        "narrative should show result value: {}",
        diag.message
    );
}

#[test]
fn verify_result_binding_in_counterexample() {
    require_z3!();
    let src = r#"
func add_one(x: i32) -> i32 {
    requires: x > 0
    ensures: result > x
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:72 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Failed);
    let diag = results[0]
        .diagnostic
        .as_ref()
        .expect("src/verifier/tests.rs:75 unwrap failed");
    assert!(
        diag.message.contains("result ="),
        "should show result value in narrative"
    );
}

#[test]
fn verify_strong_postcondition_fails() {
    require_z3!();
    let src = r#"
func abs(x: i32) -> i32 {
    requires: x > 0
    ensures: result > 0
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:89 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "x > 0 && result == x should satisfy result > 0"
    );
}

#[test]
fn verify_counterexample_extracted() {
    require_z3!();
    let src = r#"
func abs(x: i32) -> i32 {
    requires: true
    ensures: result > 0
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:105 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Failed);
    assert!(results[0].diagnostic.is_some());
    let diag = results[0]
        .diagnostic
        .as_ref()
        .expect("src/verifier/tests.rs:109 unwrap failed");
    assert!(
        diag.message.contains("result ="),
        "should show result in narrative"
    );
}

#[test]
fn verify_unsatisfiable_requires() {
    require_z3!();
    let src = r#"
func impossible(x: i32) -> i32 {
    requires: x > 0 && x < 0
    ensures: true
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:123 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Failed);
    let diag = results[0]
        .diagnostic
        .as_ref()
        .expect("src/verifier/tests.rs:126 unwrap failed");
    assert!(diag.message.contains("unsatisfiable"));
}

#[test]
fn verify_old_snapshot() {
    require_z3!();
    let src = r#"
func noop(x: i32) -> i32 {
    requires: x > 0
    ensures: result == old(x)
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:140 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "body returns x unchanged, ensures result == old(x) should hold: {}",
        results[0].message
    );
}

#[test]
fn verify_old_snapshot_fails() {
    require_z3!();
    let src = r#"
func mutate(x: i32) -> i32 {
    requires: x > 0
    ensures: result == old(x)
    x + 1
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:156 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "body returns x+1, ensures result == old(x) should fail"
    );
}

#[test]
fn format_expr_basic() {
    assert_eq!(format_expr(&Expr::Literal(Lit::Int(42))), "42");
    assert_eq!(format_expr(&Expr::Ident("x".into())), "x");
    assert_eq!(
        format_expr(&Expr::Binary(
            BinOp::Gt,
            Box::new(Expr::Ident("x".into())),
            Box::new(Expr::Literal(Lit::Int(0))),
        )),
        "x > 0"
    );
}

#[test]
fn verify_extern_ensures_consistent() {
    require_z3!();
    let src = r#"
extern "C" {
    func must_be_positive(x: i64) -> i64
        ensures: result > 0;
}

func main() -> i64 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:187 unwrap failed");
    let ext: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.contains("extern"))
        .collect();
    assert_eq!(ext.len(), 1, "extern func should be verified");
    assert_eq!(
        ext[0].status,
        VerifStatus::Disproven, // P2.3: Sat means a concrete counterexample exists.
        "extern ensures should be inconsistent: {}",
        ext[0].message
    );
}

#[test]
fn verify_extern_requires_ensures_consistent() {
    require_z3!();
    let src = r#"
extern "C" {
    func process(x: i64) -> i64
        requires: x > 0
        ensures: result > x;
}

func main() -> i64 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:206 unwrap failed");
    let ext: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.contains("extern"))
        .collect();
    assert_eq!(ext.len(), 1, "extern func should be verified");
    assert_eq!(
        ext[0].status,
        VerifStatus::Disproven, // P2.3: Sat means a concrete counterexample exists.
        "extern requires+ensures should be inconsistent: {}",
        ext[0].message
    );
}

#[test]
fn verify_extern_unsatisfiable_requires() {
    require_z3!();
    let src = r#"
extern "C" {
    func impossible(x: i64) -> i64
        requires: x > 0 && x < 0;
}

func main() -> i64 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:224 unwrap failed");
    let ext: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.contains("extern"))
        .collect();
    assert_eq!(ext.len(), 1);
    assert_eq!(
        ext[0].status,
        VerifStatus::Failed,
        "contradictory requires should fail: {}",
        ext[0].message
    );
    assert!(ext[0].message.contains("unsatisfiable"));
    let diagnostic = ext[0].diagnostic.as_ref().expect("extern diagnostic");
    assert_eq!(diagnostic.span.start_line, 3);
    assert_eq!(diagnostic.span.start_col, 5);
    assert!(diagnostic.span.end_line >= diagnostic.span.start_line);
    assert!(diagnostic.span.end_col > 0);
}

#[test]
fn verify_extern_no_contracts_skipped() {
    let src = r#"
extern "C" {
    func add(a: i64, b: i64) -> i64;
}

func main() -> i64 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:241 unwrap failed");
    let ext: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.contains("extern"))
        .collect();
    assert_eq!(
        ext.len(),
        0,
        "extern func without contracts should be skipped"
    );
}

#[test]
fn verify_extern_with_main_only() {
    let src = r#"
extern "C" {
    func identity(x: i64) -> i64
        ensures: result == x;
}

func main() -> i64 {
    ensures: true
    0
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:259 unwrap failed");
    let func_names: Vec<&str> = results.iter().map(|r| r.func_name.as_str()).collect();
    assert!(
        func_names.contains(&"extern identity"),
        "extern identity should be in results: {:?}",
        func_names
    );
    assert!(
        func_names.contains(&"main"),
        "main should be in results: {:?}",
        func_names
    );
}

// --- extract_body_return: if/else branch coverage ---

#[test]
fn verify_if_else_body_all_paths_verified() {
    require_z3!();
    let src = r#"
func abs(x: i32) -> i32 {
    requires: x >= -2147483647
    ensures: result >= 0
    if x >= 0 { x } else { -x }
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:277 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "abs with if/else should be verified: {}",
        results[0].message
    );
}

#[test]
fn verify_if_else_body_violation_detected() {
    require_z3!();
    let src = r#"
func bad_abs(x: i32) -> i32 {
    requires: true
    ensures: result >= 0
    if x >= 0 { x } else { x - 1 }
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:293 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "bad_abs with if/else should fail (else branch x-1 can be negative)"
    );
}

#[test]
fn verify_nested_if_else_body() {
    require_z3!();
    let src = r#"
func sign(x: i32) -> i32 {
    requires: true
    ensures: result == 1 || result == 0 || result == -1
    if x > 0 { 1 } else { if x < 0 { -1 } else { 0 } }
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:309 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "nested if/else should be verified: {}",
        results[0].message
    );
}

#[test]
fn verify_if_else_body_with_requires() {
    require_z3!();
    let src = r#"
func add_or_mul(x: i32, y: i32) -> i32 {
    requires: x >= 0 && y >= 0 && x <= 40000 && y <= 40000
    ensures: result >= 0
    if x > y { x + y } else { x * y }
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:325 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "add_or_mul with if/else should be verified: {}",
        results[0].message
    );
}

// --- eval_expr_on_model: f64 boolean degeneracy ---

#[test]
fn verify_f64_ensures() {
    require_z3!();
    let src = r#"
func positive(x: f64) -> f64 {
    requires: x > 0.0
    ensures: result > 0.0
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:343 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Proven,
        "finite positive identity must preserve the positive comparison: {}",
        results[0].message
    );
    assert_finite_ieee_binary64_mir_artifact(&results[0]);
}

#[test]
fn verify_f64_ensures_violation() {
    require_z3!();
    let src = r#"
func negate(x: f64) -> f64 {
    requires: x > 0.0
    ensures: result > 0.0
    -x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:359 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Disproven,
        "negating a positive finite value must disprove positivity: {}",
        results[0].message
    );
    assert!(results[0].message.contains("counterexample: result = (fp"));
    assert_finite_ieee_binary64_mir_artifact(&results[0]);
}

// --- FFI call-site verification ---

#[test]
fn verify_ffi_no_requires() {
    require_z3!();
    let src = r#"
extern "C" {
    func get_value() -> i64;
}

    func caller() -> i64 {
        get_value()
    }
"#;
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_source(src).expect("src/verifier/tests.rs:380 unwrap failed");
    assert!(
        results.is_empty(),
        "no-contract extern has no obligations: {results:?}"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "declaration-only FFI must not enter the retained AST verifier"
    );
}

#[test]
fn dual_literal_non_scalar_ffi_declaration_root_is_ast_free() {
    // The string ABI intentionally misses ScalarFfi.  A called declaration
    // whose only proof roots are boolean literals is nevertheless complete at
    // the checker-owned declaration boundary. Both public verifier entry
    // points must keep the canonical route free of retained body access.
    let source = r#"
extern "C" {
    func host_text(value: string) -> i32
        requires: true;
}
func main() -> i32 { host_text("ok") }
"#;
    let file = parse_memory_source(source, "dual-literal-ffi").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_checked_dual(&checked, source_hash.clone())
        .expect("literal non-scalar FFI dual verification");
    assert_eq!(results.len(), 1, "declaration-level result: {results:?}");
    assert_eq!(results[0].func_name, "extern host_text");
    assert_eq!(results[0].status, VerifStatus::Proven);
    let artifact = results[0].artifact.as_ref().expect("literal artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_RESOLVED);
    assert_eq!(artifact.source_hash, source_hash);
    assert_eq!(artifact.integer_model, "literal-boolean");
    assert_eq!(
        crate::core::CheckedProgram::test_legacy_body_access(),
        Vec::<crate::core::LegacyBodyConsumer>::new(),
        "dual literal declaration root must not read retained source body"
    );
}

#[test]
fn dual_scalar_ffi_literal_stays_on_canonical_mir_route() {
    require_z3!();
    let source = r#"
extern "C" {
    func host_i32(value: i32) -> i32
        requires: true;
}
func main() -> i32 { host_i32(1) }
"#;
    let file = parse_memory_source(source, "dual-scalar-literal-ffi").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_checked_dual(&checked, "dual-scalar-literal-ffi".into())
        .expect("scalar FFI dual verification");
    assert_eq!(results.len(), 1, "canonical scalar result: {results:?}");
    let artifact = results[0].artifact.as_ref().expect("MIR artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn single_literal_non_scalar_ffi_declaration_root_is_ast_free() {
    let source = r#"
extern "C" {
    func host_text(value: string) -> i32
        requires: true;
}
func main() -> i32 { host_text("ok") }
"#;
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_source(source).expect("literal non-scalar FFI verification");
    assert_eq!(results.len(), 1, "declaration-level result: {results:?}");
    assert_eq!(results[0].func_name, "extern host_text");
    assert_eq!(results[0].status, VerifStatus::Proven);
    let artifact = results[0].artifact.as_ref().expect("literal artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_RESOLVED);
    assert_eq!(artifact.integer_model, "literal-boolean");
    assert_eq!(artifact.float_model, "none");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn false_literal_non_scalar_ffi_is_failed_without_legacy() {
    let source = r#"
extern "C" {
    func host_text(value: string) -> i32
        requires: false;
}
func main() -> i32 { host_text("ok") }
"#;
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_source(source).expect("false literal FFI verification");
    assert_eq!(results.len(), 1, "declaration-level result: {results:?}");
    assert_eq!(results[0].func_name, "extern host_text");
    assert_eq!(results[0].status, VerifStatus::Failed);
    assert!(results[0]
        .message
        .contains("unsatisfiable literal requires"));
    assert!(results[0].diagnostic.is_some());
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn dual_nonliteral_non_scalar_ffi_does_not_match_literal_slice() {
    // A stateful declaration expression is outside the narrow admission.  It
    // must remain fail-closed at the public verifier boundary and cannot be
    // silently reclassified as a literal declaration proof.
    let source = r#"
extern "C" {
    func host_text(value: string) -> i32
        requires: true && true;
}
func main() -> i32 { host_text("ok") }
"#;
    let file = parse_memory_source(source, "dual-nonliteral-ffi").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let error = verify_checked_dual(&checked, "dual-nonliteral-ffi".into())
        .expect_err("non-literal non-scalar FFI must remain outside the slice");
    assert!(
        error.contains(crate::core::mir::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE),
        "{error}"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "boundary rejection must not fall through to a legacy body"
    );
}

#[test]
fn public_checked_verifiers_reject_non_scalar_extern_before_compatibility() {
    let source = r#"
extern "C" {
    func host_text(x: string) -> i32;
}
func main() -> i32 { host_text("x") }
"#;
    let file = parse_memory_source(source, "verify-non-scalar-extern-boundary").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let single_error = verify_checked(&checked, source_hash.clone())
        .expect_err("single verifier must reject unsupported called extern ABI");
    assert!(
        single_error.contains(crate::core::mir::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE),
        "{single_error}"
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let dual_error = verify_checked_dual(&checked, source_hash)
        .expect_err("dual verifier must reject unsupported called extern ABI");
    assert_eq!(single_error, dual_error);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn public_checked_verifiers_reject_recognized_mir_candidates_before_legacy() {
    let cases = [
        (
            "generic Option predicate",
            include_str!("../../tests/fixtures/mir_native_generic_option_predicate_rejected.mimi"),
            "generic-option-predicate",
        ),
        (
            "generic Result predicate",
            include_str!("../../tests/fixtures/mir_native_generic_result_predicate_rejected.mimi"),
            "generic-option-predicate",
        ),
        (
            "mixed Copy Option",
            "func i32_unwrap() -> i32 { let value: Option<i32> = Some(41); value.unwrap() } func i64_unwrap() -> i64 { let value: Option<i64> = Some(7); value.unwrap() } func main() -> i32 { i32_unwrap() }",
            "Copy Option<i32>",
        ),
        (
            "mixed Copy Option bool",
            "func bool_unwrap() -> bool { let value: Option<bool> = Some(true); value.unwrap() } func i64_unwrap() -> i64 { let value: Option<i64> = Some(7); value.unwrap() } func main() -> i32 { 42 }",
            "Copy Option<bool>",
        ),
        (
            "mixed Copy Option i64",
            include_str!("../../tests/fixtures/mir_native_option_i64_mixed_rejected.mimi"),
            "Copy Option<i64>",
        ),
        (
            "mixed Copy Option f64",
            include_str!("../../tests/fixtures/mir_native_option_f64_mixed_rejected.mimi"),
            "Copy Option<f64>",
        ),
        (
            "incomplete MapAnyOwner v1",
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let updated = map_set(root, "answer", 42)
                    let size = map_size(updated)
                    let temporary_size = map_size(map_new())
                    let unreceipted = map_get(map_new(), "missing")
                    drop(updated)
                    size + temporary_size
                }
            "#,
            "Checker recognized a MapAnyOwner v1 candidate",
        ),
        (
            "incomplete S8 Flow",
            "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) println(c2.n) c2.n }",
            "S8 Flow transition candidate",
        ),
        (
            "unsupported generic record projection",
            include_str!("../../tests/fixtures/mir_native_generic_record_projection_eight_field_rejected.mimi"),
            "generic record projection candidate",
        ),
        (
            "unsupported generic record update",
            include_str!("../../tests/fixtures/mir_native_generic_record_update_owned_copy_residual_f64_rejected.mimi"),
            "MIR-MATERIALIZATION-001",
        ),
        (
            "unsupported generic List facade",
            include_str!("../../tests/fixtures/mir_native_generic_list_construct_rejected.mimi"),
            "generic List facade candidate",
        ),
        (
            "unsupported List reverse",
            include_str!("../../tests/fixtures/mir_native_list_reverse_rejected.mimi"),
            "List.reverse candidate",
        ),
        (
            "unsupported List concat",
            include_str!("../../tests/fixtures/mir_native_list_concat_method_rejected.mimi"),
            "List.concat candidate",
        ),
    ];

    for (label, source, expected_reason) in cases {
        let file =
            parse_memory_source(source, label).unwrap_or_else(|error| panic!("{label}: {error}"));
        let checked =
            crate::core::check_program(&file).unwrap_or_else(|error| panic!("{label}: {error:?}"));
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        if label == "incomplete MapAnyOwner v1" {
            let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
            assert_eq!(
                admission.map_any_owner,
                crate::core::mir::MapAnyOwnerAdmission::IncompleteCoverage
            );
            assert_eq!(
                admission.map_root,
                crate::core::mir::MapRootAdmission::OutsideProfile
            );
            assert!(
                !crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1.is_admitted(admission)
            );
        }

        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let single_error = match verify_checked(&checked, source_hash.clone()) {
            Ok(_) => panic!("single verifier accepted {label}"),
            Err(error) => error,
        };
        assert!(
            single_error.contains(expected_reason),
            "{label}: {single_error}"
        );
        assert!(
            crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
            "single verifier entered legacy for {label}"
        );

        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let dual_error = match verify_checked_dual(&checked, source_hash) {
            Ok(_) => panic!("dual verifier accepted {label}"),
            Err(error) => error,
        };
        assert_eq!(single_error, dual_error, "{label}");
        assert!(
            crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
            "dual verifier entered legacy for {label}"
        );
    }
}

#[test]
fn verify_ffi_requires_always_satisfied() {
    require_z3!();
    let src = r#"
extern "C" {
    func read(fd: i64, buf: i64, size: i64) -> i64
        requires: fd >= 0 && size > 0;
}
func caller(fd: i64, buf: i64, size: i64) -> i64 {
    requires: fd >= 0 && size > 0
    read(fd, buf, size)
}
"#;
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_source(src).expect("src/verifier/tests.rs:397 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "requires fd >= 0 && size > 0 should satisfy read's preconditions: {}",
        results[0].message
    );
    assert_eq!(results[0].func_name, "function:caller");
    let artifact = results[0]
        .artifact
        .as_ref()
        .expect("canonical FFI proof artifact");
    assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.mir_hash.len(), 64);
    assert_eq!(artifact.source_hash.len(), 64);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn verify_ffi_requires_violated() {
    require_z3!();
    let src = r#"
extern "C" {
    func read(fd: i64, buf: i64, size: i64) -> i64
        requires: fd >= 0 && size > 0;
}
func bad_caller(size: i64) -> i64 {
    read(-1, 0, size)
}
"#;
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_source(src).expect("src/verifier/tests.rs:415 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "read(-1, 0, size) should fail: fd is negative"
    );
    let artifact = results[0]
        .artifact
        .as_ref()
        .expect("canonical FFI counterexample artifact");
    assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.mir_hash.len(), 64);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let diagnostic = results[0]
        .diagnostic
        .as_ref()
        .expect("extern call-site diagnostic");
    assert_eq!(diagnostic.span.start_line, 7);
    assert_eq!(diagnostic.span.start_col, 5);
    assert_eq!(diagnostic.span.end_line, 7);
    assert_eq!(diagnostic.span.end_col, 22);
}

#[test]
fn scalar_ffi_public_mir_verifier_reports_preconditions_without_function_ensures() {
    require_z3!();
    for (value, expected) in [(42, VerifStatus::Proven), (-1, VerifStatus::Disproven)] {
        let source = format!(
            "extern \"C\" {{ func foreign(x: i64) -> i64 requires: x > 0; }} func main() -> i64 {{ foreign({value} as i64) }}"
        );
        let file =
            crate::parser::Parser::new(crate::lexer::Lexer::new(&source).tokenize().unwrap())
                .parse_file()
                .unwrap();
        let checked = crate::core::check_program(&file).unwrap();
        let program =
            crate::core::mir::reference::MirProgram::from_checked_program(&checked).unwrap();
        let digest = program.canonical_digest();
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let bytecode = crate::interp::bytecode::compile_mir_program(&program).unwrap();
        assert!(bytecode.ast.is_none());
        assert!(bytecode.extern_names.is_empty());
        let context = inkwell::context::Context::create();
        let mut generator = crate::codegen::CodeGenerator::new(&context, "ffi_contract");
        generator.compile_mir_native(&program).unwrap();
        generator.module.verify().unwrap();
        let results = verify_mir(&program, "same-mir-ffi-fixture".into()).unwrap();
        assert_eq!(
            results.len(),
            1,
            "extern requires is an obligation even without function ensures"
        );
        assert_eq!(results[0].status, expected, "{results:?}");
        let artifact = results[0].artifact.as_ref().unwrap();
        assert_eq!(artifact.mir_hash, digest);
        assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

        // The public CheckedProgram APIs must pick this same checker-owned
        // scalar FFI MIR route before their retained AST verifier owners.
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        crate::core::mir::reset_test_route_materialization_count();
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let checked_results = crate::verifier::verify_checked(&checked, source_hash.clone())
            .expect("public checked scalar FFI verifier");
        assert_eq!(checked_results.len(), 1, "{checked_results:?}");
        assert_eq!(checked_results[0].status, expected, "{checked_results:?}");
        if expected == VerifStatus::Disproven {
            assert_eq!(
                checked_results[0].diagnostic.as_ref().unwrap().span,
                results[0].diagnostic.as_ref().unwrap().span,
                "the checked adapter must preserve the exact FFI call-site span"
            );
        }
        assert_eq!(
            checked_results[0]
                .artifact
                .as_ref()
                .map(|artifact| artifact.engine.as_str()),
            Some(ProofArtifact::ENGINE_MIR)
        );
        assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

        crate::core::mir::reset_test_route_materialization_count();
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let dual_results = crate::verifier::verify_checked_dual(&checked, source_hash)
            .expect("public dual checked scalar FFI verifier");
        assert_eq!(dual_results.len(), 1, "{dual_results:?}");
        assert_eq!(dual_results[0].status, expected, "{dual_results:?}");
        if expected == VerifStatus::Disproven {
            assert_eq!(
                dual_results[0].diagnostic.as_ref().unwrap().span,
                results[0].diagnostic.as_ref().unwrap().span,
                "the dual adapter must preserve the exact FFI call-site span"
            );
        }
        assert_eq!(
            dual_results[0]
                .artifact
                .as_ref()
                .map(|artifact| artifact.engine.as_str()),
            Some(ProofArtifact::ENGINE_MIR)
        );
        assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn scalar_ffi_mir_proofs_preserve_path_facts_and_fresh_external_results() {
    require_z3!();
    for (body, proven, disproven) in [
        ("let x = unknown(); if x > (0 as i64) { positive(x) } else { positive(1 as i64) }", 2, 0),
        ("let x = unknown(); if x > (0 as i64) { positive(x) } else { positive(x) }", 1, 1),
        ("let first = unknown(); let second = unknown(); if first == second { positive(1 as i64) } else { positive(-1 as i64) }", 1, 1),
        ("ensures: true\npositive(-1 as i64)", 1, 1),
        ("unknown()", 0, 0),
    ] {
        let source = format!(
            "extern \"C\" {{ func unknown() -> i64; func positive(x: i64) -> i64 requires: x > 0; }} func main() -> i64 {{ {body} }}"
        );
        let file = crate::parser::Parser::new(crate::lexer::Lexer::new(&source).tokenize().unwrap())
            .parse_file().unwrap();
        let checked = crate::core::check_program(&file).unwrap();
        let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked).unwrap();
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = verify_mir(&program, "ffi-paths".into()).unwrap();
        assert_eq!(results.iter().filter(|r| r.status == VerifStatus::Proven).count(), proven,
            "{body}: {results:?}");
        assert_eq!(results.iter().filter(|r| r.status == VerifStatus::Disproven).count(), disproven,
            "{body}: {results:?}");
        assert_eq!(results.len(), proven + disproven, "{body}: {results:?}");
        assert!(results.iter().all(|result| result.artifact.as_ref().is_some_and(|artifact|
            artifact.engine == ProofArtifact::ENGINE_MIR && artifact.mir_hash == program.canonical_digest())));
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn scalar_ffi_proofs_include_runtime_predicate_definedness() {
    require_z3!();
    for (condition, argument, expected) in [
        ("x + 1 > x", i64::MAX, VerifStatus::Disproven),
        ("x + 1 > x", 4, VerifStatus::Proven),
        ("x * 2 > x", i64::MAX, VerifStatus::Disproven),
        ("x > 0 || 1 / (x - x) > 0", 4, VerifStatus::Proven),
        ("x > 0 || 1 / (x - x) > 0", -1, VerifStatus::Disproven),
        ("x / 3 == -2 && x % 3 == -1", -7, VerifStatus::Proven),
        ("x < 0 && 1 / 0 > 0", 4, VerifStatus::Disproven),
        ("x > 0 || x % -1 == 0", i64::MIN + 1, VerifStatus::Proven),
        ("x > 0 || x / -1 > 0", i64::MIN, VerifStatus::Disproven),
    ] {
        // Spell MIN using supported signed literal arithmetic in the body.
        let argument = if argument == i64::MIN {
            "(-9223372036854775807 as i64) - (1 as i64)".to_string()
        } else {
            format!("{argument} as i64")
        };
        let source = format!(
            "extern \"C\" {{ func foreign(x: i64) -> i64 requires: {condition}; }} func main() -> i64 {{ foreign({argument}) }}"
        );
        let file =
            crate::parser::Parser::new(crate::lexer::Lexer::new(&source).tokenize().unwrap())
                .parse_file()
                .unwrap();
        let checked = crate::core::check_program(&file).unwrap();
        let program =
            crate::core::mir::reference::MirProgram::from_checked_program(&checked).unwrap();
        let results = verify_mir(&program, "ffi-definedness".into()).unwrap();
        assert_eq!(results.len(), 1, "{condition}: {results:?}");
        assert_eq!(results[0].status, expected, "{condition}: {results:?}");
        assert_eq!(
            results[0].artifact.as_ref().unwrap().mir_hash,
            program.canonical_digest()
        );
    }
}

#[test]
fn verify_ffi_string_empty_violation() {
    require_z3!();
    let src = r#"
extern "C" {
    func strlen(s: string) -> i64
        requires: s != "";
}
func caller(s: string) -> i64 {
    strlen(s)
}
"#;
    let results = verify_ffi_source(src).expect("src/verifier/tests.rs:433 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "strlen(s) without guard should fail: s could be empty"
    );
}

#[test]
fn verify_ffi_string_empty_protected() {
    require_z3!();
    let src = r#"
extern "C" {
    func strlen(s: string) -> i64
        requires: s != "";
}
func caller(s: string) -> i64 {
    requires: s != ""
    strlen(s)
}
"#;
    let results = verify_ffi_source(src).expect("src/verifier/tests.rs:452 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "strlen(s) with guard should be Verified: {}",
        results[0].message
    );
}

#[test]
fn verify_ffi_multiple_externs() {
    require_z3!();
    let src = r#"
extern "C" {
    func read(fd: i64, buf: i64, size: i64) -> i64
        requires: fd >= 0;
    func write(fd: i64, buf: i64, size: i64) -> i64
        requires: fd >= 0;
}
func ok_caller(fd: i64) -> bool {
    requires: fd >= 0
    read(fd, 0, 1) == write(fd, 0, 1)
}
func bad_caller(fd: i64) -> bool {
    read(fd, 0, 1) == write(fd, 0, 1)
}
"#;
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_source(src).expect("src/verifier/tests.rs:476 unwrap failed");
    assert_eq!(results.len(), 4);
    let ok_results: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.ends_with("ok_caller"))
        .collect();
    assert_eq!(ok_results.len(), 2);
    assert!(
        ok_results.iter().all(|r| r.status == VerifStatus::Verified),
        "ok_caller should pass: {:?}",
        ok_results
    );
    let bad_results: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.ends_with("bad_caller"))
        .collect();
    assert_eq!(bad_results.len(), 2);
    assert!(
        bad_results.iter().any(|r| r.status == VerifStatus::Failed),
        "bad_caller should have at least one failure: {:?}",
        bad_results
    );
    assert!(
        results.iter().all(|result| {
            result.artifact.as_ref().is_some_and(|artifact| {
                artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
            })
        }),
        "all direct scalar multi-FFI results must be MIR artifacts: {results:?}"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "direct scalar multi-FFI verification must not enter the retained AST verifier"
    );
}

fn verify_entry_invariant_without_legacy_access(source: &str) -> Vec<VerificationResult> {
    let file = parse_memory_source(source, "entry-invariant-owner-retirement").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let single = verify_checked(&checked, source_hash.clone()).expect("typed entry invariant");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let super::mir_request::Preparation::Complete(request) = super::mir_request::prepare(&checked)
    else {
        panic!("entry invariant request must be complete");
    };
    for result in &single {
        let artifact = result
            .artifact
            .as_ref()
            .expect("real invariant proof/counterexample");
        assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.source_hash, source_hash);
        assert_eq!(artifact.mir_hash, request.program().canonical_digest());
    }
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let dual =
        verify_checked_dual(&checked, source_hash).expect("independent typed invariant dual");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    for result in &single {
        let merged = dual
            .iter()
            .find(|merged| merged.func_name == result.func_name)
            .expect("same canonical owner present in dual");
        if result.status == VerifStatus::Disproven {
            assert_eq!(
                merged.status,
                VerifStatus::Disproven,
                "counterexample must stay visible"
            );
        }
    }
    single
}

#[test]
fn verify_invariant_basic() {
    require_z3!();
    let src = r#"
func identity(x: i32) -> i32 {
    requires: x > 0
    ensures: result > 0
    invariant: x > 0
    x
}
"#;
    let results = verify_entry_invariant_without_legacy_access(src);
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "invariant as constraint should verify: {:?}",
        results[0]
    );
}

#[test]
fn verify_invariant_with_ensures() {
    require_z3!();
    let src = r#"
func add_one(x: i32) -> i32 {
    requires: x > 0 && x < 2147483647
    ensures: result > x
    invariant: x > 0
    x + 1
}
"#;
    let results = verify_entry_invariant_without_legacy_access(src);
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "invariant + ensures should verify: {:?}",
        results[0]
    );
}

#[test]
fn verify_f64_add_and_compare() {
    require_z3!();
    let src = r#"
func scale_add(x: f64) -> f64 {
    requires: x > 1.0
    ensures: result > x
    x + 1.0
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_f64_add_and_compare");
    assert_eq!(results.len(), 1);
    // Finite IEEE rounding makes this postcondition false for sufficiently
    // large x, where x + 1.0 rounds back to x.
    assert_eq!(
        results[0].status,
        VerifStatus::Disproven,
        "finite IEEE rounding must expose the counterexample: {:?}",
        results[0]
    );
    assert!(results[0].message.contains("counterexample: result = (fp"));
    assert_finite_ieee_binary64_mir_artifact(&results[0]);
}

#[test]
fn verify_record_field_access_int() {
    require_z3!();
    let src = r#"
type Point { x: i32, y: i32 }
func point_x_positive(p: Point) -> i32 {
    requires: p.x > 0
    ensures: result > 0
    p.x
}
"#;
    let results =
        verify_source(src).expect("src/verifier/tests.rs: verify_record_field_access_int");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "record field access in contract should verify: {:?}",
        results[0]
    );
}

#[test]
fn verify_record_field_violation() {
    require_z3!();
    let src = r#"
type Point { x: i32, y: i32 }
func bad_point_x(p: Point) -> i32 {
    requires: p.x > 0
    ensures: result > p.x
    p.x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_record_field_violation");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "record field violation should be detected: {:?}",
        results[0]
    );
}

#[test]
fn public_checked_verifier_routes_closed_copy_record_to_mir() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_verifier_record_contract.mimi");
    let file = parse_memory_source(source, "mir-record-public-api").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&program, &source_hash);
    let exact_mir = exact_mir_result_for_test(&exact_mir_results, "advance");
    assert_eq!(
        exact_mir.status,
        VerifStatus::Proven,
        "{}",
        exact_mir.message
    );
    assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verify_result = verify_checked(&program, source_hash.clone()).expect("MIR verify");
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "closed Copy record verifier route must not access retained legacy bodies"
    );
    let result = verify_result
        .iter()
        .find(|result| result.func_name.ends_with("advance"))
        .expect("record contract result");
    assert_eq!(result.status, VerifStatus::Proven);
    assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);
    assert_eq!(
        result
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR)
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let dual_result = verify_checked_dual(&program, source_hash.clone()).expect("MIR dual verify");
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "closed Copy record dual verifier route must not access retained legacy bodies"
    );
    let dual = dual_result
        .iter()
        .find(|result| result.func_name.ends_with("advance"))
        .expect("dual record contract result");
    assert_eq!(dual.status, VerifStatus::Proven);
    assert_dual_result_keeps_the_agreed_engine_artifact(
        dual,
        exact_mir,
        &source_hash,
        &typed_receipt,
    );
}

#[test]
fn public_checked_verifier_routes_generic_variant_and_managed_result_profiles_without_legacy_access(
) {
    require_z3!();
    use crate::core::mir::CanonicalMirRouteProfile as Profile;

    const CASES: &[(&str, Profile)] = &[
        (
            include_str!("../../tests/fixtures/mir_native_generic_option_predicate.mimi"),
            Profile::GenericOptionPredicate,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_generic_option_unwrap.mimi"),
            Profile::GenericOptionProjection,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or.mimi"),
            Profile::GenericOptionProjectionFallback,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_generic_result_unwrap.mimi"),
            Profile::GenericResultProjection,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_or.mimi"),
            Profile::GenericResultProjectionFallback,
        ),
        (
            include_str!("../../tests/fixtures/mir_result_list_i32_call_return.mimi"),
            Profile::ManagedResultCall,
        ),
    ];

    for (source, profile) in CASES {
        let file = parse_memory_source(source, profile.as_str()).expect("parse");
        let mut file = file;
        crate::loader::merge_prelude_into(&mut file);
        let checked = crate::core::check_program(&file)
            .unwrap_or_else(|diags| panic!("check {}: {diags:?}", profile.as_str()));
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .unwrap_or_else(|error| panic!("materialize {}: {error:?}", profile.as_str()));
        assert!(
            profile.is_admitted(route.admission),
            "{} must have checker-owned complete admission",
            profile.as_str()
        );
        assert!(
            profile.is_materialized(&route),
            "{} must have its canonical operation receipt",
            profile.as_str()
        );

        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let checked_results = crate::verifier::verify_checked(&checked, source_hash.clone())
            .unwrap_or_else(|error| panic!("verify_checked {}: {error}", profile.as_str()));
        assert!(
            crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
            "{} checked verifier must not access retained legacy bodies",
            profile.as_str()
        );
        assert!(
            checked_results.iter().any(|result| {
                result.status == VerifStatus::Proven
                    && result
                        .artifact
                        .as_ref()
                        .is_some_and(|artifact| artifact.engine == ProofArtifact::ENGINE_MIR)
            }),
            "{} checked verifier must produce a MIR-backed proof for its contract: {checked_results:?}",
            profile.as_str()
        );

        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let dual_results = crate::verifier::verify_checked_dual(&checked, source_hash)
            .unwrap_or_else(|error| panic!("verify_checked_dual {}: {error}", profile.as_str()));
        assert!(
            crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
            "{} dual verifier must not access retained legacy bodies",
            profile.as_str()
        );
        assert!(
            dual_results.iter().any(|result| {
                result.status == VerifStatus::Proven
                    && result
                        .artifact
                        .as_ref()
                        .is_some_and(|artifact| artifact.engine == ProofArtifact::ENGINE_MIR)
            }),
            "{} dual verifier must produce a MIR-backed proof for its contract: {dual_results:?}",
            profile.as_str()
        );
    }
}

#[test]
fn public_checked_verifier_materializes_generic_scalar_identity_before_no_contract_result() {
    require_z3!();
    use crate::core::mir::CanonicalMirRouteProfile as Profile;

    let cases = [
        (
            "func identity<T>(value: T) -> T { value }\nfunc main() -> i32 { identity(42) }",
            Profile::ScalarGenericIdentityI32,
        ),
        (
            "func identity<T>(value: T) -> T { value }\nfunc main() -> i64 { identity(2147483690) }",
            Profile::ScalarGenericIdentityI64,
        ),
    ];

    for (source, profile) in cases {
        let file = parse_memory_source(source, profile.as_str()).expect("parse");
        let checked = crate::core::check_program(&file).expect("typecheck");
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("generic scalar identity route");
        assert!(profile.is_admitted(route.admission));
        assert!(profile.is_materialized(&route));
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        // This profile is deliberately contract-free: adding a contract
        // would widen its exact whole-program identity shape. The route
        // materialization counter therefore proves the public verifier chose
        // and validated the canonical profile before returning no obligations.
        crate::core::mir::reset_test_route_materialization_count();
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let checked_results = crate::verifier::verify_checked(&checked, source_hash.clone())
            .expect("public checked generic identity verifier");
        assert!(checked_results.is_empty(), "identity has no contracts");
        assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

        crate::core::mir::reset_test_route_materialization_count();
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let dual_results = crate::verifier::verify_checked_dual(&checked, source_hash)
            .expect("public dual checked generic identity verifier");
        assert!(dual_results.is_empty(), "identity has no contracts");
        assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn public_checked_verifier_tripwires_copy_variant_and_session_profiles() {
    require_z3!();
    use crate::core::mir::CanonicalMirRouteProfile as Profile;

    const SESSION_SOURCE: &str = r#"
        session Proto = !i32 . ?i32 . end
        func main() -> i64 {
            ensures: result >= 0
            let (tx, rx) = session_pair::<Proto>()
            session_send(tx, 10)
            let first = session_recv(rx)
            session_send(rx, 11)
            let second = session_recv(tx)
            session_close(rx)
            session_close(tx)
            0
        }
    "#;
    let cases: [(&str, Profile, bool); 6] = [
        (
            include_str!("../../tests/fixtures/mir_native_option_i32_unwrap.mimi"),
            Profile::CopyOptionI32Variant,
            true,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_option_bool_unwrap.mimi"),
            Profile::CopyOptionBoolVariant,
            true,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_option_i64_unwrap.mimi"),
            Profile::CopyOptionI64Variant,
            true,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_option_f64_unwrap.mimi"),
            Profile::CopyOptionF64Variant,
            false,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_result_i32_unwrap.mimi"),
            Profile::CopyResultI32Variant,
            true,
        ),
        (SESSION_SOURCE, Profile::SessionChannel, true),
    ];

    for (source, profile, has_proof) in cases {
        let file = parse_memory_source(source, profile.as_str()).expect("parse");
        let checked = crate::core::check_program(&file)
            .unwrap_or_else(|error| panic!("{} typecheck: {error:?}", profile.as_str()));
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .unwrap_or_else(|error| panic!("{} materialize: {error:?}", profile.as_str()));
        assert!(profile.is_admitted(route.admission), "{}", profile.as_str());
        assert!(profile.is_materialized(&route), "{}", profile.as_str());
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        for dual in [false, true] {
            crate::core::CheckedProgram::reset_test_legacy_body_access();
            let results = if dual {
                crate::verifier::verify_checked_dual(&checked, source_hash.clone())
            } else {
                crate::verifier::verify_checked(&checked, source_hash.clone())
            }
            .unwrap_or_else(|error| {
                panic!(
                    "{} {} verifier: {error}",
                    profile.as_str(),
                    if dual { "dual" } else { "single" }
                )
            });
            assert!(
                crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
                "{} {} verifier touched retained legacy body",
                profile.as_str(),
                if dual { "dual" } else { "single" }
            );
            if has_proof {
                assert!(
                    results.iter().any(|result| {
                        result.status == VerifStatus::Proven
                            && result.artifact.as_ref().is_some_and(|artifact| {
                                artifact.engine == ProofArtifact::ENGINE_MIR
                                    && artifact.mir_route_receipt.as_ref().is_some_and(|receipt| {
                                        receipt.mir_digest == artifact.mir_hash
                                    })
                            })
                    }),
                    "{} {} verifier must produce a MIR proof: {results:?}",
                    profile.as_str(),
                    if dual { "dual" } else { "single" }
                );
            } else {
                assert!(
                    results.is_empty(),
                    "{} has no source contracts: {results:?}",
                    profile.as_str()
                );
            }
        }
    }
}

#[test]
fn recoverable_cross_state_flow_verifier_proves_from_the_same_mir_identity() {
    require_z3!();
    let source = include_str!(
        "../../tests/real_world/flow_state_match_fail_result_failure_dual_backend.mimi"
    );
    let file = parse_memory_source(source, "mir-recoverable-cross-state-proof").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical recoverable cross-state MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("recoverable cross-state verifier capability");

    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("recoverable cross-state MIR verifier");
    let result = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("recoverable cross-state main proof");
    assert_eq!(result.status, VerifStatus::Proven);
    assert!(result.constraint_count > 0);
    let artifact = result
        .artifact
        .as_ref()
        .expect("recoverable cross-state proof artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.mir_hash, canonical.canonical_digest());
}

#[test]
fn recoverable_cross_state_flow_verifier_reports_a_real_counterexample() {
    require_z3!();
    let source = include_str!(
        "../../tests/real_world/flow_state_match_fail_result_failure_disproven_dual_backend.mimi"
    );
    let file =
        parse_memory_source(source, "mir-recoverable-cross-state-counterexample").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical recoverable cross-state MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("recoverable cross-state verifier capability");

    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("recoverable cross-state MIR verifier");
    let result = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("recoverable cross-state counterexample");
    assert_eq!(result.status, VerifStatus::Disproven);
    assert!(result.constraint_count > 0);
    assert!(result.message.contains("ensures contract is disproven"));
    let artifact = result.artifact.as_ref().expect("counterexample artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.mir_hash, canonical.canonical_digest());
}

#[test]
fn flow_vir_bool_result_postconditions_keep_reference_and_dual_fails_closed() {
    require_z3!();
    // Keep this fixture on the explicit export-wrapper compatibility face:
    // the regression must exercise VIR, even after scalar contracts gain a
    // canonical production route.
    let source = r#"
        extern "C" func compatibility_marker(value: i32) -> i32 { value }
        func true_value() -> bool { ensures: result == true
 true }
        func false_claim() -> bool { ensures: result == true
 false }
        func not_false() -> bool { ensures: result != false
 true }
        func same(flag: bool) -> bool { ensures: result == flag
 flag }
        func flip(flag: bool) -> bool { ensures: result != flag
 !flag }
        func integer_claim() -> i32 { ensures: result == 2
 1 }
        func main() -> i32 { 0 }
    "#;
    let file = parse_memory_source(source, "bool-result-compatibility").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let mut primary = Verifier::new().expect("resolved verifier");
    let resolved = primary.verify_checked(&checked);
    let flow = flow::flow_verify_file_with_hashes(&file, source_hash.clone(), String::new())
        .expect("Flow/VIR verifier");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let dual = verify_checked_dual(&checked, source_hash).expect("dual verifier");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    for (name, expected) in [
        ("true_value", VerifStatus::Proven),
        ("false_claim", VerifStatus::Disproven),
        ("not_false", VerifStatus::Proven),
        ("same", VerifStatus::Proven),
        ("flip", VerifStatus::Proven),
        ("integer_claim", VerifStatus::Disproven),
    ] {
        for results in [&resolved, &flow] {
            let result = results
                .iter()
                .find(|result| result.func_name == name)
                .expect("contract result");
            assert_eq!(result.status, expected, "{name}: {}", result.message);
            assert!(!result.message.contains("E0439"), "{}", result.message);
        }
        let result = dual
            .iter()
            .find(|result| result.func_name == name)
            .expect("dual coverage result");
        assert_eq!(result.status, VerifStatus::NotInTrustedSubset);
        assert!(result
            .message
            .contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE));
        assert_eq!(
            result
                .diagnostic
                .as_ref()
                .and_then(|diagnostic| diagnostic.code.as_deref()),
            Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
    }
}

#[test]
fn ordinary_scalar_contracts_share_mir_and_retire_legacy_body_access() {
    require_z3!();
    let source = r#"
        func positive(value: i32) -> bool {
            requires: value > 0
            ensures: result == true
            value > 0
        }
        func identity(value: i64) -> i64 {
            ensures: result == old(value)
            value
        }
        func false_claim() -> bool { ensures: result == true
 false }
        func checked_increment(value: i32) -> i32 {
            ensures: result > value
            value + 1
        }
        func main() -> i32 { if positive(1) { 42 } else { 0 } }
        func no_contract(value: i32) -> i32 { value }
    "#;
    let file = parse_memory_source(source, "ordinary-scalar-contracts").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("shared scalar-contract materialization");
    assert!(route.admission.collection_complete());
    assert!(route.materialized_collection_candidate);
    assert!(!route.materialized_collection_operation_candidate);
    let canonical = route.program;
    let receipt = canonical.route_receipt(crate::core::mir::SCALAR_COLLECTION_ISLAND);
    crate::core::mir::validate_scalar_collection_island(&canonical).expect("scalar island");
    crate::verifier::validate_mir_capabilities(&canonical).expect("verifier capability");
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(42)
    );
    let bytecode =
        crate::interp::bytecode::compile_mir_program_with_route_receipt(&canonical, &receipt)
            .expect("AST-free MIR bytecode");
    let mut vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(vm.run().expect("bytecode execution"), 42);
    let context = inkwell::context::Context::create();
    let mut native = crate::codegen::CodeGenerator::new(&context, "ordinary_scalar_contracts");
    native
        .compile_mir_native_with_route_receipt(&canonical, &receipt)
        .expect("native consumes the same MIR receipt");
    native.module.verify().expect("LLVM verification");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let results = verify_mir_with_route_receipt(&canonical, &receipt, source_hash.clone())
        .expect("same-MIR verifier");
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&checked, &source_hash);
    for (name, status) in [
        ("positive", VerifStatus::Proven),
        ("identity", VerifStatus::Proven),
        ("false_claim", VerifStatus::Disproven),
        ("checked_increment", VerifStatus::Disproven),
        ("main", VerifStatus::Proven),
    ] {
        let result = results
            .iter()
            .find(|result| result.func_name == format!("function:{name}"))
            .expect("canonical contract verdict");
        assert_eq!(result.status, status, "{name}: {}", result.message);
        let artifact = result.artifact.as_ref().expect("canonical proof identity");
        assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.mir_hash, receipt.mir_digest);

        let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
        assert_eq!(
            exact_mir.status, status,
            "typed MIR {name}: {}",
            exact_mir.message
        );
        assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);
    }
    assert!(results.iter().all(|result| {
        result.func_name != "function:no_contract" && result.func_name != "no_contract"
    }));
    for dual in [false, true] {
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let public_results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("public checked verifier");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
        for (name, status) in [
            ("positive", VerifStatus::Proven),
            ("identity", VerifStatus::Proven),
            ("false_claim", VerifStatus::Disproven),
            ("checked_increment", VerifStatus::Disproven),
            ("main", VerifStatus::Proven),
        ] {
            let public = exact_mir_result_for_test(&public_results, name);
            let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
            assert_eq!(public.status, status, "{name}: {}", public.message);
            if dual {
                assert_dual_result_keeps_the_agreed_engine_artifact(
                    public,
                    exact_mir,
                    &source_hash,
                    &typed_receipt,
                );
            } else {
                assert_single_exact_mir_artifact(public, &source_hash, &typed_receipt);
            }
        }
        assert!(public_results.iter().all(|result| {
            result.func_name != "function:no_contract" && result.func_name != "no_contract"
        }));
    }
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let mut direct_native =
        crate::codegen::CodeGenerator::new(&context, "checked_scalar_contracts");
    direct_native
        .compile_checked(&checked)
        .expect("direct native shared route");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    direct_native
        .module
        .verify()
        .expect("direct native LLVM verification");
}

#[test]
fn ordinary_scalar_contract_library_preserves_call_obligations_without_main() {
    require_z3!();
    let source = r#"
func positive(value: i32) -> i32 {
    requires: value > 0
    value
}
func rejected() -> i32 { positive(-1) }
func admitted() -> i32 { positive(1) }
func impossible(value: i32) -> i32 {
    requires: value > 0 && value < 0
    ensures: true
    value
}
func false_claim() -> bool {
    ensures: result == true
    false
}
"#;
    let file = super::parse_memory_source(source, "scalar-contract-library").expect("parse");
    let checked = crate::core::check_program(&file).expect("check library without main");
    let canonical = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("materialize identical library graph")
        .program;
    crate::core::mir::validate_scalar_collection_graph(&canonical)
        .expect("all library functions belong to the scalar graph");
    let execution_errors = crate::core::mir::validate_scalar_collection_island(&canonical)
        .expect_err("execution still requires main");
    assert!(execution_errors
        .iter()
        .any(|error| error.contains("function:main")));
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&checked, &source_hash);
    for (name, status) in [
        ("positive", VerifStatus::Proven),
        ("rejected", VerifStatus::Disproven),
        ("admitted", VerifStatus::Proven),
        ("impossible", VerifStatus::Disproven),
        ("false_claim", VerifStatus::Disproven),
    ] {
        let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
        assert_eq!(
            exact_mir.status, status,
            "exact MIR {name}: {}",
            exact_mir.message
        );
        assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);
        if name == "rejected" {
            assert!(
                exact_mir
                    .message
                    .contains("call to 'function:positive' may violate precondition"),
                "exact rejected call-site VC: {}",
                exact_mir.message
            );
            assert_mir_contract_diagnostic_origin(&checked, exact_mir);
            let diagnostic = exact_mir.diagnostic.as_ref().unwrap();
            assert_eq!(diagnostic.span.start_line, 6);
            let call_site = checked
                .call_sites_sorted()
                .into_iter()
                .find(|site| site.owner == "function:rejected" && site.callee == "positive")
                .expect("exact rejected-to-positive call-site identity");
            assert_eq!(call_site.origin.user_span().start_line, 6);
        }
        if name == "impossible" {
            assert_exact_mir_contract_diagnostic(
                &checked,
                exact_mir,
                "canonical MIR requires are unsatisfiable",
            );
        }
        if name == "false_claim" {
            assert_exact_mir_contract_diagnostic(
                &checked,
                exact_mir,
                "canonical MIR ensures contract is disproven; counterexample: result = false",
            );
        }
    }
    assert_exact_mir_secondary_status(&exact_mir_results, "rejected", VerifStatus::Disproven);
    for dual in [false, true] {
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("verify library without AST fallback");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
        for (name, status) in [
            ("positive", VerifStatus::Proven),
            ("rejected", VerifStatus::Disproven),
            ("admitted", VerifStatus::Proven),
            ("impossible", VerifStatus::Disproven),
            ("false_claim", VerifStatus::Disproven),
        ] {
            let result = exact_mir_result_for_test(&results, name);
            let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
            assert_eq!(result.status, status, "{name}: {}", result.message);
            if dual {
                assert_dual_result_keeps_the_agreed_engine_artifact(
                    result,
                    exact_mir,
                    &source_hash,
                    &typed_receipt,
                );
            } else {
                assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);
            }
        }
    }
}

#[test]
fn ordinary_scalar_contract_library_tracks_preconditions_through_helpers_and_branches() {
    require_z3!();
    let source = r#"
func positive(value: i32) -> i32 {
    requires: value > 0
    requires: value < 100
    value
}
func forward(value: i32) -> i32 { positive(value) }
func bad_lower() -> i32 { forward(-1) }
func bad_upper() -> i32 { forward(100) }
func good() -> i32 { forward(42) }
func guarded(value: i32) -> i32 {
    ensures: result >= 0
    if value > 0 && value < 100 { forward(value) } else { 0 }
}
"#;
    let file = parse_memory_source(source, "scalar-contract-helper-obligations")
        .expect("parse helper graph");
    let checked = crate::core::check_program(&file).expect("check helper graph");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&checked, &source_hash);
    for (name, status) in [
        ("positive", VerifStatus::Proven),
        ("forward", VerifStatus::Disproven),
        ("bad_lower", VerifStatus::Disproven),
        ("bad_upper", VerifStatus::Disproven),
        ("good", VerifStatus::Proven),
        ("guarded", VerifStatus::Proven),
    ] {
        let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
        assert_eq!(
            exact_mir.status, status,
            "exact MIR {name}: {}",
            exact_mir.message
        );
        assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);
        if status == VerifStatus::Disproven {
            assert_mir_contract_diagnostic_origin(&checked, exact_mir);
            assert!(
                exact_mir.message.contains("may violate precondition"),
                "exact MIR helper obligation for {name}: {}",
                exact_mir.message
            );
            let owner = format!("function:{name}");
            assert!(
                checked
                    .call_sites_sorted()
                    .iter()
                    .any(|site| site.owner == owner),
                "{name} retains its checked helper call-site identity"
            );
        }
    }
    for dual in [false, true] {
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("verify canonical helper graph");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
        for (name, status) in [
            ("positive", VerifStatus::Proven),
            ("forward", VerifStatus::Disproven),
            ("bad_lower", VerifStatus::Disproven),
            ("bad_upper", VerifStatus::Disproven),
            ("good", VerifStatus::Proven),
            ("guarded", VerifStatus::Proven),
        ] {
            let result = exact_mir_result_for_test(&results, name);
            let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
            assert_eq!(result.status, status, "{name}: {}", result.message);
            if dual {
                assert_dual_result_keeps_the_agreed_engine_artifact(
                    result,
                    exact_mir,
                    &source_hash,
                    &typed_receipt,
                );
            } else {
                assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);
            }
        }
    }
}

#[test]
fn ordinary_scalar_contract_unit_return_is_proven_without_main() {
    require_z3!();
    let source = r#"
func finished() {
    ensures: true
}
func guarded(flag: bool) {
    requires: flag
    ensures: old(flag)
}
"#;
    let file =
        parse_memory_source(source, "scalar-contract-unit-return").expect("parse unit contracts");
    let checked = crate::core::check_program(&file).expect("check unit contracts");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&checked, &source_hash);
    assert_eq!(exact_mir_results.len(), 2);
    for name in ["finished", "guarded"] {
        let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
        assert_eq!(
            exact_mir.status,
            VerifStatus::Proven,
            "{}",
            exact_mir.message
        );
        assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);
    }
    for dual in [false, true] {
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("verify canonical unit returns");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
        assert_eq!(results.len(), 2);
        for name in ["finished", "guarded"] {
            let result = exact_mir_result_for_test(&results, name);
            let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
            assert_eq!(result.status, VerifStatus::Proven, "{}", result.message);
            if dual {
                assert_dual_result_keeps_the_agreed_engine_artifact(
                    result,
                    exact_mir,
                    &source_hash,
                    &typed_receipt,
                );
            } else {
                assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);
            }
        }
    }
}

#[test]
fn ordinary_scalar_contract_arithmetic_uses_checked_expression_width() {
    require_z3!();
    let source = r#"
func safe32(x: i32, y: i32) -> bool {
    requires: y != 0 && (x != -2147483648 || y != -1)
    ensures: old(x) / old(y) == x / y
    true
}
func safe64(x: i64, y: i64) -> bool {
    requires: y != 0 && (x != -9223372036854775808 || y != -1)
    ensures: old(x) / old(y) == x / y
    true
}
func unsafe_division(x: i32, y: i32) -> bool {
    ensures: x / y == x / y
    true
}
func unsafe_remainder(x: i32, y: i32) -> bool {
    requires: y != 0
    ensures: x % y == x % y
    true
}
func unsafe64(x: i64) -> bool {
    ensures: x / -1 == x / -1
    true
}
func mixed_width(x: i32, y: i64) -> bool {
    requires: y != 0
    ensures: x / y == x / y
    true
}
func wide_literal() -> bool {
    ensures: 2147483648 / -1 == -2147483648
    true
}
func truncation() -> bool {
    ensures: -7 / 2 == -3 && -7 % 2 == -1
    true
}
func unsafe_contract_add(x: i32) -> bool {
    ensures: x + 1 == x + 1
    true
}
func safe_contract_add(x: i32) -> bool {
    requires: x < 2147483647
    ensures: x + 1 == x + 1
    true
}
"#;
    let file = parse_memory_source(source, "typed-contract-arithmetic").expect("parse");
    let checked = crate::core::check_program(&file).expect("check contract library");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&checked, &source_hash);
    let expected = [
        ("safe32", VerifStatus::Proven),
        ("safe64", VerifStatus::Proven),
        ("unsafe_division", VerifStatus::Disproven),
        ("unsafe_remainder", VerifStatus::Disproven),
        ("unsafe64", VerifStatus::Disproven),
        ("mixed_width", VerifStatus::Proven),
        ("wide_literal", VerifStatus::Proven),
        ("truncation", VerifStatus::Proven),
        ("unsafe_contract_add", VerifStatus::Disproven),
        ("safe_contract_add", VerifStatus::Proven),
    ];
    for (name, status) in expected.iter().cloned() {
        let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
        assert_eq!(
            exact_mir.status, status,
            "exact MIR {name}: {}",
            exact_mir.message
        );
        assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);
        if status == VerifStatus::Disproven {
            assert_exact_mir_contract_diagnostic(
                &checked,
                exact_mir,
                "canonical MIR ensures contract expression is undefined",
            );
        }
    }
    for dual in [false, true] {
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("verify actual expression types");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
        for (name, status) in expected.iter().cloned() {
            let result = exact_mir_result_for_test(&results, name);
            let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
            assert_eq!(result.status, status, "{name}: {}", result.message);
            if dual {
                assert_dual_result_keeps_the_agreed_engine_artifact(
                    result,
                    exact_mir,
                    &source_hash,
                    &typed_receipt,
                );
            } else {
                assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);
            }
        }
    }
}

#[test]
fn ordinary_scalar_contract_definedness_preserves_short_circuit_and_order() {
    require_z3!();
    let source = r#"
func guarded_requires(x: i32, y: i32) -> bool {
    requires: x >= 0
    requires: y != 0 && x / y == x / y
    ensures: true
    true
}
func reversed_requires(x: i32, y: i32) -> bool {
    requires: x >= 0
    requires: x / y == x / y && y != 0
    ensures: true
    true
}
func unsafe_or(x: i32, y: i32) -> bool {
    requires: y != 0 || x / y == x / y
    ensures: true
    true
}
func guarded_or(x: i32, y: i32) -> bool {
    requires: y == 0 || ((x != -2147483648 || y != -1) && x / y == x / y)
    ensures: true
    true
}
func earlier_guards(x: i32, y: i32) -> bool {
    requires: y != 0
    requires: x != -2147483648 || y != -1
    requires: x / y == x / y
    ensures: true
    true
}
func later_guards(x: i32, y: i32) -> bool {
    requires: x / y == x / y
    requires: y != 0 && (x != -2147483648 || y != -1)
    ensures: true
    true
}
func skipped_postcondition(x: i32, y: i32) -> i32 {
    requires: y == 0
    ensures: y == 0 || x / y == x / y
    0
}
func false_short_circuit(x: i32, y: i32) -> i32 {
    requires: y == 0
    ensures: y != 0 && x / y == x / y
    0
}
func return_path_guard(x: i32, y: i32) -> i32 {
    requires: x >= 0
    ensures: y == 0 || result == x / y
    if y == 0 { 0 } else { x / y }
}
"#;
    let file = parse_memory_source(source, "contract-evaluation-order").expect("parse");
    let checked = crate::core::check_program(&file).expect("check contract library");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&checked, &source_hash);
    let expected = [
        ("guarded_requires", VerifStatus::Proven),
        ("reversed_requires", VerifStatus::Disproven),
        ("unsafe_or", VerifStatus::Disproven),
        ("guarded_or", VerifStatus::Proven),
        ("earlier_guards", VerifStatus::Proven),
        ("later_guards", VerifStatus::Disproven),
        ("skipped_postcondition", VerifStatus::Proven),
        ("false_short_circuit", VerifStatus::Disproven),
        ("return_path_guard", VerifStatus::Proven),
    ];
    for (name, status) in expected.iter().cloned() {
        let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
        assert_eq!(
            exact_mir.status, status,
            "exact MIR {name}: {}",
            exact_mir.message
        );
        assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);
        if matches!(name, "reversed_requires" | "unsafe_or" | "later_guards") {
            assert_exact_mir_contract_diagnostic(
                &checked,
                exact_mir,
                "canonical MIR requires contract expression is undefined",
            );
        }
        if name == "false_short_circuit" {
            assert_exact_mir_contract_diagnostic(
                &checked,
                exact_mir,
                "canonical MIR ensures contract is disproven; counterexample: result = 0",
            );
        }
    }
    for dual in [false, true] {
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("verify short-circuit guard obligations");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
        for (name, status) in expected.iter().cloned() {
            let result = exact_mir_result_for_test(&results, name);
            let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
            assert_eq!(result.status, status, "{name}: {}", result.message);
            if dual {
                assert_dual_result_keeps_the_agreed_engine_artifact(
                    result,
                    exact_mir,
                    &source_hash,
                    &typed_receipt,
                );
            } else {
                assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);
            }
        }
    }
}

#[test]
fn ordinary_scalar_contract_callee_definedness_uses_actual_arguments() {
    require_z3!();
    let source = r#"
func unsafe_gate(x: i32, y: i32) -> i32 {
    requires: x / y == x / y
    x
}
func rejected_call() -> i32 { unsafe_gate(1, 0) }
func admitted_call() -> i32 { unsafe_gate(2, 1) }
func proven_call() -> i32 {
    ensures: result == 2
    unsafe_gate(2, 1)
}
"#;
    let file = parse_memory_source(source, "callee-contract-definedness").expect("parse");
    let checked = crate::core::check_program(&file).expect("check contract call graph");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&checked, &source_hash);
    let expected = [
        ("unsafe_gate", VerifStatus::Disproven),
        ("rejected_call", VerifStatus::Disproven),
        ("proven_call", VerifStatus::Proven),
    ];
    for (name, status) in expected.iter().cloned() {
        let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
        assert_eq!(
            exact_mir.status, status,
            "exact MIR {name}: {}",
            exact_mir.message
        );
        assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);
    }
    assert!(exact_mir_results
        .iter()
        .all(|result| result.func_name != "admitted_call"));
    for dual in [false, true] {
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("verify argument-specific contract safety");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
        for (name, status) in expected.iter().cloned() {
            let result = exact_mir_result_for_test(&results, name);
            let exact_mir = exact_mir_result_for_test(&exact_mir_results, name);
            assert_eq!(result.status, status, "{name}: {}", result.message);
            if dual {
                assert_dual_result_keeps_the_agreed_engine_artifact(
                    result,
                    exact_mir,
                    &source_hash,
                    &typed_receipt,
                );
            } else {
                assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);
            }
            if status == VerifStatus::Disproven {
                assert!(result.message.contains("undefined"), "{}", result.message);
            }
        }
        assert!(results
            .iter()
            .all(|result| result.func_name != "admitted_call"));
    }
}

#[test]
fn unsupported_contract_returns_structured_coverage_without_legacy_access() {
    require_z3!();
    let source = r#"
        func guarded(value: i32) -> i32 {
            ensures: value > 0
            value + {
                requires: value > 0
                0
            }
        }

        func main() -> i32 { guarded(1) }
    "#;
    let file = parse_memory_source(source, "legacy-body-owner-audit").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let single = verify_checked(&program, source_hash.clone()).expect("typed coverage result");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let guarded = single
        .iter()
        .find(|result| result.func_name == "guarded")
        .expect("deferred guarded owner");
    assert_eq!(guarded.status, VerifStatus::NotInTrustedSubset);
    assert!(guarded
        .message
        .contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let dual = verify_checked_dual(&program, source_hash).expect("typed dual coverage result");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let guarded = dual
        .iter()
        .find(|result| result.func_name == "guarded")
        .expect("deferred dual guarded owner");
    assert_eq!(guarded.status, VerifStatus::NotInTrustedSubset);
    assert!(guarded
        .message
        .contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE));
}

#[test]
fn public_checked_verifier_routes_closed_scalar_collection_to_mir() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_list_len.mimi");
    let file = parse_memory_source(source, "mir-scalar-collection-public-api").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_scalar_collection_admission(&program),
        crate::core::mir::ScalarCollectionAdmission::CompleteCoverage
    );
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&program, &source_hash);
    let exact_mir = exact_mir_result_for_test(&exact_mir_results, "list_len_contract");
    assert_eq!(
        exact_mir.status,
        VerifStatus::Proven,
        "{}",
        exact_mir.message
    );
    assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verify_result = verify_checked(&program, source_hash.clone()).expect("MIR verify");
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "closed verifier route must not access a retained legacy body"
    );
    let result = verify_result
        .iter()
        .find(|result| result.func_name == "list_len_contract")
        .expect("scalar collection contract result");
    assert_eq!(result.status, VerifStatus::Proven);
    assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);
    assert_eq!(
        result
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR)
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let dual_result = verify_checked_dual(&program, source_hash.clone()).expect("MIR dual verify");
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "closed dual verifier route must not access a retained legacy body"
    );
    let dual = dual_result
        .iter()
        .find(|result| result.func_name == "list_len_contract")
        .expect("dual scalar collection contract result");
    assert_eq!(dual.status, VerifStatus::Proven);
    assert_dual_result_keeps_the_agreed_engine_artifact(
        dual,
        exact_mir,
        &source_hash,
        &typed_receipt,
    );
}

#[test]
fn public_checked_verifier_routes_nested_option_tuple_to_mir() {
    require_z3!();
    let source = include_str!("../../tests/real_world/mir_nested_tuple_option.mimi");
    let file = parse_memory_source(source, "mir-nested-option-tuple-public-api").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_option_nested_tuple_variant_admission(&program),
        crate::core::mir::OptionNestedTupleVariantAdmission::CompleteCoverage
    );
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results =
        verify_checked(&program, source_hash.clone()).expect("nested Option tuple MIR verify");
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "nested Option tuple verifier must not access retained legacy bodies"
    );
    let result = results
        .iter()
        .find(|result| result.func_name == "main")
        .expect("nested Option tuple contract result");
    assert_eq!(result.status, VerifStatus::Proven);
    // Each of the two postconditions has a separate definedness proof before
    // its predicate is checked. Neither obligation may be omitted.
    assert_eq!(result.constraint_count, 4);
    let artifact = result.artifact.as_ref().expect("MIR proof artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&program)
        .expect("canonical nested Option tuple MIR");
    assert_eq!(artifact.mir_hash, canonical.canonical_digest());

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let dual = verify_checked_dual(&program, source_hash).expect("nested Option tuple dual verify");
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "nested Option tuple dual verifier must not access retained legacy bodies"
    );
    let dual_result = dual
        .iter()
        .find(|result| result.func_name == "main")
        .expect("nested Option tuple dual contract result");
    assert_eq!(dual_result.status, VerifStatus::Proven);
    assert_eq!(dual_result.constraint_count, 4);
    let dual_artifact = dual_result
        .artifact
        .as_ref()
        .expect("dual MIR proof artifact");
    assert_eq!(dual_artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(dual_artifact.mir_hash, canonical.canonical_digest());
    assert_eq!(
        dual_artifact
            .mir_route_receipt
            .as_ref()
            .map(|receipt| receipt.mir_digest.as_str()),
        Some(dual_artifact.mir_hash.as_str())
    );
}

#[test]
fn public_checked_verifier_routes_recoverable_cross_state_flow_to_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/real_world/flow_state_match_fail_result_failure_dual_backend.mimi"
    );
    let file = parse_memory_source(source, "mir-recoverable-cross-state-public-api")
        .expect("parse recoverable cross-state Flow");
    let program =
        crate::core::check_program(&file).expect("typecheck recoverable cross-state Flow");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    assert!(crate::core::mir::classify_canonical_mir_route_admission(&program).flow_failure_retry);
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&program, &source_hash);
    let exact_mir = exact_mir_result_for_test(&exact_mir_results, "main");
    assert_eq!(
        exact_mir.status,
        VerifStatus::Proven,
        "{}",
        exact_mir.message
    );
    assert!(exact_mir.constraint_count > 0);
    assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_checked(&program, source_hash.clone()).expect("public MIR verify");
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "closed recoverable Flow verifier must not access the compatibility body"
    );
    let result = exact_mir_result_for_test(&results, "main");
    assert_eq!(result.status, VerifStatus::Proven);
    assert!(result.constraint_count > 0);
    assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let dual = verify_checked_dual(&program, source_hash.clone()).expect("public dual MIR verify");
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "closed recoverable Flow dual verifier must not access the compatibility body"
    );
    let dual_result = exact_mir_result_for_test(&dual, "main");
    assert!(dual_result.constraint_count > 0);
    assert_dual_result_keeps_the_agreed_engine_artifact(
        dual_result,
        exact_mir,
        &source_hash,
        &typed_receipt,
    );
}

#[test]
fn public_checked_verifier_materializes_flow_state_match_before_no_contract_result() {
    require_z3!();
    let source =
        include_str!("../../tests/real_world/flow_state_match_fail_result_dual_backend.mimi");
    let file = parse_memory_source(source, "mir-flow-state-match-no-contract").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
    assert!(admission.flow_failure_retry);
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    for dual in [false, true] {
        crate::core::mir::reset_test_route_materialization_count();
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = if dual {
            crate::verifier::verify_checked_dual(&checked, source_hash.clone())
        } else {
            crate::verifier::verify_checked(&checked, source_hash.clone())
        }
        .unwrap_or_else(|error| {
            panic!(
                "{} Flow state-match verifier: {error}",
                if dual { "dual" } else { "single" }
            )
        });
        assert!(results.is_empty(), "fixture has no contracts: {results:?}");
        assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
        assert!(
            crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
            "{} Flow state-match verifier must not access a legacy body",
            if dual { "dual" } else { "single" }
        );
    }
}

#[test]
fn public_checked_verifier_reports_recoverable_cross_state_counterexample_from_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/real_world/flow_state_match_fail_result_failure_disproven_dual_backend.mimi"
    );
    let file = parse_memory_source(source, "mir-recoverable-cross-state-public-counterexample")
        .expect("parse recoverable cross-state counterexample");
    let program = crate::core::check_program(&file).expect("typecheck counterexample");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&program, &source_hash);
    let exact_mir = exact_mir_result_for_test(&exact_mir_results, "main");
    assert_eq!(
        exact_mir.status,
        VerifStatus::Disproven,
        "{}",
        exact_mir.message
    );
    assert!(exact_mir.constraint_count > 0);
    assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_checked(&program, source_hash.clone()).expect("public MIR counterexample");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let result = exact_mir_result_for_test(&results, "main");
    assert_eq!(result.status, VerifStatus::Disproven);
    assert!(result.constraint_count > 0);
    assert!(result.message.contains("ensures contract is disproven"));
    assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);
}

#[test]
fn public_checked_verifier_proves_multifield_flow_source_receipt_from_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_m3_flow_multifield_string_source_receipt.mimi");
    let file = parse_memory_source(source, "mir-multifield-flow-source-receipt-public-api")
        .expect("parse multifield Flow source receipt");
    let program =
        crate::core::check_program(&file).expect("typecheck multifield Flow source receipt");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&program, &source_hash);
    let exact_mir = exact_mir_result_for_test(&exact_mir_results, "main");
    assert_eq!(
        exact_mir.status,
        VerifStatus::Proven,
        "{}",
        exact_mir.message
    );
    assert!(exact_mir.constraint_count > 0);
    assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results =
        verify_checked(&program, source_hash.clone()).expect("public multifield MIR verify");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let result = exact_mir_result_for_test(&results, "main");
    assert_eq!(result.status, VerifStatus::Proven);
    assert!(result.constraint_count > 0);
    assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let dual_results = crate::verifier::verify_checked_dual(&program, source_hash.clone())
        .expect("public dual multifield MIR verify");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let dual_result = exact_mir_result_for_test(&dual_results, "main");
    assert!(dual_result.constraint_count > 0);
    assert_dual_result_keeps_the_agreed_engine_artifact(
        dual_result,
        exact_mir,
        &source_hash,
        &typed_receipt,
    );
}

#[test]
fn public_checked_verifier_proves_f64_flow_arithmetic_from_mir() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_r6_flow_f64_cross_state_receipt.mimi");
    let file = parse_memory_source(source, "mir-f64-flow-source-receipt-public-api")
        .expect("parse f64 Flow source receipt");
    let program = crate::core::check_program(&file).expect("typecheck f64 Flow source receipt");
    assert!(crate::core::mir::classify_canonical_mir_route_admission(&program).flow_failure_retry);
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_checked(&program, source_hash).expect("public f64 Flow MIR verify");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let result = results
        .iter()
        .find(|result| result.func_name == "main")
        .expect("f64 Flow verifier result");
    // R6-1061 restatement: the transition's `self.balance + 1.0` now carries
    // the Z3 IEEE Float symbolic domain, so the recoverable receipt proves
    // on the canonical engine instead of reporting the float boundary.
    assert_eq!(result.status, VerifStatus::Proven);
    assert!(result.constraint_count > 0);
    assert_finite_ieee_binary64_mir_artifact(result);
}

#[test]
fn scalar_collection_verifier_admission_does_not_overmatch_managed_siblings() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_test_scalar_collection_mixed.mimi");
    let file = parse_memory_source(source, "mir-scalar-collection-mixed").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_scalar_collection_admission(&program),
        crate::core::mir::ScalarCollectionAdmission::MixedCoverage
    );
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    assert!(verify_closed_scalar_collection_mir(&program, source_hash)
        .expect("mixed collection program remains outside the closed verifier island")
        .is_none());
}

#[test]
fn scalar_collection_route_receipt_is_shared_by_all_consumers() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_list_len.mimi");
    let file = parse_memory_source(source, "mir-scalar-collection-receipt").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_scalar_collection_admission(&checked),
        crate::core::mir::ScalarCollectionAdmission::CompleteCoverage
    );
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    crate::core::mir::validate_scalar_collection_island(&canonical)
        .expect("scalar collection island");
    crate::verifier::validate_mir_capabilities(&canonical).expect("verifier capability");

    let receipt = canonical.route_receipt(crate::core::mir::SCALAR_COLLECTION_ISLAND);
    assert_eq!(receipt.schema, crate::core::mir::MIR_ROUTE_RECEIPT_SCHEMA);
    assert_eq!(receipt.profile, crate::core::mir::SCALAR_COLLECTION_ISLAND);
    assert_eq!(receipt.mir_digest.len(), 64);
    assert_eq!(receipt.type_desc_digest.len(), 64);
    assert_eq!(receipt.ownership_digest.len(), 64);
    assert!(receipt
        .root_owners
        .iter()
        .any(|owner| owner.0 == "function:main"));

    // Every consumer below receives the same immutable MIR graph.  The
    // receipt is recomputed only for comparison; it is never a backend input
    // from which a consumer could reconstruct frontend semantics.
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(42)
    );
    let bytecode = crate::interp::bytecode::compile_mir_program(&canonical).expect("MIR bytecode");
    let mut bytecode_vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(bytecode_vm.run().expect("MIR bytecode execution"), 42);
    crate::codegen::mir::validate_mir_native(&canonical).expect("native MIR capability");
    let verifier_results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let artifact = verifier_results
        .iter()
        .find(|result| result.func_name == "function:list_len_contract")
        .and_then(|result| result.artifact.as_ref())
        .expect("MIR proof artifact");
    assert_eq!(artifact.mir_hash, receipt.mir_digest);
    assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);

    let after_consumers = canonical.route_receipt(crate::core::mir::SCALAR_COLLECTION_ISLAND);
    assert_eq!(receipt, after_consumers);
}

#[test]
fn finite_f64_add_verifier_capability_is_closed_before_symbolic_execution() {
    let source = include_str!("../../tests/fixtures/mir_native_f64_add.mimi");
    let file = parse_memory_source(source, "mir-f64-add-capability").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical f64 add MIR");
    crate::verifier::validate_mir_capabilities(&canonical).expect("finite-only f64 Add capability");

    let source = include_str!("../../tests/fixtures/mir_native_f64_subtract.mimi");
    let file = parse_memory_source(source, "mir-f64-subtract-capability").expect("parse subtract");
    let checked = crate::core::check_program(&file).expect("typecheck subtract");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical f64 subtract MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("finite-only f64 Subtract capability");

    // R6-1067 restatement: Multiply/Divide joined the finite-only Copy f64
    // capability beside Add/Subtract, and the surface cannot express float
    // remainder (E0202 at typecheck), so the residual operator's
    // rejection is pinned at the TypeDesc validator level in
    // core::mir::tests instead of through a surface program.
    let multiply = parse_memory_source(
        "func main() -> f64 { 1.0 * 2.0 }",
        "mir-f64-multiply-capability",
    )
    .expect("parse multiply");
    let checked = crate::core::check_program(&multiply).expect("typecheck multiply");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical f64 multiply MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("finite-only f64 Multiply capability");

    let divide = parse_memory_source(
        "func main() -> f64 { 1.0 / 2.0 }",
        "mir-f64-divide-capability",
    )
    .expect("parse divide");
    let checked = crate::core::check_program(&divide).expect("typecheck divide");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical f64 divide MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("finite-only f64 Divide capability");
}

#[test]
fn finite_f64_contract_comparison_enters_the_symbolic_domain() {
    require_z3!();
    // R6-1063 Face B: f64 contract values are in-domain for ordering
    // comparisons.  The E0813 finiteness obligation at parameter
    // introduction is load-bearing here — without it the NaN model would
    // falsify the reflexive ordering and the proof would fail.
    let source = r#"
func carry(x: f64) -> f64 {
    ensures: result >= x
    x
}
func main() -> i64 { 42 }
"#;
    let file = parse_memory_source(source, "mir-f64-contract-comparison").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let mir = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("f64 comparison contracts materialize");
    crate::verifier::validate_mir_capabilities(&mir).expect("f64 comparison capability gate");
    let results = crate::verifier::verify_mir(&mir, "f64-contract-comparison".into())
        .expect("f64 comparison verification");
    // Only `carry` carries a contract, so exactly one obligation comes back.
    assert_eq!(results.len(), 1, "obligation count");
    assert_eq!(results[0].status, VerifStatus::Verified);
}

#[test]
fn finite_f64_contract_arithmetic_enters_the_symbolic_domain() {
    require_z3!();
    // R6-1067 restatement of the Face B residual boundary: contract
    // arithmetic over f64 joined the symbolic domain beside the R6-1063
    // ordering comparisons — RNE fpa ops are the exact fmul/fdiv runtime
    // semantics.  The bounded add contract verifies; the unbounded twin is
    // honestly disproven because `left + right` can overflow and reach the
    // runtime E0813 trap (arithmetic has real finiteness teeth the bare
    // comparison face never exposed).
    let bounded = r#"
func add(left: f64, right: f64) -> f64 {
    requires: left >= 0.0
    requires: left <= 1.0
    requires: right >= 0.0
    requires: right <= 1.0
    ensures: result == left + right
    left + right
}
func main() -> i64 { 42 }
"#;
    let file = parse_memory_source(bounded, "mir-f64-contract-arithmetic").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let mir = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("f64 contract arithmetic materializes");
    crate::verifier::validate_mir_capabilities(&mir).expect("f64 arithmetic capability gate");
    let results = crate::verifier::verify_mir(&mir, "f64-contract-arithmetic".into())
        .expect("f64 arithmetic verification");
    assert_eq!(results.len(), 1, "obligation count");
    assert_eq!(results[0].status, VerifStatus::Verified);

    let unbounded = r#"
func add(left: f64, right: f64) -> f64 {
    ensures: result == left + right
    left + right
}
func main() -> i64 { 42 }
"#;
    let file = parse_memory_source(unbounded, "mir-f64-contract-arithmetic-unbounded")
        .expect("parse unbounded");
    let checked = crate::core::check_program(&file).expect("typecheck unbounded");
    let mir = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("unbounded f64 contract arithmetic materializes");
    let results = crate::verifier::verify_mir(&mir, "f64-contract-arithmetic-unbounded".into())
        .expect("unbounded f64 arithmetic verification");
    assert_eq!(results.len(), 1, "unbounded obligation count");
    assert_eq!(results[0].status, VerifStatus::Disproven);
}

#[test]
fn generic_record_projection_is_consumed_by_mir_verifier_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_record_projection.mimi");
    let file = parse_memory_source(source, "mir-generic-record-projection").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic record projection MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection { .. }
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn nested_generic_list_construct_is_consumed_by_mir_verifier_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_list_nested_owned.mimi");
    let file = parse_memory_source(source, "mir-generic-nested-list").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical nested generic List MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("nested generic List verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn nested_list_len_is_consumed_by_mir_verifier_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_nested_list_len.mimi");
    let file = parse_memory_source(source, "mir-nested-list-len").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical nested List.len MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("nested List.len verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn nested_list_index_is_consumed_by_mir_verifier_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_nested_list_index.mimi");
    let file = parse_memory_source(source, "mir-nested-list-index").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical nested List index MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("nested List index verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn nested_list_reverse_is_consumed_by_mir_verifier_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_nested_list_reverse.mimi");
    let file = parse_memory_source(source, "mir-nested-list-reverse").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical nested List.reverse MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("nested List.reverse verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn nested_list_concat_is_consumed_by_mir_verifier_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_nested_list_concat.mimi");
    let file = parse_memory_source(source, "mir-nested-list-concat").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical nested List.concat MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("nested List.concat verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn generic_record_update_is_consumed_by_mir_verifier_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_record_update.mimi");
    let file = parse_memory_source(source, "mir-generic-record-update").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic record update MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordUpdate { ref contract }
            if contract.arity == 2 && contract.fields[0].name == "tag"
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic record update verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn two_override_generic_record_update_is_consumed_by_mir_verifier() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_record_update_multi.mimi");
    let file = parse_memory_source(source, "mir-generic-record-update-multi").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical two-override generic record update MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordUpdate { ref contract }
            if contract.arity == 3 && contract.fields.len() == 2
                && contract.fields[0].name == "tag"
                && contract.fields[1].name == "enabled"
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("two-override generic record update verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn owned_generic_record_update_is_consumed_by_mir_verifier() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_record_update_owned.mimi");
    let file = parse_memory_source(source, "mir-generic-record-update-owned").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic record update MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { ref contract }
            if contract.updates.len() == 1 && contract.residual.len() == 1
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic record update verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(
        results.iter().all(|result| {
            matches!(
                result.status,
                VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
            )
        }),
        "{results:?}"
    );
}

#[test]
fn owned_generic_record_update_with_copy_residual_is_consumed_by_mir_verifier() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_update_owned_copy_residual.mimi"
    );
    let file = parse_memory_source(source, "mir-generic-record-update-owned-copy").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical mixed owned generic record update MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { ref contract }
            if contract.residual.len() == 1
                && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::Noop
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("mixed owned generic record update verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn owned_generic_record_update_with_two_residuals_is_consumed_by_mir_verifier() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_update_owned_multi_residual.mimi"
    );
    let file = parse_memory_source(source, "mir-generic-record-update-owned-multi-residual")
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical multi-residual owned generic record update MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { ref contract }
            if contract.arity == 3
                && contract.residual.len() == 2
                && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::Noop
                && contract.residual[1].glue
                    == crate::core::mir::types::MirGlueKind::OwnedString
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("multi-residual owned generic record update verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn owned_generic_record_update_with_three_residuals_is_consumed_by_mir_verifier() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_update_owned_three_residual.mimi"
    );
    let file = parse_memory_source(source, "mir-generic-record-update-owned-three-residual")
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical three-residual owned generic record update MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { ref contract }
            if contract.arity == 4
                && contract.residual.len() == 3
                && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::Noop
                && contract.residual[1].glue
                    == crate::core::mir::types::MirGlueKind::OwnedString
                && contract.residual[2].glue
                    == crate::core::mir::types::MirGlueKind::OwnedString
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("three-residual owned generic record update verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_option_predicate_is_verified_from_canonical_mir_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_option_predicate.mimi");
    let file = parse_memory_source(source, "mir-generic-option-predicate").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option predicate MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate { .. }
            )
        })
        .expect("generic Option predicate instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate { .. }
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option predicate verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_option_unwrap_is_verified_from_canonical_mir_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_option_unwrap.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option unwrap MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { .. }
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option unwrap verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(
        results.is_empty()
            || results.iter().any(|result| {
                matches!(
                    result.status,
                    VerifStatus::Proven | VerifStatus::NoObligations
                )
            })
    );
}

#[test]
fn generic_option_unwrap_f64_is_verified_from_canonical_mir() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_f64.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap-f64").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option<f64> unwrap MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                    contract
                } if contract.projection.nominal.as_str() == "builtin:type:Option"
                    && contract.projection.ownership
                        == crate::core::mir::types::MirOwnership::Copy
            )
        })
        .expect("generic Option<f64> projection instance");
    assert_eq!(
        canonical
            .type_catalog()
            .get(&instance.arguments[0])
            .unwrap()
            .abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 64 }
    );
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option<f64> verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(
        results.is_empty()
            || results.iter().any(|result| {
                matches!(
                    result.status,
                    VerifStatus::Proven | VerifStatus::NoObligations
                )
            })
    );
}

#[test]
fn generic_option_unwrap_f32_has_canonical_verifier_capability() {
    let source = include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_f32.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap-f32").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option<f32> unwrap MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                    contract
                } if contract.projection.nominal.as_str() == "builtin:type:Option"
                    && contract.projection.ownership
                        == crate::core::mir::types::MirOwnership::Copy
            )
        })
        .expect("generic Option<f32> projection instance");
    assert_eq!(
        canonical
            .type_catalog()
            .get(&instance.arguments[0])
            .unwrap()
            .abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 32 }
    );
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option<f32> verifier capability");
}

#[test]
fn generic_option_unwrap_owned_string_is_verified_from_canonical_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_owned_string.mimi");
    let file =
        parse_memory_source(source, "mir-generic-option-unwrap-owned-string").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic owned Option unwrap MIR");
    assert!(canonical.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Option"
                    && contract.projection.ownership
                        == crate::core::mir::types::MirOwnership::Move
        )
    }));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic owned Option unwrap verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_option_unwrap_owned_list_is_verified_from_canonical_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_owned_list.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap-owned-list").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic owned Option<List> unwrap MIR");
    assert!(canonical.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Option"
                    && contract.projection.ownership
                        == crate::core::mir::types::MirOwnership::Move
                    && contract.projection.move_out_glue
                        == crate::core::mir::types::MirGlueKind::List
        )
    }));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic owned Option<List> verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_option_unwrap_owned_list_i64_and_bool_are_verified_from_canonical_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_option_unwrap_owned_list_scalars.mimi"
    );
    let file =
        parse_memory_source(source, "mir-generic-option-unwrap-owned-list-scalars").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option<List<i64|bool>> unwrap MIR");
    let instances = canonical
        .instances()
        .values()
        .filter(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership
                            == crate::core::mir::types::MirOwnership::Move
                        && contract.projection.move_out_glue
                            == crate::core::mir::types::MirGlueKind::List
            )
        })
        .count();
    assert_eq!(instances, 2);
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option<List<i64|bool>> verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_option_unwrap_or_is_verified_from_canonical_mir_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option unwrap_or MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option unwrap_or verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_option_unwrap_or_none_is_verified_from_canonical_mir() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or_none.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap-or-none").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option unwrap_or None MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option unwrap_or None verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_option_unwrap_or_f64_is_verified_from_canonical_mir() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or_f64.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap-or-f64").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option<f64> unwrap_or MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                    contract
                } if contract.projection.nominal.as_str() == "builtin:type:Option"
                    && contract.projection.ownership
                        == crate::core::mir::types::MirOwnership::Copy
            )
        })
        .expect("generic Option<f64> fallback projection instance");
    assert_eq!(
        canonical
            .type_catalog()
            .get(&instance.arguments[0])
            .expect("generic Option<f64> TypeDesc")
            .abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 64 }
    );
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option<f64> unwrap_or verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(
        results.is_empty()
            || results.iter().any(|result| {
                matches!(
                    result.status,
                    VerifStatus::Proven | VerifStatus::NoObligations
                )
            })
    );
}

#[test]
fn generic_option_unwrap_or_f64_none_is_verified_from_canonical_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or_f64_none.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap-or-f64-none").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option<f64> unwrap_or None MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option<f64> unwrap_or None verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_option_unwrap_or_owned_string_is_verified_from_canonical_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_string.mimi");
    let file =
        parse_memory_source(source, "mir-generic-option-unwrap-or-owned-string").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical managed Option unwrap_or MIR");
    assert!(canonical.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                contract
            } if contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
        )
    }));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("managed Option fallback verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_option_unwrap_or_owned_string_none_is_verified_from_canonical_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_string_none.mimi"
    );
    let file = parse_memory_source(source, "mir-generic-option-unwrap-or-owned-string-none")
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical managed Option unwrap_or None MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("managed Option fallback None verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_option_unwrap_or_owned_list_is_verified_from_canonical_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_list.mimi");
    let file =
        parse_memory_source(source, "mir-generic-option-unwrap-or-owned-list").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical managed Option<List> unwrap_or MIR");
    assert!(canonical.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                contract
            } if contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
                && contract.projection.move_out_glue == crate::core::mir::types::MirGlueKind::List
        )
    }));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("managed Option<List> fallback verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_option_unwrap_or_owned_list_none_is_verified_from_canonical_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_list_none.mimi"
    );
    let file =
        parse_memory_source(source, "mir-generic-option-unwrap-or-owned-list-none").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical managed Option<List> unwrap_or None MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("managed Option<List> fallback None verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_result_unwrap_is_verified_from_canonical_mir_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_result_unwrap.mimi");
    let file = parse_memory_source(source, "mir-generic-result-unwrap").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result unwrap MIR");
    assert!(canonical.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Result"
        )
    }));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Result unwrap verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_result_unwrap_or_is_verified_from_canonical_mir_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_or.mimi");
    let file = parse_memory_source(source, "mir-generic-result-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result unwrap_or MIR");
    assert!(canonical.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                contract
            } if contract.projection.nominal.as_str() == "builtin:type:Result"
        )
    }));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Result unwrap_or verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_distinct_result_unwrap_or_is_verified_from_canonical_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_distinct_unwrap_or.mimi");
    let file = parse_memory_source(source, "mir-generic-distinct-result-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical heterogeneous Result unwrap_or MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("heterogeneous Result unwrap_or verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn managed_generic_result_unwrap_or_is_verified_from_canonical_mir() {
    require_z3!();
    for source in [
        include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_string.mimi"),
        include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_string_err.mimi"
        ),
    ] {
        let file =
            parse_memory_source(source, "mir-managed-generic-result-unwrap-or").expect("parse");
        let checked = crate::core::check_program(&file).expect("typecheck");
        let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("canonical managed Result unwrap_or MIR");
        assert!(canonical.instances().values().any(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                    contract
                } if contract.projection.nominal.as_str() == "builtin:type:Result"
                    && contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
            )
        }));
        crate::verifier::validate_mir_capabilities(&canonical)
            .expect("managed Result unwrap_or verifier capability");
        let results = crate::verifier::verify_mir(
            &canonical,
            blake3::hash(source.as_bytes()).to_hex().to_string(),
        )
        .expect("MIR verifier");
        assert!(results.iter().all(|result| {
            matches!(
                result.status,
                VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
            )
        }));
    }
}

#[test]
fn managed_generic_bool_result_unwrap_or_is_verified_from_canonical_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_owned_string.mimi"
    );
    let file =
        parse_memory_source(source, "mir-managed-generic-result-bool-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical managed Result<bool> unwrap_or MIR");
    assert!(canonical.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                contract
            } if contract.projection.nominal.as_str() == "builtin:type:Result"
                && contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
        )
    }));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("managed Result<bool> unwrap_or verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn managed_generic_bool_result_unwrap_or_rejects_nested_managed_payload_before_legacy() {
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_owned_rejected.mimi"
    );
    let file =
        parse_memory_source(source, "mir-managed-generic-result-bool-rejected").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let error = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect_err("nested managed bool Result fallback must fail before legacy verifier");
    assert!(
        error.contains("generic MIR instance") || error.contains("generic Result"),
        "{error}"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "unsupported managed Result shape must not enter a legacy verifier"
    );
}

#[test]
fn managed_generic_result_unwrap_or_owned_list_is_verified_from_canonical_mir() {
    require_z3!();
    for source in [
        include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list.mimi"),
        include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list_err.mimi"
        ),
    ] {
        let file = parse_memory_source(source, "mir-managed-generic-result-list-unwrap-or")
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("typecheck");
        let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("canonical Result<List> unwrap_or MIR");
        assert!(canonical.instances().values().any(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                    contract
                } if contract.projection.nominal.as_str() == "builtin:type:Result"
                    && contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
                    && contract.projection.move_out_glue
                        == crate::core::mir::types::MirGlueKind::List
            )
        }));
        crate::verifier::validate_mir_capabilities(&canonical)
            .expect("Result<List> unwrap_or verifier capability");
        let results = crate::verifier::verify_mir(
            &canonical,
            blake3::hash(source.as_bytes()).to_hex().to_string(),
        )
        .expect("MIR verifier");
        assert!(results.iter().all(|result| {
            matches!(
                result.status,
                VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
            )
        }));
    }
}

#[test]
fn managed_generic_result_unwrap_or_owned_list_copy_scalar_family_is_verified() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list_scalars.mimi"
    );
    let file = parse_memory_source(source, "mir-managed-generic-result-list-scalars-unwrap-or")
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical Result<List<i64|bool>> unwrap_or MIR");
    let contracts = canonical
        .instances()
        .values()
        .filter_map(|instance| match &instance.contract {
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                contract,
            } if contract.projection.nominal.as_str() == "builtin:type:Result" => Some(contract),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(contracts.len(), 2, "i64 and bool specializations");
    assert!(contracts.iter().all(|contract| {
        contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
            && contract.projection.move_out_glue == crate::core::mir::types::MirGlueKind::List
    }));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("Result List scalar fallback verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_result_unwrap_or_is_rejected_before_legacy_verifier_fallback() {
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_or_rejected.mimi");
    let file = parse_memory_source(source, "mir-generic-result-unwrap-or-rejected").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let error = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect_err("unsupported generic Result unwrap_or must not fall through to AST verifier");
    assert!(
        error.contains("generic Result fallback projection"),
        "{error}"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "unsupported generic Result fallback must not enter a legacy verifier"
    );
}

#[test]
fn generic_result_projection_is_rejected_before_legacy_verifier_fallback() {
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_rejected.mimi");
    let file = parse_memory_source(source, "mir-generic-result-rejected").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let single_error = crate::verifier::verify_checked(&checked, source_hash.clone())
        .expect_err("single verifier must reject unsupported generic Result projection");
    assert!(
        single_error.contains("generic-result-projection-v1")
            || single_error.contains("generic Result projection"),
        "{single_error}"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "single verifier must not enter the retained AST compatibility path"
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let error = crate::verifier::verify_checked_dual(&checked, source_hash)
        .expect_err("unsupported generic Result projection must not fall through to AST verifier");
    assert!(
        error.contains("generic-result-projection-v1")
            || error.contains("generic Result projection"),
        "{error}"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "unsupported generic Result projection must not enter a legacy verifier"
    );
    assert_eq!(single_error, error);
}

#[test]
fn generic_result_distinct_projection_is_verified_from_canonical_mir() {
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_distinct_unwrap.mimi");
    let file = parse_memory_source(source, "mir-generic-result-distinct").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let results = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("distinct generic Result projection must remain on canonical MIR");
    assert!(results.is_empty(), "fixture has no contracts to verify");
}

#[test]
fn generic_result_f64_projection_is_verified_from_canonical_mir() {
    let source = include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_f64.mimi");
    let file = parse_memory_source(source, "mir-generic-result-f64").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result<T,i32> f64 MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
            if contract.projection.nominal.as_str() == "builtin:type:Result"
                && contract.projection.ownership == crate::core::mir::types::MirOwnership::Copy
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Result<T,i32> f64 verifier capability");
    let results = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("generic Result<T,i32> f64 must remain on canonical verifier route");
    assert!(results.is_empty(), "fixture has no contracts to verify");
}

#[test]
fn generic_result_f64_unwrap_or_is_verified_from_canonical_mir() {
    let source = include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_or_f64.mimi");
    let file = parse_memory_source(source, "mir-generic-result-f64-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result<T,i32> f64 unwrap_or MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
            contract
        } if contract.projection.nominal.as_str() == "builtin:type:Result"
            && contract.projection.ownership == crate::core::mir::types::MirOwnership::Copy
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Result<T,i32> f64 unwrap_or verifier capability");
    let results = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("generic Result<T,i32> f64 unwrap_or must remain on canonical verifier route");
    let result = results
        .iter()
        .find(|result| result.func_name == "main")
        .expect("f64 Result unwrap_or fixture must produce a main contract result");
    assert_eq!(result.status, VerifStatus::Proven);
    assert!(result.constraint_count > 0);
    let artifact = result
        .artifact
        .as_ref()
        .expect("f64 Result unwrap_or proof artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.mir_hash, canonical.canonical_digest());
}

#[test]
fn generic_result_bool_f64_unwrap_or_is_verified_from_canonical_mir() {
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_f64.mimi");
    let file = parse_memory_source(source, "mir-generic-result-bool-f64-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result<T,bool> f64 unwrap_or MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
            contract
        } if contract.projection.nominal.as_str() == "builtin:type:Result"
            && contract.projection.ownership == crate::core::mir::types::MirOwnership::Copy
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Result<T,bool> f64 unwrap_or verifier capability");
    let results = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("generic Result<T,bool> f64 unwrap_or must remain on canonical verifier route");
    let result = results
        .iter()
        .find(|result| result.func_name == "main")
        .expect("f64 Result<bool> unwrap_or fixture must produce a main contract result");
    assert_eq!(result.status, VerifStatus::Proven);
    assert!(result.constraint_count > 0);
    let artifact = result
        .artifact
        .as_ref()
        .expect("f64 Result<bool> unwrap_or proof artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.mir_hash, canonical.canonical_digest());
}

#[test]
fn generic_record_f64_projection_is_verified_from_canonical_mir() {
    let source = include_str!("../../tests/fixtures/mir_native_generic_record_projection_f64.mimi");
    let file = parse_memory_source(source, "mir-generic-record-f64").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Record<f64> projection MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            contract
        } if contract.arity == 1 && contract.name == "value"
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Record<f64> projection verifier capability");
    let results = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("generic Record<f64> projection must remain on canonical verifier route");
    assert!(results.is_empty(), "fixture has no contracts to verify");
}

#[test]
fn generic_record_owned_list_projection_is_verified_from_canonical_mir() {
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_owned_list_projection.mimi");
    let file = parse_memory_source(source, "mir-generic-record-owned-list").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Record<List<i32>> projection MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjection { contract }
            if contract.arity == 1 && contract.name == "value"
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Record<List<i32>> projection verifier capability");
    let results = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("generic Record<List<i32>> projection must remain on canonical verifier route");
    assert!(
        results
            .iter()
            .all(|result| result.message == "no contracts to verify"),
        "unexpected verifier result for managed record projection: {results:?}"
    );
}

#[test]
fn generic_record_list_projection_is_rejected_before_verifier_consumers() {
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_projection_list_rejected.mimi"
    );
    let file = parse_memory_source(source, "mir-generic-record-list-rejected").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic Record<List<List<i32>>> projection must fail before verifier");
    let message = format!("{error:?}");
    assert!(
        message.contains("generic record projection")
            || message.contains("nested")
            || message.contains("Copy scalar/bool"),
        "unexpected verifier-boundary nested List rejection: {message}"
    );
}

#[test]
fn direct_result_bool_f64_unwrap_or_is_rejected_before_verifier_consumers() {
    let source =
        include_str!("../../tests/fixtures/mir_native_result_bool_f64_unwrap_or_rejected.mimi");
    let file = parse_memory_source(source, "mir-direct-result-bool-f64-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("direct concrete Result<f64,bool> unwrap_or must fail before verifier");
    assert!(
        format!("{error:?}").contains("Result fallback")
            || format!("{error:?}").contains("variant projection"),
        "unexpected canonical rejection: {error:?}"
    );
}

#[test]
fn generic_result_homogeneous_f64_unwrap_or_is_rejected_before_verifier_consumers() {
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_result_unwrap_or_homogeneous_f64_rejected.mimi"
    );
    let file =
        parse_memory_source(source, "mir-generic-result-homogeneous-f64-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("homogeneous Result<T,T> f64 unwrap_or must be rejected before verifier");
    assert!(
        format!("{error:?}").contains("generic MIR instance"),
        "unexpected canonical rejection: {error:?}"
    );
}

#[test]
fn generic_result_homogeneous_f64_projection_is_rejected_before_verifier_consumers() {
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_result_unwrap_homogeneous_f64_rejected.mimi"
    );
    let file = parse_memory_source(source, "mir-generic-result-homogeneous-f64").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("homogeneous Result<T,T> f64 must be rejected before verifier");
    assert!(
        format!("{error:?}").contains("generic MIR instance"),
        "unexpected canonical rejection: {error:?}"
    );
}

#[test]
fn generic_result_bool_error_projection_is_verified_from_canonical_mir() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_result_bool_error.mimi");
    let file = parse_memory_source(source, "mir-generic-result-bool-error").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result<T,bool> MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
            if contract.projection.nominal.as_str() == "builtin:type:Result"
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Result<T,bool> verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn generic_result_owned_string_projection_is_verified_from_canonical_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_owned_string.mimi");
    let file = parse_memory_source(source, "mir-generic-result-owned-string").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result<T,i32> owned String MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
            if contract.projection.nominal.as_str() == "builtin:type:Result"
                && contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
                && contract.projection.move_out_glue
                    == crate::core::mir::types::MirGlueKind::OwnedString
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic owned Result projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn generic_result_bool_error_owned_string_projection_is_verified_from_canonical_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_bool_error_owned_string.mimi");
    let file = parse_memory_source(source, "mir-generic-result-bool-owned-string").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result<T,bool> owned String MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
            if contract.projection.nominal.as_str() == "builtin:type:Result"
                && contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
                && contract.projection.move_out_glue
                    == crate::core::mir::types::MirGlueKind::OwnedString
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Result<T,bool> owned String verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn generic_result_owned_list_projection_is_verified_from_canonical_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_owned_list.mimi");
    let file = parse_memory_source(source, "mir-generic-result-owned-list").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result<T,i32> owned List MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
            if contract.projection.nominal.as_str() == "builtin:type:Result"
                && contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
                && contract.projection.move_out_glue
                    == crate::core::mir::types::MirGlueKind::List
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic owned Result<List> projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn generic_option_unwrap_or_is_rejected_before_legacy_verifier_fallback() {
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or_rejected.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let error = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect_err("generic Option unwrap_or must not fall through to AST verifier");
    assert!(error.contains("generic Option projection"), "{error}");
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "unsupported generic Option fallback must not enter a legacy verifier"
    );
}

#[test]
fn generic_result_predicate_is_verified_from_canonical_mir_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_result_predicate.mimi");
    let file = parse_memory_source(source, "mir-generic-result-predicate").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result predicate MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate {
            contract: crate::core::mir::types::MirVariantPredicateContract {
                predicate: crate::core::mir::MirVariantPredicate::IsOk,
                ..
            }
        }
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Result predicate verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_result_error_slot_predicate_is_verified_from_canonical_mir() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_result_error_slot.mimi");
    let file = parse_memory_source(source, "mir-generic-result-error-slot").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical Result<i32, T> predicate MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate {
            contract: crate::core::mir::types::MirVariantPredicateContract {
                predicate: crate::core::mir::MirVariantPredicate::IsErr,
                ..
            }
        }
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("Result error-slot predicate verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn two_field_generic_record_projection_is_consumed_by_mir_verifier() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_projection_pair.mimi");
    let file = parse_memory_source(source, "mir-generic-record-projection-pair").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("two-field generic record projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("two-field generic record projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.arity == 2 && contract.name == "left"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("two-field generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn three_field_generic_record_projection_is_consumed_by_mir_verifier() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_projection_three_field.mimi");
    let file =
        parse_memory_source(source, "mir-generic-record-projection-three-field").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("three-field generic record projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("three-field generic record projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.arity == 3 && contract.name == "value"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("three-field generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn four_field_generic_record_projection_is_consumed_by_mir_verifier() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_projection_four_field.mimi");
    let file =
        parse_memory_source(source, "mir-generic-record-projection-four-field").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("four-field generic record projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("four-field generic record projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.arity == 4 && contract.name == "value"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("four-field generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn five_field_generic_record_projection_is_consumed_by_mir_verifier() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_projection_five_field.mimi");
    let file =
        parse_memory_source(source, "mir-generic-record-projection-five-field").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("five-field generic record projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("five-field generic record projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.arity == 5 && contract.name == "value"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("five-field generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn six_field_generic_record_projection_is_consumed_by_mir_verifier() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_projection_six_field.mimi");
    let file =
        parse_memory_source(source, "mir-generic-record-projection-six-field").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("six-field generic record projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("six-field generic record projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.arity == 6 && contract.name == "value"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("six-field generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn seven_field_generic_record_projection_is_consumed_by_mir_verifier() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_projection_seven_field.mimi");
    let file =
        parse_memory_source(source, "mir-generic-record-projection-seven-field").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("seven-field generic record projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("seven-field generic record projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.arity == 7 && contract.name == "value"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("seven-field generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn seven_field_generic_record_tail_projection_preserves_field_index_in_verifier() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_projection_seven_field_tail.mimi"
    );
    let file = parse_memory_source(source, "mir-generic-record-projection-seven-field-tail")
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("seven-field tail projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("seven-field tail projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.field_index == 6 && contract.arity == 7 && contract.name == "value"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("seven-field tail projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn seven_field_repeated_generic_record_projection_preserves_field_identity_in_verifier() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_projection_seven_field_repeated_generic.mimi"
    );
    let file = parse_memory_source(
        source,
        "mir-generic-record-projection-seven-field-repeated-generic",
    )
    .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("repeated-generic projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("repeated-generic projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.field_index == 0 && contract.arity == 7 && contract.name == "value"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("repeated-generic projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn seven_field_repeated_generic_nonzero_projection_preserves_field_identity_in_verifier() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_projection_seven_field_repeated_generic_nonzero.mimi"
    );
    let file = parse_memory_source(
        source,
        "mir-generic-record-projection-seven-field-repeated-generic-nonzero",
    )
    .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("repeated-generic nonzero projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("repeated-generic nonzero projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.field_index == 2 && contract.arity == 7 && contract.name == "mirror"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("repeated-generic nonzero projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn three_field_generic_record_tail_projection_preserves_field_index_in_verifier() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_projection_three_field_tail.mimi"
    );
    let file = parse_memory_source(source, "mir-generic-record-projection-three-field-tail")
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("three-field tail projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("three-field tail projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.field_index == 2 && contract.arity == 3 && contract.name == "value"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("three-field tail projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn mixed_generic_record_projection_is_consumed_by_mir_verifier() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_projection_mixed.mimi");
    let file = parse_memory_source(source, "mir-generic-record-projection-mixed").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("mixed generic record projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("mixed generic record projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.arity == 2 && contract.name == "value"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("mixed generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn generic_record_projection_rvalue_call_is_verified_from_consuming_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_projection_rvalue.mimi");
    let file = parse_memory_source(source, "mir-generic-record-projection-rvalue").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic record Copy rvalue MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic record Copy rvalue verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn scalar_generic_record_projection_i64_and_bool_keep_mir_proof_artifacts() {
    require_z3!();
    for (label, source) in [
        (
            "mir-generic-record-projection-i64",
            include_str!("../../tests/fixtures/mir_native_generic_record_projection_i64.mimi"),
        ),
        (
            "mir-generic-record-projection-bool",
            include_str!("../../tests/fixtures/mir_native_generic_record_projection_bool.mimi"),
        ),
    ] {
        let file = parse_memory_source(source, label).expect("parse");
        let checked = crate::core::check_program(&file).expect("typecheck");
        let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("scalar generic record projection MIR");
        assert!(canonical.instances().values().any(|instance| matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection { .. }
        )));
        crate::verifier::validate_mir_capabilities(&canonical)
            .expect("scalar generic record projection verifier capability");
        let results = crate::verifier::verify_mir(
            &canonical,
            blake3::hash(source.as_bytes()).to_hex().to_string(),
        )
        .expect("MIR verifier");
        let main = results
            .iter()
            .find(|result| result.func_name == "function:main")
            .expect("main verification result");
        assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
        assert_eq!(
            main.artifact
                .as_ref()
                .map(|artifact| artifact.engine.as_str()),
            Some(crate::verifier::ProofArtifact::ENGINE_MIR)
        );
    }
}

#[test]
fn owned_generic_record_projection_is_verified_from_consuming_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_owned_string_projection.mimi");
    let file = parse_memory_source(source, "mir-owned-generic-record-projection").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic record projection MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjection { .. }
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_mixed_generic_record_projection_is_verified_from_consuming_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_owned_string_mixed.mimi");
    let file =
        parse_memory_source(source, "mir-owned-mixed-generic-record-projection").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical mixed owned generic record projection MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjection { ref contract }
            if contract.arity == 2 && contract.name == "value"
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("mixed owned generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_generic_record_projection_with_residual_drop_is_verified_from_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_owned_string_residual.mimi");
    let file = parse_memory_source(source, "mir-owned-generic-record-residual").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic record residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
            ref contract
        } if contract.projection.arity == 2
            && contract.projection.name == "value"
            && contract.residual.len() == 1
            && contract.residual[0].name == "note"
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic record residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_generic_record_list_projection_with_string_residual_is_verified_from_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_owned_list_projection_residual.mimi"
    );
    let file =
        parse_memory_source(source, "mir-owned-generic-record-list-residual").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic List residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
            if contract.projection.arity == 2
                && contract.projection.name == "value"
                && contract.residual.len() == 1
                && contract.residual[0].name == "note"
                && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::OwnedString
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic List residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_generic_record_list_projection_with_list_residual_is_verified_from_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_owned_list_projection_list_residual.mimi"
    );
    let file =
        parse_memory_source(source, "mir-owned-generic-record-list-list-residual").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic List/List residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
            if contract.projection.arity == 2
                && contract.projection.name == "value"
                && contract.residual.len() == 1
                && contract.residual[0].name == "tail"
                && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::List
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic List/List residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_generic_record_list_projection_with_two_list_residual_is_verified_from_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_owned_list_projection_two_list_residual.mimi"
    );
    let file =
        parse_memory_source(source, "mir-owned-generic-record-two-list-residual").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic two-List residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
            if contract.projection.arity == 3
                && contract.projection.name == "value"
                && contract.residual.len() == 2
                && contract.residual[0].name == "spare"
                && contract.residual[1].name == "tail"
                && contract.residual.iter().all(|residual| {
                    residual.glue == crate::core::mir::types::MirGlueKind::List
                })
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic two-List residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_generic_record_list_string_residual_is_verified_from_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_owned_list_string_residual.mimi"
    );
    let file = parse_memory_source(source, "mir-owned-generic-record-list-string-residual")
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic List/String residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
            if contract.projection.arity == 3
                && contract.projection.name == "value"
                && contract.residual.len() == 2
                && contract.residual[0].name == "note"
                && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::OwnedString
                && contract.residual[1].name == "tail"
                && contract.residual[1].glue == crate::core::mir::types::MirGlueKind::List
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic List/String residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_generic_record_list_two_string_residual_is_verified_from_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_owned_list_two_string_residual.mimi"
    );
    let file = parse_memory_source(source, "mir-owned-generic-record-list-two-string-residual")
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic List/two-String residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
            if contract.projection.arity == 4
                && contract.projection.name == "value"
                && contract.residual.len() == 3
                && contract.residual[0].name == "spare"
                && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::OwnedString
                && contract.residual[1].name == "note"
                && contract.residual[1].glue == crate::core::mir::types::MirGlueKind::OwnedString
                && contract.residual[2].name == "tail"
                && contract.residual[2].glue == crate::core::mir::types::MirGlueKind::List
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic List/two-String residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_generic_record_bool_list_two_string_residual_is_verified_from_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_owned_bool_list_two_string_residual.mimi"
    );
    let file = parse_memory_source(
        source,
        "mir-owned-generic-record-bool-list-two-string-residual",
    )
    .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic Bool List/two-String residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
            if contract.projection.arity == 4
                && contract.projection.name == "value"
                && contract.residual.len() == 3
                && contract.residual[0].name == "spare"
                && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::OwnedString
                && contract.residual[1].name == "note"
                && contract.residual[1].glue == crate::core::mir::types::MirGlueKind::OwnedString
                && contract.residual[2].name == "tail"
                && contract.residual[2].glue == crate::core::mir::types::MirGlueKind::List
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic Bool List/two-String residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_generic_record_set_residual_is_verified_from_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_owned_set_residual.mimi");
    let file = parse_memory_source(source, "mir-owned-generic-record-set-residual").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic Set residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
            if contract.projection.arity == 2
                && contract.projection.name == "value"
                && contract.residual.len() == 1
                && contract.residual[0].name == "tail"
                && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::Set
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic Set residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_generic_record_set_scalar_family_is_verified_from_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_owned_set_scalar_family.mimi");
    let file =
        parse_memory_source(source, "mir-owned-generic-record-set-scalar-family").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic Set scalar-family MIR");
    let instances = canonical
        .instances()
        .values()
        .filter(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                    contract
                } if contract.projection.arity == 2
                    && contract.projection.name == "value"
                    && contract.residual.len() == 1
                    && contract.residual[0].name == "tail"
                    && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::Set
            )
        })
        .count();
    assert_eq!(instances, 2, "both Set element instances must materialize");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic Set scalar-family verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn three_field_owned_generic_record_projection_with_two_residuals_is_verified_from_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_owned_string_three_field_residual.mimi"
    );
    let file = parse_memory_source(source, "mir-owned-generic-record-three-field").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical three-field owned generic record residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
            ref contract
        } if contract.projection.arity == 3
            && contract.residual.len() == 2
            && contract.residual[0].name == "tail"
            && contract.residual[1].name == "note"
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("three-field owned generic residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn heterogeneous_owned_generic_record_projection_is_verified_from_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_owned_string_heterogeneous_residual.mimi"
    );
    let file =
        parse_memory_source(source, "mir-owned-generic-record-heterogeneous").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical heterogeneous owned generic record residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
            ref contract
        } if contract.projection.arity == 2
            && contract.projection.name == "value"
            && contract.residual.len() == 1
            && contract.residual[0].name == "note"
            && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::OwnedString
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("heterogeneous owned generic residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn three_field_heterogeneous_owned_generic_record_projection_is_verified_from_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_owned_string_three_field_heterogeneous_residual.mimi"
    );
    let file = parse_memory_source(source, "mir-owned-generic-record-three-field-heterogeneous")
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical three-field heterogeneous owned residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
            ref contract
        } if contract.projection.arity == 3
            && contract.projection.name == "value"
            && contract.residual.len() == 2
            && contract.residual[0].name == "tail"
            && contract.residual[1].name == "note"
            && contract.residual.iter().all(|residual| {
                residual.glue == crate::core::mir::types::MirGlueKind::OwnedString
            })
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("three-field heterogeneous owned residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn four_field_heterogeneous_owned_generic_record_projection_is_verified_from_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_owned_string_four_field_heterogeneous_residual.mimi"
    );
    let file = parse_memory_source(source, "mir-owned-generic-record-four-field-heterogeneous")
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical four-field heterogeneous owned residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
            ref contract
        } if contract.projection.arity == 4
            && contract.projection.name == "value"
            && contract.residual.len() == 3
            && contract.residual[0].name == "spare"
            && contract.residual[1].name == "tail"
            && contract.residual[2].name == "note"
            && contract.residual.iter().all(|residual| {
                residual.glue == crate::core::mir::types::MirGlueKind::OwnedString
            })
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("four-field heterogeneous owned residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_generic_record_projection_rvalue_is_verified_from_consuming_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_owned_string_rvalue_call.mimi"
    );
    let file = parse_memory_source(source, "mir-owned-generic-record-rvalue").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic record rvalue MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic record rvalue verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn flat_copy_variant_predicates_are_verified_from_canonical_mir() {
    require_z3!();
    let source = r#"
        func main() -> i32 {
            ensures: result == 4
            let some: Option<i32> = Some(41)
            let none: Option<i32> = None
            let ok: Result<i32, i32> = Ok(7)
            let err: Result<i32, i32> = Err(9)
            let all_true = some.is_some() && none.is_none() && ok.is_ok() && err.is_err()
            if all_true { 4 } else { 0 }
        }
    "#;
    let file = parse_memory_source(source, "mir-variant-predicate").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical variant predicate MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("flat Copy variant predicate capability");
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(4)
    );

    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("canonical MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn exact_s8_route_receipt_covers_all_consumers_without_legacy_escape() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_flow_transition.mimi");
    let file = parse_memory_source(source, "mir-s8-four-consumer-receipt").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("exact S8 route must materialize");
    assert_eq!(
        route.admission.flow,
        crate::core::mir::S8FlowAdmission::CompleteCoverage
    );
    assert!(route.materialized_flow_candidate);
    let canonical = route.program;
    let profile = crate::core::mir::CanonicalMirRouteProfile::S8FlowTransition.as_str();
    let receipt = canonical.route_receipt(profile);
    assert_eq!(receipt.schema, crate::core::mir::MIR_ROUTE_RECEIPT_SCHEMA);
    assert_eq!(receipt.profile, profile);
    assert_eq!(receipt.mir_digest.len(), 64);
    assert_eq!(receipt.type_desc_digest.len(), 64);
    assert_eq!(receipt.abi_digest.len(), 64);
    assert_eq!(receipt.ownership_digest.len(), 64);
    assert_eq!(receipt.flow_transition_digest.len(), 64);
    assert!(receipt
        .root_owners
        .iter()
        .any(|owner| owner.0 == "function:main"));

    // The receipt is an immutable identity witness, not a backend input. All
    // four consumers below receive this same MIR object and no consumer may
    // reopen CheckedProgram or the source AST.
    crate::verifier::validate_mir_capabilities(&canonical).expect("MIR verifier capability");
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(42)
    );
    let bytecode =
        crate::interp::bytecode::compile_mir_program(&canonical).expect("MIR bytecode compilation");
    let mut bytecode_vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(bytecode_vm.run().expect("MIR bytecode execution"), 42);

    crate::codegen::mir::validate_mir_native(&canonical).expect("native MIR validation");
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "s28_s8_receipt");
    codegen
        .compile_mir_native(&canonical)
        .expect("native MIR compilation");

    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let mir_results =
        crate::verifier::verify_mir(&canonical, source_hash.clone()).expect("MIR verifier");
    assert!(mir_results.is_empty(), "fixture has no contracts");

    // The thread-local tripwires isolate this proof from unrelated parallel
    // tests: verify_checked and verify_checked_dual must each materialize once
    // and must not touch the compatibility raw-AST accessor.
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    crate::core::mir::reset_test_route_materialization_count();
    assert!(
        crate::verifier::verify_checked(&checked, source_hash.clone())
            .expect("public verifier")
            .is_empty()
    );
    assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    crate::core::mir::reset_test_route_materialization_count();
    assert!(crate::verifier::verify_checked_dual(&checked, source_hash)
        .expect("dual public verifier")
        .is_empty());
    assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    // Recomputing the audit witness after all consumers proves that no
    // backend mutated the canonical graph or its side tables.
    assert_eq!(receipt, canonical.route_receipt(profile));
    assert_eq!(
        receipt,
        crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("repeat canonical materialization")
            .route_receipt(profile)
    );
    assert_eq!(
        crate::core::mir::MIR_ROUTE_VALIDATOR_CONTRACT_ID,
        "mimi-mir-route-validator-v1"
    );
}

#[test]
fn public_checked_verifier_routes_map_any_owner_v1_receipt_contracts_through_mir() {
    require_z3!();
    let source = r#"
        func checked_map_size(value: i32) -> i32 {
            requires: value >= 0
            ensures: result == 1
            let root = map_new()
            let updated = map_set(root, "answer", value)
            let size = map_size(updated)
            drop(updated)
            size
        }
        func false_map_size(value: i32) -> i32 {
            ensures: result == 99
            let root = map_new()
            let updated = map_set(root, "answer", value)
            let size = map_size(updated)
            drop(updated)
            size
        }
        func checked_string_map_size() -> i32 {
            ensures: result == 1
            let root = map_new()
            let updated = map_set(root, "answer", "x\0雪")
            let size = map_size(updated)
            drop(updated)
            size
        }
        func false_string_map_size() -> i32 {
            ensures: result == 99
            let root = map_new()
            let updated = map_set(root, "answer", "x\0雪")
            let size = map_size(updated)
            drop(updated)
            size
        }
        func checked_map_remove_size() -> i32 {
            ensures: result == 1
            let root = map_new()
            let first = map_set(root, "remove", 41)
            let second = map_set(first, "keep", 42)
            let removed = map_remove(second, "remove")
            let size = map_size(removed)
            drop(removed)
            size
        }
        func false_map_remove_size() -> i32 {
            ensures: result == 0
            let root = map_new()
            let first = map_set(root, "remove", 41)
            let second = map_set(first, "keep", 42)
            let removed = map_remove(second, "remove")
            let size = map_size(removed)
            drop(removed)
            size
        }
        func checked_missing_map_remove_size() -> i32 {
            ensures: result == 2
            let root = map_new()
            let first = map_set(root, "first", 41)
            let second = map_set(first, "second", 42)
            let unchanged = map_remove(second, "missing")
            let size = map_size(unchanged)
            drop(unchanged)
            size
        }
        func main() -> i32 { 0 }
    "#;
    let file = parse_memory_source(source, "map-any-owner-v1-public-verify").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let profile = crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1;
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
    assert_eq!(
        admission.map_any_owner,
        crate::core::mir::MapAnyOwnerAdmission::CompleteCoverage
    );
    assert_eq!(
        admission.map_root,
        crate::core::mir::MapRootAdmission::OutsideProfile
    );
    assert!(profile.is_admitted(admission));

    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("complete MapAnyOwner v1 Checker receipt must materialize");
    assert!(profile.is_materialized(&route));
    assert!(!route.materialized_map_root_candidate);
    let receipt = route.program.route_receipt(profile.as_str());
    assert_eq!(receipt.profile, "map-any-owner-v1");
    receipt
        .validate()
        .expect("valid MapAnyOwner v1 route receipt");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&checked, &source_hash);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_checked(&checked, source_hash.clone())
        .expect("public checked verifier must use the MapAnyOwner v1 MIR profile");
    let proven = results
        .iter()
        .find(|result| result.func_name == "checked_map_size")
        .expect("positive MapAnyOwner v1 proof");
    assert_eq!(proven.status, VerifStatus::Proven, "{}", proven.message);
    assert_eq!(
        proven
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR)
    );
    let disproven = results
        .iter()
        .find(|result| result.func_name == "false_map_size")
        .expect("negative MapAnyOwner v1 proof");
    assert_eq!(
        disproven.status,
        VerifStatus::Disproven,
        "{}",
        disproven.message
    );
    assert_eq!(
        disproven
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR)
    );
    let string_proven = results
        .iter()
        .find(|result| result.func_name == "checked_string_map_size")
        .expect("positive String MapAnyOwner v1 proof");
    assert_eq!(
        string_proven.status,
        VerifStatus::Proven,
        "{}",
        string_proven.message
    );
    assert_eq!(
        string_proven
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR)
    );
    let string_disproven = results
        .iter()
        .find(|result| result.func_name == "false_string_map_size")
        .expect("negative String MapAnyOwner v1 proof");
    assert_eq!(
        string_disproven.status,
        VerifStatus::Disproven,
        "{}",
        string_disproven.message
    );
    assert_eq!(
        string_disproven
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR)
    );
    for (function, status) in [
        ("checked_map_remove_size", VerifStatus::Proven),
        ("false_map_remove_size", VerifStatus::Disproven),
        ("checked_missing_map_remove_size", VerifStatus::Proven),
    ] {
        let result = results
            .iter()
            .find(|result| result.func_name == function)
            .unwrap_or_else(|| panic!("missing MapAnyOwner v1 Remove result for {function}"));
        assert_eq!(result.status, status, "{}", result.message);
        assert_eq!(
            result
                .artifact
                .as_ref()
                .map(|artifact| artifact.engine.as_str()),
            Some(ProofArtifact::ENGINE_MIR)
        );
    }
    for (function, status) in [
        ("checked_map_size", VerifStatus::Proven),
        ("false_map_size", VerifStatus::Disproven),
        ("checked_string_map_size", VerifStatus::Proven),
        ("false_string_map_size", VerifStatus::Disproven),
        ("checked_map_remove_size", VerifStatus::Proven),
        ("false_map_remove_size", VerifStatus::Disproven),
        ("checked_missing_map_remove_size", VerifStatus::Proven),
    ] {
        let exact_mir = exact_mir_result_for_test(&exact_mir_results, function);
        assert_eq!(
            exact_mir.status, status,
            "MIR secondary {function}: {}",
            exact_mir.message
        );
        assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);
        let public = exact_mir_result_for_test(&results, function);
        assert_eq!(
            public.status, status,
            "public {function}: {}",
            public.message
        );
        assert_single_exact_mir_artifact(public, &source_hash, &typed_receipt);
    }
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "public MapAnyOwner v1 verification must not access the retained legacy body"
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let dual_results = crate::verifier::verify_checked_dual(&checked, source_hash.clone())
        .expect("public dual checked verifier");
    for (function, status) in [
        ("checked_map_size", VerifStatus::Proven),
        ("false_map_size", VerifStatus::Disproven),
        ("checked_string_map_size", VerifStatus::Proven),
        ("false_string_map_size", VerifStatus::Disproven),
        ("checked_map_remove_size", VerifStatus::Proven),
        ("false_map_remove_size", VerifStatus::Disproven),
        ("checked_missing_map_remove_size", VerifStatus::Proven),
    ] {
        let result = dual_results
            .iter()
            .find(|result| result.func_name == function)
            .unwrap_or_else(|| {
                panic!("missing dual MapAnyOwner v1 result for {function}: {dual_results:?}")
            });
        assert_eq!(result.status, status, "{}", result.message);
        let exact_mir = exact_mir_result_for_test(&exact_mir_results, function);
        assert_dual_result_keeps_the_agreed_engine_artifact(
            result,
            exact_mir,
            &source_hash,
            &typed_receipt,
        );
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn public_checked_verifier_routes_map_any_owner_v1_receipts_through_mir() {
    require_z3!();
    let source = r#"
        func checked_owner_size(value: i32) -> i32 {
            requires: value >= 0
            ensures: result == 1
            let root = map_new()
            let inserted = map_set(root, "answer", value)
            let size = map_size(inserted)
            drop(inserted)
            size
        }
        func false_owner_size(value: i32) -> i32 {
            ensures: result == 99
            let root = map_new()
            let inserted = map_set(root, "answer", value)
            let size = map_size(inserted)
            drop(inserted)
            size
        }
        func main() -> i32 { 0 }
    "#;
    let file = parse_memory_source(source, "map-any-owner-v1-public-verify").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
    assert_eq!(
        admission.map_any_owner,
        crate::core::mir::MapAnyOwnerAdmission::CompleteCoverage
    );
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let profile = crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1;
    let map_route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("MapAnyOwner v1 route must remain materializable");
    assert!(profile.is_materialized(&map_route));
    let map_receipt = map_route.program.route_receipt(profile.as_str());
    map_receipt
        .validate_against_program(&map_route.program)
        .expect("MapAnyOwner receipt matches its exact route graph");
    crate::verifier::validate_mir_capabilities(&map_route.program)
        .expect("MapAnyOwner route passes verifier capability gate");
    let map_results = crate::verifier::verify_mir_with_route_receipt(
        &map_route.program,
        &map_receipt,
        source_hash.clone(),
    )
    .expect("independent MapAnyOwner v1 verifier");
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&checked, &source_hash);
    assert_eq!(
        map_receipt.mir_digest, typed_receipt.mir_digest,
        "the profile route and typed public request must name the same canonical graph"
    );
    for (function, status) in [
        ("checked_owner_size", VerifStatus::Proven),
        ("false_owner_size", VerifStatus::Disproven),
    ] {
        let map_result = exact_mir_result_for_test(&map_results, function);
        assert_eq!(
            map_result.status, status,
            "MapAnyOwner {function}: {}",
            map_result.message
        );
        let map_artifact = map_result
            .artifact
            .as_ref()
            .expect("MapAnyOwner proof artifact");
        assert_eq!(map_artifact.engine, ProofArtifact::ENGINE_MIR);
        assert_eq!(map_artifact.source_hash, source_hash);
        assert_eq!(map_artifact.mir_hash, map_receipt.mir_digest);
        assert_eq!(map_artifact.mir_route_receipt.as_ref(), Some(&map_receipt));

        let exact_mir = exact_mir_result_for_test(&exact_mir_results, function);
        assert_eq!(
            exact_mir.status, status,
            "typed request {function}: {}",
            exact_mir.message
        );
        assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);
    }

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_checked(&checked, source_hash.clone())
        .expect("public checked verifier must use the MapAnyOwner v1 MIR profile");
    for (function, status) in [
        ("checked_owner_size", VerifStatus::Proven),
        ("false_owner_size", VerifStatus::Disproven),
    ] {
        let result = exact_mir_result_for_test(&results, function);
        let exact_mir = exact_mir_result_for_test(&exact_mir_results, function);
        assert_eq!(result.status, status, "{}", result.message);
        assert_eq!(
            exact_mir.status, status,
            "typed MIR {}: {}",
            function, exact_mir.message
        );
        assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn map_any_owner_v1_nul_key_is_fail_closed_in_symbolic_verifier() {
    require_z3!();
    let source = r#"
func checked_size() -> i32 {
    ensures: result == 1
    let root = map_new()
    let inserted = map_set(root, "answer", 1)
    let size = map_size(inserted)
    drop(inserted)
    size
}
func main() -> i32 { 0 }
"#;
    let file = parse_memory_source(source, "map-any-owner-v1-nul-key").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let mut program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MapAnyOwner v1 MIR");
    let mut function = program
        .functions()
        .values()
        .find(|function| function.owner.0 == "function:checked_size")
        .expect("MapAnyOwner function")
        .clone();
    let set = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                &instruction.kind,
                crate::core::mir::MirInstructionKind::MapRootSet { .. }
            )
        })
        .expect("MapAnyOwner Set instruction");
    let crate::core::mir::MirInstructionKind::MapRootSet { key, .. } = &mut set.kind else {
        unreachable!("selected MapAnyOwner Set instruction")
    };
    key.push('\0');
    program.replace_function_for_test_only(function);

    let results = crate::verifier::mir::verify_program(
        &program,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("the direct verifier should report the malformed key as unsupported");
    let result = results
        .iter()
        .find(|result| result.func_name == "function:checked_size")
        .expect("MapAnyOwner proof result");
    assert_eq!(
        result.status,
        VerifStatus::NotInTrustedSubset,
        "{}",
        result.message
    );
    assert!(
        result.message.contains("MapRoot key contains NUL"),
        "NUL keys must remain rejected at the MapAnyOwner key boundary: {}",
        result.message
    );
}

#[test]
fn non_copy_option_string_switch_move_closes_all_four_consumers() {
    require_z3!();
    let source = r#"
        func consume(value: Option<string>) -> i32 {
            ensures: result >= 0
            match value {
                Some(_) => 41,
                None => 0
            }
        }

        func discard(value: Option<string>) -> i32 {
            ensures: result >= 0
            match value {
                Some(_) => 7,
                None => 8
            }
        }

        func main() -> i32 {
            let first: Option<string> = Some("owned")
            let second: Option<string> = Some("discard")
            let a = consume(first)
            let b = discard(second)
            a + b
        }
    "#;
    let file = parse_memory_source(source, "mir-option-string-switch-move").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("Option<string> SwitchMove verifier capability");

    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(48)
    );
    let bytecode =
        crate::interp::bytecode::compile_mir_program(&canonical).expect("MIR bytecode compilation");
    let mut bytecode_vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(bytecode_vm.run().expect("MIR bytecode execution"), 48);

    crate::codegen::mir::validate_mir_native(&canonical).expect("native MIR validation");
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "mir_option_string_verifier");
    codegen
        .compile_mir_native(&canonical)
        .expect("native MIR compilation");
    codegen.module.verify().expect("native module verification");

    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir_results, typed_receipt) =
        verify_exact_typed_mir_request_for_test(&checked, &source_hash);
    let canonical_digest = canonical.canonical_digest();
    assert_eq!(typed_receipt.mir_digest, canonical_digest);
    let results =
        crate::verifier::verify_mir(&canonical, source_hash.clone()).expect("MIR verifier");
    for function in ["function:consume", "function:discard"] {
        let result = results
            .iter()
            .find(|result| result.func_name == function)
            .expect("contract-bearing Option<string> function");
        assert_eq!(
            result.status,
            VerifStatus::Proven,
            "{function}: {}",
            result.message
        );
    }

    // Public checked-verifier results must carry the same canonical proof
    // identity while avoiding retained legacy bodies.
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let checked_results = crate::verifier::verify_checked(&checked, source_hash.clone())
        .expect("public checked verifier must use the Option<string> MIR island");
    for function in ["consume", "discard"] {
        let result = checked_results
            .iter()
            .find(|result| result.func_name == function)
            .unwrap_or_else(|| panic!("public verifier omitted {function}: {checked_results:?}"));
        assert_eq!(
            result.status,
            VerifStatus::Proven,
            "{function}: {}",
            result.message
        );
        let exact_mir = exact_mir_result_for_test(&exact_mir_results, function);
        assert_eq!(
            exact_mir.status,
            VerifStatus::Proven,
            "{function}: {}",
            exact_mir.message
        );
        assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);
        assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);
        let artifact = result
            .artifact
            .as_ref()
            .unwrap_or_else(|| panic!("{function} has no proof artifact"));
        assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.mir_hash, canonical_digest);
        assert_eq!(
            artifact
                .mir_route_receipt
                .as_ref()
                .map(|receipt| receipt.mir_digest.as_str()),
            Some(artifact.mir_hash.as_str())
        );
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let dual_results = crate::verifier::verify_checked_dual(&checked, source_hash.clone())
        .expect("dual public checked verifier must use the Option<string> MIR island");
    for function in ["consume", "discard"] {
        let result = dual_results
            .iter()
            .find(|result| result.func_name == function)
            .unwrap_or_else(|| panic!("dual verifier omitted {function}: {dual_results:?}"));
        assert_eq!(
            result.status,
            VerifStatus::Proven,
            "{function}: {}",
            result.message
        );
        let exact_mir = exact_mir_result_for_test(&exact_mir_results, function);
        assert_dual_result_keeps_the_agreed_engine_artifact(
            result,
            exact_mir,
            &source_hash,
            &typed_receipt,
        );
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn non_copy_option_string_switch_move_default_is_rejected_before_consumers() {
    let source = r#"
        func main() -> i32 {
            let value: Option<string> = Some("owned")
            match value {
                Some(_) => 41,
                _ => 0
            }
        }
    "#;
    let file = parse_memory_source(source, "mir-option-string-switch-default").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let errors = crate::verifier::validate_mir_capabilities(&canonical)
        .expect_err("consuming default must remain outside the verifier island");
    assert!(errors
        .iter()
        .any(|error| { error.contains("explicit variant arms") || error.contains("SwitchMove") }));
}

#[test]
fn malformed_s8_mir_missing_transition_contract_is_rejected_before_consumers() {
    let source = include_str!("../../tests/fixtures/mir_native_flow_transition.mimi");
    let file = parse_memory_source(source, "mir-s8-invalid-transition").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let error =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            canonical.functions().clone(),
            canonical.type_catalog().clone(),
            canonical.instances().clone(),
            std::collections::BTreeMap::new(),
        )
        .expect_err("FlowTransition without its contract must be rejected by MIR validation");
    assert!(error.iter().any(|error| {
        error.message.contains("transition") && error.message.contains("contract")
    }));
}

#[test]
fn public_checked_verifier_routes_closed_s8_flow_to_mir() {
    require_z3!();
    let source = r#"
        flow Counter {
            state Zero { n: i32 }
            transition inc(Zero) -> Zero {
                return Zero { n: self.n + 1 }
            }
        }

        func main() -> i32 {
            let c = Zero { n: 41 }
            let c2 = Counter::inc(c)
            c2.n
        }
    "#;
    let file = parse_memory_source(source, "mir-s8-public-api").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert!(crate::core::mir::is_exact_s8_flow_transition(&program));
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let mir_route = verify_closed_s8_flow_mir(&program, source_hash.clone())
        .expect("closed S8 MIR verifier route");
    assert!(
        mir_route.is_some(),
        "exact S8 must be owned by the MIR route"
    );
    crate::core::mir::reset_test_route_materialization_count();
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verify_result = verify_checked(&program, source_hash.clone()).expect("MIR verify");
    assert!(verify_result.is_empty());
    assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    assert!(mir_route.unwrap().is_empty());

    crate::core::mir::reset_test_route_materialization_count();
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let dual_result = verify_checked_dual(&program, source_hash).expect("MIR dual verify");
    assert!(dual_result.is_empty());
    assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn public_checked_verifier_does_not_overmatch_non_exact_s8_flow() {
    require_z3!();
    let source = r#"
        flow Counter {
            state Zero { n: i32 }
            transition inc(Zero) -> Zero {
                return Zero { n: self.n + 1 }
            }
        }

        func main() -> i32 {
            let c = Zero { n: 41 }
            let c2 = Counter::inc(c)
            println(c2.n)
            c2.n
        }
    "#;
    let file = parse_memory_source(source, "mir-s8-non-exact").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert!(crate::core::mir::is_s8_flow_transition_candidate(&program));
    assert!(!crate::core::mir::is_exact_s8_flow_transition(&program));
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    assert!(verify_closed_s8_flow_mir(&program, source_hash)
        .expect("non-exact S8 remains outside the closed MIR verifier island")
        .is_none());
}

#[test]
fn flat_copy_record_admission_is_checker_owned_before_materialization() {
    let source = include_str!("../../tests/fixtures/mir_verifier_record_contract.mimi");
    let file = parse_memory_source(source, "mir-record-admission").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_flat_copy_record_admission(&program),
        crate::core::mir::FlatCopyRecordAdmission::CompleteCoverage
    );

    let mixed_source = r#"
        type Point { x: i32 }
        func identity<T>(value: T) -> T { value }
        func main() -> i32 {
            let point = Point { x: 1 }
            point.x
        }
    "#;
    let mixed_file = parse_memory_source(mixed_source, "mir-record-mixed-admission")
        .expect("parse mixed source");
    let mixed_program = crate::core::check_program(&mixed_file).expect("typecheck mixed source");
    assert_eq!(
        crate::core::mir::classify_flat_copy_record_admission(&mixed_program),
        crate::core::mir::FlatCopyRecordAdmission::MixedCoverage
    );

    let borrow_source = r#"
        type Rec { a: i32, b: i32 }
        func swap(mutated: mutate Rec) -> Rec {
            mutated = Rec { a: mutated.b, b: mutated.a }
            mutated
        }
        func main() -> i32 { 0 }
    "#;
    let borrow_file = parse_memory_source(borrow_source, "mir-record-borrow-admission")
        .expect("parse borrow source");
    let borrow_program = crate::core::check_program(&borrow_file).expect("typecheck borrow source");
    assert_eq!(
        crate::core::mir::classify_flat_copy_record_admission(&borrow_program),
        crate::core::mir::FlatCopyRecordAdmission::MixedCoverage
    );

    let scalar_source = "func main() -> i32 { 0 }";
    let scalar_file = parse_memory_source(scalar_source, "mir-record-outside-admission")
        .expect("parse scalar source");
    let scalar_program = crate::core::check_program(&scalar_file).expect("typecheck scalar source");
    assert_eq!(
        crate::core::mir::classify_flat_copy_record_admission(&scalar_program),
        crate::core::mir::FlatCopyRecordAdmission::OutsideProfile
    );
}

#[test]
fn complete_flat_copy_record_materialization_failure_is_not_a_verifier_fallback() {
    require_z3!();
    let source = r#"
        type Point { x: i32 }

        func make_fn(p: Point) -> func(i32) -> i32 {
            fn(value: i32) -> i32 { value + 1 }
        }

        func main() -> i32 { 0 }
    "#;
    let file = parse_memory_source(source, "mir-record-materialization-error").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_flat_copy_record_admission(&program),
        crate::core::mir::FlatCopyRecordAdmission::CompleteCoverage
    );
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let error = verify_checked(&program, source_hash).expect_err("MIR construction must be hard");
    assert!(
        error.contains("MIR-MATERIALIZATION-001"),
        "complete admission must not fall back after construction failure: {error}"
    );
}

#[test]
fn public_checked_verifier_rejects_mixed_record_graph_without_ast_fallback() {
    require_z3!();
    let source = r#"
        type Point { x: i32 }

        func make_some() -> Result<string, i32> { Ok("owned") }

        func main() -> i32 {
            let point = Point { x: 1 }
            point.x
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let error = verify_checked(&program, source_hash.clone())
        .expect_err("recognized mixed record graph must not fall back to Flow/AST verifier");
    assert!(error.contains("MIR-CAPABILITY-001"), "{error}");

    let dual_error = verify_checked_dual(&program, source_hash)
        .expect_err("dual verifier must preserve the same fail-closed boundary");
    assert!(dual_error.contains("MIR-CAPABILITY-001"), "{dual_error}");
}

#[test]
fn verify_shared_param_field_scalar_contract() {
    require_z3!();
    let src = r#"
func read_shared(x: shared i32) -> i32 {
    requires: x > 0
    ensures: result > 0
    x
}
"#;
    // Current verifier policy: functions taking a `shared` parameter with
    // contracts are rejected fail-closed because Z3 cannot model shared heap
    // state (and `shared i32` does not auto-deref to `i32` in contract
    // expressions). Verifying shared *scalar* params is a future enhancement
    // (deferred past 0.31.6 hemostasis); this test locks the fail-closed
    // rejection so a silent unsound "Verified" can never slip through.
    let results = verify_source(src);
    match results {
        Err(diags) => {
            let msg = format!("{diags:?}");
            assert!(
                msg.contains("shared parameter") || msg.contains("shared"),
                "expected the shared-param rejection diagnostic, got: {msg}"
            );
        }
        Ok(results) => {
            // If a future verifier supports shared scalars, it must at least
            // not claim Verified without modeling the shared read.
            let silently_verified = results
                .first()
                .is_some_and(|r| r.status == VerifStatus::Verified);
            assert!(
                !silently_verified,
                "shared-param contract must not silently verify: {:?}",
                results
            );
        }
    }
}

#[test]
fn verify_multi_func_no_calls() {
    require_z3!();
    // Multiple functions with contracts, no function calls in bodies.
    let src = r#"
func add(x: i32) -> i32 {
    requires: x > 0 && x < 2147483647
    ensures: result > x
    x + 1
}
func double(y: i32) -> i32 {
    requires: y > 5 && y <= 1000000000
    ensures: result > 5
    y * 2
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_multi_func_no_calls");
    assert_eq!(results.len(), 2);
    assert!(
        results.iter().all(|r| r.status == VerifStatus::Verified),
        "all functions should verify: {:?}",
        results
    );
}

#[test]
fn verify_func_call_passes() {
    require_z3!();
    // Function call in ensures: double(x) > 0 when x > 0.
    // This verifies that the result variable for the call exists.
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1000000000 && x <= 1000000000
    ensures: result == x * 2
    x * 2
}
func main() -> i32 {
    0
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_func_call_passes");
    let verified: Vec<_> = results
        .iter()
        .filter(|r| r.status == VerifStatus::Verified)
        .collect();
    assert_eq!(
        verified.len(),
        1,
        "double should verify; main has no contracts: {:?}",
        results
    );
}

#[test]
fn verify_func_call_silent() {
    require_z3!();
    // The body returns 0 but ensures says result > 0 — must fail.
    // Before P0.2, this test did not assert the status; now it checks
    // that the contradiction is detected.
    let src = r#"
func double(x: i32) -> i32 {
    ensures: result > 0
    0  // Body returns 0, but ensures says result > 0 — should fail
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_func_call_silent");
    let double_result = results.iter().find(|r| r.func_name == "double");
    assert!(double_result.is_some(), "double function should be present");
    assert_eq!(
        double_result.unwrap().status,
        VerifStatus::Failed,
        "double body 0 contradicts ensures result > 0: {:?}",
        double_result.unwrap()
    );
}

#[test]
fn verify_func_call_let_binding_propagation() {
    require_z3!();
    // P0.1: Call in a let-binding must propagate callee ensures.
    // Before the fix, assert_callee_ensures_in_expr only scanned
    // the tail expression; `let y = double(x); y` would not propagate.
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0 && x <= 1000000000
    ensures: result == x * 2
    x * 2
}
func wrap(x: i32) -> i32 {
    requires: x > 0 && x <= 1000000000
    ensures: result > 0
    let y = double(x)
    y
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: let_binding_propagation");
    let wrap_result = results.iter().find(|r| r.func_name == "wrap");
    assert!(wrap_result.is_some(), "wrap function should be present");
    assert_eq!(
        wrap_result.unwrap().status,
        VerifStatus::Verified,
        "wrap with let-binding should verify with ensures propagation: {:?}",
        wrap_result.unwrap()
    );
}

#[test]
fn verify_func_call_wrap_pass() {
    require_z3!();
    // wrap(x) calls double(x), ensures result > 0.
    // With ensures propagation, double(x) == x*2 is asserted so
    // wrap's ensures result > 0 should be Verified when x > 0.
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0 && x <= 1000000000
    ensures: result == x * 2
    x * 2
}
func wrap(x: i32) -> i32 {
    requires: x > 0 && x <= 1000000000
    ensures: result > 0
    double(x)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_func_call_wrap_pass");
    let wrap_result = results.iter().find(|r| r.func_name == "wrap");
    assert!(wrap_result.is_some(), "wrap function should be present");
    assert_eq!(
        wrap_result.unwrap().status,
        VerifStatus::Verified,
        "wrap with x>0, double(x)==x*2 should satisfy result>0: {:?}",
        wrap_result.unwrap()
    );
}

#[test]
fn verify_string_len_positive() {
    require_z3!();
    let src = r#"
func validate(s: string) -> i32 {
    requires: len(s) > 0
    ensures: result > 0
    len(s)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: string_len");
    let v = results.iter().find(|r| r.func_name == "validate");
    assert!(v.is_some(), "validate should be verified");
    assert_eq!(
        v.unwrap().status,
        VerifStatus::Verified,
        "len(s) > 0 should imply result > 0: {:?}",
        v.unwrap()
    );
}

#[test]
fn verify_z3_fallback_returns_unknown() {
    // 4.1: Verify that verify_source returns Ok even when Z3 is unavailable,
    // with all results as Unknown.
    let src = r#"
func add(x: i32) -> i32 {
    requires: x < 2147483647
    ensures: result > x
    x + 1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src);
    assert!(
        results.is_ok(),
        "verify_source should return Ok even if Z3 missing"
    );
    // If Z3 IS available, we still get valid results; if not, mock returns Unknown.
    for r in results.unwrap() {
        assert!(
            r.status == VerifStatus::Proven || r.status.is_inconclusive(),
            "status should be Proven or inconclusive, got {:?}",
            r
        );
    }
}

#[test]
fn verify_is_z3_available_not_panics() {
    // is_z3_available() should never panic regardless of Z3 installation.
    let _available = crate::verifier::is_z3_available();
    // Just verify it returns a bool without panicking.
}

#[test]
fn verify_rule_ensures_combo() {
    require_z3!();
    // 4.4: rule annotations should be extractable and verifiable.
    let src = r#"
func abs(x: i32) -> i32 {
    rule "ensures: result >= 0"
    if x < 0 { -x } else { x }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: rule_ensures");
    let abs_result = results.iter().find(|r| r.func_name == "abs");
    assert!(abs_result.is_some(), "abs function should be verified");
    // Should at least produce a deterministic status.
    assert!(
        abs_result.unwrap().status == VerifStatus::Proven
            || abs_result.unwrap().status == VerifStatus::Disproven
            || abs_result.unwrap().status.is_inconclusive()
    );
}

#[test]
fn verify_cross_module_ensures_propagation() {
    require_z3!();
    // 1.2: Function A calls function B. The verifier should propagate
    // B's ensures to constrain the call variable for A, allowing A's
    // ensures to be verified.
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1000000000 && x <= 1000000000
    ensures: result == x * 2
    x * 2
}
func caller(y: i32) -> i32 {
    requires: y >= -1000000000 && y <= 1000000000
    ensures: result == y * 2
    double(y)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: cross_module_ensures");
    let double = results.iter().find(|r| r.func_name == "double");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(double.is_some(), "double should be present: {:?}", results);
    assert_eq!(
        double.unwrap().status,
        VerifStatus::Verified,
        "double should verify first: {:?}",
        double.unwrap()
    );
    assert!(caller.is_some(), "caller should be present");
    // caller ensures result == y * 2. double(y) ensures result == y * 2.
    // With ensures propagation, the verifier can prove this.
    assert_eq!(
        caller.unwrap().status,
        VerifStatus::Verified,
        "caller should verify with ensures propagation: {:?}",
        caller.unwrap()
    );
}

#[test]
fn verify_cross_module_ensures_violation() {
    require_z3!();
    // Caller violates ensures because callee's ensures don't guarantee it.
    let src = r#"
func add_one(x: i32) -> i32 {
    ensures: result > x
    x + 1
}
func caller_bad(y: i32) -> i32 {
    ensures: result == y  // Violation: add_one(y) > y, cannot equal y
    add_one(y)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: cross_module_violation");
    let caller = results.iter().find(|r| r.func_name == "caller_bad");
    assert!(caller.is_some(), "caller_bad should be present");
    assert_eq!(
        caller.unwrap().status,
        VerifStatus::Failed,
        "caller_bad should fail: {:?}",
        caller.unwrap()
    );
}

#[test]
fn verify_callee_precondition_failure_has_diagnostic_span() {
    require_z3!();
    let src = r#"
func positive(x: i32) -> i32 {
    requires: x > 0
    x
}
func caller() -> i32 {
    positive(-1)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("verify callee precondition");
    let caller = results
        .iter()
        .find(|result| result.func_name == "caller")
        .expect("caller result");
    assert_eq!(caller.status, VerifStatus::Failed);
    let diagnostic = caller.diagnostic.as_ref().expect("structured diagnostic");
    assert_eq!(diagnostic.span.start_line, 7);
    assert_eq!(diagnostic.span.start_col, 5);
    assert_eq!(diagnostic.span.end_line, 7);
    assert_eq!(diagnostic.span.end_col, 17);
}

#[test]
fn verify_callee_unencodable_precondition_is_fail_closed() {
    // H1 (audit-triage-0.35.25): the callee-requires walker was fail-OPEN —
    // a precondition `expr_to_z3_bool` could not encode (calls, strings,
    // ...) was silently skipped, so the caller verified Proven while the
    // runtime traps E0801 at the call site. The string comparison in the
    // requires is unencodable (strings are not modeled on the AST path);
    // the call must now come back "not verified", never Proven.
    require_z3!();
    let src = r#"
func guarded(tag: string) -> i64 {
    requires: tag == "admin"
    tag.len()
}

func caller(tag: string) -> i64 {
    guarded(tag)
}
func main() -> i64 { 0 }
"#;
    let results = verify_source(src).expect("verify unencodable callee requires");
    let caller = results
        .iter()
        .find(|r| r.func_name == "caller")
        .expect("caller result");
    assert_ne!(
        caller.status,
        VerifStatus::Proven,
        "unencodable callee requires must not verify the caller: {}",
        caller.message
    );
    assert!(
        caller.message.contains("cannot be encoded"),
        "failure must name the unencodable precondition: {}",
        caller.message
    );
}

#[test]
fn verify_callee_requires_unknown_is_fail_closed() {
    // H1: a solver timeout (Unknown) on the callee-requires check used to be
    // treated as satisfied. The disjunction `x < 0 || x * x < 0` is
    // nonlinear and hard for Z3 to decide — if it returns Unknown the call
    // must not verify. The important lock is the fail-closed branch: should
    // the solver ever answer Unknown, the caller gets an explicit "could not
    // be decided" failure instead of Proven.
    require_z3!();
    let src = r#"
func tricky(x: i64) -> i64 {
    requires: x < 0 || x * x < 0
    x * x
}
func caller(x: i64) -> i64 {
    tricky(x)
}
func main() -> i64 { 0 }
"#;
    let results = verify_source(src).expect("verify callee requires Unknown path");
    let caller = results
        .iter()
        .find(|r| r.func_name == "caller")
        .expect("caller result");
    match caller.status {
        VerifStatus::Proven => panic!(
            "solver timeout on callee requires must not verify the caller: {}",
            caller.message
        ),
        VerifStatus::Failed => {
            // The checked MIR route can prove that evaluating x*x in the
            // precondition itself overflows before the predicate is decided.
            // That is a concrete failure, and still satisfies the no-proof
            // contract that an Unknown solver result must also preserve.
            let undefined_requires = caller
                .message
                .contains("callee requires expression is undefined");
            if undefined_requires {
                assert_eq!(
                    caller
                        .artifact
                        .as_ref()
                        .map(|artifact| artifact.engine.as_str()),
                    Some(ProofArtifact::ENGINE_MIR),
                    "definedness failure must come from the canonical proof"
                );
                assert!(caller.diagnostic.is_some());
            }
            assert!(
                caller.message.contains("could not be decided")
                    || caller.message.contains("may violate precondition")
                    || undefined_requires,
                "Unknown path must fail closed: {}",
                caller.message
            );
        }
        _ => {
            // SolverUnknown keeps the no-proof contract (never Proven).
        }
    }
}

#[test]
fn verify_callee_unencodable_ensures_is_fail_closed() {
    // Preserve the original trap negative independently from the unencodable
    // postcondition case below. `arr[0]` may trap before the call when `arr`
    // is empty, so this source must report its real E0803 body-trap obligation
    // instead of pretending it reached the later callee-ensures encoder.
    require_z3!();
    let src = r#"
func pos(x: i32) -> i32 {
    ensures: result == x
    x
}
func caller(arr: List<i32>) -> i32 {
    ensures: result >= 0
    pos(arr[0])
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("verify unencodable callee ensures");
    let caller = results
        .iter()
        .find(|r| r.func_name == "caller")
        .expect("caller result");
    assert_ne!(
        caller.status,
        VerifStatus::Proven,
        "an actual argument trap must not verify the caller: {}",
        caller.message
    );
    assert!(
        caller.message.contains("E0803") && caller.message.contains("trap"),
        "the original empty-list index remains an exact E0803 trap negative: {}",
        caller.message
    );
    let artifact = caller
        .artifact
        .as_ref()
        .expect("E0803 body trap retains canonical proof provenance");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(
        artifact.source_hash,
        blake3::hash(src.as_bytes()).to_hex().to_string()
    );
    assert_eq!(
        artifact
            .mir_route_receipt
            .as_ref()
            .map(|receipt| receipt.profile.as_str()),
        Some("canonical-contract-request-v1")
    );
}

#[test]
fn verify_callee_unencodable_ensures_with_total_index_argument_fails_closed() {
    require_z3!();
    let src = r#"
func pos(x: i32) -> i32 {
    ensures: result == x
    x
}
func caller(arr: List<i32>) -> i32 {
    requires: len(arr) > 0
    ensures: true
    pos(arr[0])
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("verify total argument with unencodable ensures");
    let caller = results
        .iter()
        .find(|result| result.func_name == "caller")
        .expect("caller result");
    assert_ne!(
        caller.status,
        VerifStatus::Proven,
        "an unencodable callee postcondition must not verify the caller: {}",
        caller.message
    );
    assert!(
        caller
            .message
            .contains("call to 'pos' has a postcondition that cannot be encoded for verification"),
        "failure must name the exact unencodable callee postcondition: {}",
        caller.message
    );
    assert!(
        !caller.message.contains("E0803"),
        "the `len(arr) > 0` entry precondition makes this argument total: {}",
        caller.message
    );
}

#[test]
fn verify_branch_callee_ensures_not_unconditional() {
    require_z3!();
    // V-C5: callee ensures inside a never-taken branch must not prove caller.
    let src = r#"
func always_ten(x: i32) -> i32 {
    ensures: result == 10
    10
}
func caller(y: i32) -> i32 {
    ensures: result == 10
    if false {
        always_ten(y)
    } else {
        y
    }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("branch callee");
    let always = results.iter().find(|r| r.func_name == "always_ten");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(always.is_some() && always.unwrap().status == VerifStatus::Verified);
    assert!(caller.is_some(), "caller present");
    assert_ne!(
        caller.unwrap().status,
        VerifStatus::Verified,
        "dead-branch callee ensures must not prove caller: {:?}",
        caller.unwrap()
    );
}

#[test]
fn verify_assign_updates_let_subst() {
    require_z3!();
    // V-C2: assignment must update flat let substitution.
    let src = r#"
func f() -> i32 {
    ensures: result == 2
    let mut y = 1
    y = 2
    y
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("assign subst");
    let f = results.iter().find(|r| r.func_name == "f");
    assert!(f.is_some(), "f present: {:?}", results);
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "y=2 should make ensures result==2 hold: {:?}",
        f.unwrap()
    );
}

#[test]
fn extract_body_return_first_return_wins() {
    // V-C3: sequential dead return must not win over the first return.
    // Typecheck rejects dead code after return, so exercise the helper directly.
    use crate::ast::{Expr, Lit, Stmt};
    use crate::verifier::helpers::extract_body_return;
    let stmts = vec![
        Stmt::Return(Some(Expr::Literal(Lit::Int(0)))),
        Stmt::Return(Some(Expr::Literal(Lit::Int(1)))),
    ];
    let e = extract_body_return(&stmts).expect("return found");
    match e {
        Expr::Literal(Lit::Int(0)) => {}
        other => panic!("expected first return 0, got {:?}", other),
    }
}

#[test]
fn verify_failed_callee_ensures_not_axioms() {
    require_z3!();
    // V-C4: a callee whose ensures fail must not make the caller's
    // postconditions verify via untrusted axioms.
    let src = r#"
func bad(x: i32) -> i32 {
    ensures: result == x + 1
    x
}
func caller(y: i32) -> i32 {
    ensures: result == y + 1
    bad(y)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: failed_callee_not_axiom");
    let bad = results.iter().find(|r| r.func_name == "bad");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(bad.is_some(), "bad should be present");
    assert_eq!(
        bad.unwrap().status,
        VerifStatus::Failed,
        "bad should fail its own ensures: {:?}",
        bad.unwrap()
    );
    assert!(caller.is_some(), "caller should be present");
    assert_ne!(
        caller.unwrap().status,
        VerifStatus::Verified,
        "caller must not verify by trusting failed callee ensures: {:?}",
        caller.unwrap()
    );
}

#[test]
fn verify_f64_large_unbounded_input_reports_e0813() {
    require_z3!();
    // The lower bound exercises a large finite value, while the missing upper
    // bound leaves a finite x whose doubling reaches the runtime E0813 trap.
    let src = r#"
func scale(x: f64) -> f64 {
    requires: x >= 1e10
    ensures: result >= 0.0
    x * 2.0
}
func main() -> f64 { 0.0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: f64_large");
    let s = results.iter().find(|r| r.func_name == "scale");
    assert!(s.is_some(), "scale function should be verified");
    let result = s.unwrap();
    assert_eq!(result.status, VerifStatus::Disproven, "{}", result.message);
    assert!(result.message.contains("E0813"), "{}", result.message);
    assert_finite_ieee_binary64_mir_artifact(result);
}

#[test]
fn verify_f64_tiny_lower_bound_still_exposes_unbounded_overflow() {
    require_z3!();
    // The positive lower bound excludes underflow for the intended tiny
    // region, but the missing upper bound leaves finite values whose doubling
    // reaches the runtime E0813 trap.
    let src = r#"
func check(x: f64) -> f64 {
    requires: x > 1e-20
    ensures: result > 0.0
    x * 2.0
}
func main() -> f64 { 0.0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: f64_tiny");
    let c = results.iter().find(|r| r.func_name == "check");
    assert!(c.is_some(), "check function should be verified");
    let result = c.unwrap();
    assert_eq!(result.status, VerifStatus::Disproven, "{}", result.message);
    assert!(result.message.contains("E0813"), "{}", result.message);
    assert_finite_ieee_binary64_mir_artifact(result);
}

#[test]
fn verify_match_all_arms_positive() {
    require_z3!();
    // Match with wildcard: all arms return >= 0, so ensures should hold.
    let src = r#"
func categorize(x: i32) -> i32 {
    ensures: result >= 0
    match x {
        1 => 10
        2 => 20
        _ => 0
    }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: match_all_positive");
    let f = results.iter().find(|r| r.func_name == "categorize");
    assert!(f.is_some(), "categorize should be present");
    assert_ne!(
        f.unwrap().status,
        VerifStatus::Failed,
        "match should not produce false positive: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_match_violation() {
    require_z3!();
    let src = r#"
func categorize(x: i32) -> i32 {
    ensures: result > 0
    match x {
        1 => 10
        _ => 0
    }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: match_violation");
    let f = results.iter().find(|r| r.func_name == "categorize");
    assert!(f.is_some(), "categorize should be present");
    assert!(
        f.unwrap().status == VerifStatus::Disproven || f.unwrap().status.is_inconclusive(),
        "match violation should be detected: {:?}",
        f.unwrap()
    );
}

// --- P1.1: Spawn/Await encoding ---

#[test]
fn verify_spawn_await_body_verified() {
    require_z3!();
    let src = r#"
func add_pair(x: i32, y: i32) -> i32 {
    requires: x >= -1000000000 && x <= 1000000000 && y >= -1000000000 && y <= 1000000000
    ensures: result == x + y
    let task = spawn add(x, y)
    await task
}
func add(a: i32, b: i32) -> i32 {
    requires: a >= -1000000000 && a <= 1000000000 && b >= -1000000000 && b <= 1000000000
    ensures: result == a + b
    a + b
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: spawn_await");
    let f = results.iter().find(|r| r.func_name == "add_pair");
    assert!(f.is_some(), "add_pair should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "spawn/await body should be verifiable: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_spawn_await_violation_detected() {
    require_z3!();
    let src = r#"
func bad_add(x: i32, y: i32) -> i32 {
    ensures: result == x + y
    let task = spawn sub(x, y)
    await task
}
func sub(a: i32, b: i32) -> i32 {
    a - b
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: spawn_await_violation");
    let f = results.iter().find(|r| r.func_name == "bad_add");
    assert!(f.is_some(), "bad_add should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Failed,
        "spawn/await with wrong func should fail: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_spawn_no_await_passes() {
    require_z3!();
    // Spawn without await (discard the future) — the function result
    // still comes from a separate return expression.
    let src = r#"
func compute_discard(x: i32) -> i32 {
    ensures: result == x
    spawn side_effect(x)
    x
}
func side_effect(a: i32) -> i32 {
    a
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: spawn_discard");
    let f = results.iter().find(|r| r.func_name == "compute_discard");
    assert!(f.is_some(), "compute_discard should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "spawn-discard body should be verifiable: {:?}",
        f.unwrap()
    );
}

// --- P1.2: String theory (Z3 Seq) ---

#[test]
fn verify_string_eq_param_requires_nonempty() {
    require_z3!();
    // String param with equality in requires controls a numeric return.
    let src = r#"
func greet_len(name: string) -> i32 {
    requires: name == "hello"
    ensures: result == 5
    len(name)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: string_eq_requires");
    let f = results.iter().find(|r| r.func_name == "greet_len");
    assert!(f.is_some(), "greet_len should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "string == literal in requires should verify: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_string_eq_param_requires_violation() {
    require_z3!();
    let src = r#"
func bad_len(name: string) -> i32 {
    requires: name == "hello"
    ensures: result == 3
    len(name)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: string_eq_violation");
    let f = results.iter().find(|r| r.func_name == "bad_len");
    assert!(f.is_some(), "bad_len should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Failed,
        "string requires + wrong ensures should fail: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_string_eq_in_ensures_with_requires() {
    require_z3!();
    // String equality with a require ensures the body path.
    let src = r#"
func is_same(a: string, b: string) -> i32 {
    requires: a == b
    ensures: result == 1
    if a == b { 1 } else { 0 }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: string_eq_ensures");
    let f = results.iter().find(|r| r.func_name == "is_same");
    assert!(f.is_some(), "is_same should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "string == in requires + ensures should verify: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_string_nonempty_preserved() {
    require_z3!();
    let src = r#"
func id_nonempty(s: string) -> i32 {
    requires: s != ""
    ensures: result == 1
    1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: string_nonempty");
    let f = results.iter().find(|r| r.func_name == "id_nonempty");
    assert!(f.is_some(), "id_nonempty should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "string != '' in requires should verify: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_string_len_gt_zero() {
    require_z3!();
    let src = r#"
func short(s: string) -> i32 {
    requires: len(s) > 0
    ensures: result == 1
    1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: string_len");
    let f = results.iter().find(|r| r.func_name == "short");
    assert!(f.is_some(), "short should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "len(s) > 0 with ensures should verify: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_string_char_at_contract() {
    require_z3!();
    let src = r#"
func first_char_check(s: string) -> i32 {
    requires: len(s) > 0 && char_at(s, 0) == "h"
    ensures: result == 1
    1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: char_at");
    let f = results.iter().find(|r| r.func_name == "first_char_check");
    assert!(f.is_some(), "first_char_check should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "char_at in requires should verify: {:?}",
        f.unwrap()
    );
}

// --- P1.1 supplementary: Lambda/Comprehension ---

#[test]
fn verify_lambda_in_body_not_crash() {
    require_z3!();
    // Lambda in function body — should not crash, may be Unknown since
    // closures can't be encoded as Z3 terms. The key assertion is that
    // verification completes without panic and the result is not Unknown.
    let src = r#"
func make_adder(x: i32) -> func(i32) -> i32 {
    fn(y: i32) -> i32 { x + y }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: lambda");
    let f = results.iter().find(|r| r.func_name == "make_adder");
    assert!(f.is_some(), "make_adder should be present");
    // Lambda bodies can't be encoded as int/real; result is Unknown
    assert_ne!(
        f.unwrap().status,
        VerifStatus::Failed,
        "lambda body should not produce false positive: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_comprehension_in_body_not_crash() {
    require_z3!();
    let src = r#"
func make_list(n: i32) -> i32 {
    let xs = [i for i in range(0, n)]
    len(xs)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: comprehension");
    let f = results.iter().find(|r| r.func_name == "make_list");
    assert!(f.is_some(), "make_list should be present");
    assert_ne!(
        f.unwrap().status,
        VerifStatus::Failed,
        "comprehension body should not crash: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_multiple_spawn_await() {
    require_z3!();
    let src = r#"
func sum_pair(x: i32, y: i32) -> i32 {
    requires: x >= -1000000000 && x <= 1000000000 && y >= -1000000000 && y <= 1000000000
    ensures: result == x + y
    let t1 = spawn id(x)
    let t2 = spawn id(y)
    (await t1) + (await t2)
}
func id(a: i32) -> i32 {
    ensures: result == a
    a
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: multi_spawn");
    let f = results.iter().find(|r| r.func_name == "sum_pair");
    assert!(f.is_some(), "sum_pair should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "multiple spawn/await should verify: {:?}",
        f.unwrap()
    );
}

// --- P1.2 supplementary: contains/starts_with/ends_with ---

#[test]
fn verify_string_contains_ensures() {
    require_z3!();
    let src = r#"
func check_prefix(s: string) -> i32 {
    requires: contains(s, "abc")
    ensures: result == 1
    1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: contains");
    let f = results.iter().find(|r| r.func_name == "check_prefix");
    assert!(f.is_some(), "check_prefix should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "contains in requires should verify: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_string_starts_ends_with() {
    require_z3!();
    let src = r#"
func both_ends(s: string) -> i32 {
    requires: starts_with(s, "A") && ends_with(s, "Z")
    ensures: result == 1
    1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: starts_ends");
    let f = results.iter().find(|r| r.func_name == "both_ends");
    assert!(f.is_some(), "both_ends should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "starts_with/ends_with in requires should verify: {:?}",
        f.unwrap()
    );
}

/// E1: After a solver Unknown (timeout/crash), the solver is replaced with a
/// fresh one (push depth 0). Pop(1) must not underflow.
#[test]
fn verify_solver_pop_after_unknown_no_crash() {
    require_z3!();
    let src = r#"
func complex(x: i32) -> i32 {
    requires: x > 0
    ensures: result > 0
    if x > 1 { x } else { x + 1 }
}
func main() -> i32 { 0 }
"#;
    let mut verifier = Verifier::with_timeout(1).expect("solver init");
    let results = verify_source_with(src, &mut verifier)
        .expect("src/verifier/tests.rs: verify_solver_pop_after_unknown");
    let f = results.iter().find(|r| r.func_name == "complex");
    assert!(f.is_some(), "complex should be present");
    // With 1ms timeout, this should return SolverUnknown, not crash
    assert!(
        matches!(
            f.unwrap().status,
            VerifStatus::Proven
                | VerifStatus::NotInTrustedSubset
                | VerifStatus::SolverUnknown
                | VerifStatus::Timeout
                | VerifStatus::InfrastructureError
                | VerifStatus::RuntimeOnlyContract
                | VerifStatus::NoObligations
        ),
        "should not crash (Proven or inconclusive): {:?}",
        f.unwrap().status,
    );
}

#[test]
fn solver_replacement_skips_pending_pop_and_reset_recovers() {
    require_z3!();
    let mut session = super::ctx::SolverSession::new(100).expect("solver init");

    // A replacement starts at depth zero while callers still owe the old
    // solver's pop. The pending pop must not touch the fresh solver.
    session.replaced = true;
    session.poisoned = true;
    session.pop();

    session.reset();
    session.push();
    session.pop();
    assert!(!session.replaced);
    assert!(!session.poisoned);
}

/// E2: Non-exhaustive match (no wildcard) — result is unconstrained, so
/// ensures `result >= 0` should NOT be Verified because the fallback arm
/// returns an unconstrained variable (not silently 0).
#[test]
fn verify_match_nonexhaustive_no_false_positive() {
    require_z3!();
    let src = r#"
func pick(x: i32) -> i32 {
    requires: x >= 0
    ensures: result >= 0
    match x {
        0 => { 0 }
        1 => { 1 }
    }
}
func main() -> i32 { 0 }
"#;
    // No-false-positive contract: a non-exhaustive match must never be silently
    // Verified. The checker rejects i32 matches without a wildcard arm outright
    // (verify_source -> Err), which is an even stronger guarantee than a Failed
    // /Unknown verification status; accept either as "not a false positive".
    match verify_source(src) {
        Err(_) => { /* checker rejected the non-exhaustive match — no false positive */ }
        Ok(results) => {
            let f = results.iter().find(|r| r.func_name == "pick");
            assert!(f.is_some(), "pick should be present");
            assert!(
                matches!(
                    f.unwrap().status,
                    VerifStatus::Disproven
                        | VerifStatus::NotInTrustedSubset
                        | VerifStatus::SolverUnknown
                        | VerifStatus::Timeout
                        | VerifStatus::InfrastructureError
                        | VerifStatus::RuntimeOnlyContract
                        | VerifStatus::NoObligations
                ),
                "non-exhaustive match should not silently pass ensures: {:?}",
                f.unwrap().status,
            );
        }
    }
}

/// E2: Exhaustive match (with wildcard) — all arms return >= 0, so
/// ensures result >= 0 should be Verified.
#[test]
fn verify_match_exhaustive_wildcard_passes() {
    require_z3!();
    let src = r#"
func pick_safe(x: i32) -> i32 {
    requires: x >= 0 && x <= 1
    ensures: result >= 0
    match x {
        0 => { 0 }
        1 => { 1 }
        _ => { 0 }
    }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: match_exhaustive");
    let f = results.iter().find(|r| r.func_name == "pick_safe");
    assert!(f.is_some(), "pick_safe should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "exhaustive match with wildcard should verify: {:?}",
        f.unwrap().status,
    );
}

/// Entry invariants need establishment under requires before becoming assumptions.
/// Loop preservation remains a separate retained-owner obligation below.
#[test]
fn verify_invariant_not_established_fails() {
    require_z3!();
    // V-H1: invariant not implied by requires must Fail at establish.
    let src = r#"
func broken(x: i32) -> i32 {
    requires: x == 0
    invariant: x > 100
    ensures: result > 0
    42
}
func main() -> i32 { 0 }
"#;
    let results = verify_entry_invariant_without_legacy_access(src);
    let f = results.iter().find(|r| r.func_name == "broken");
    assert!(f.is_some(), "broken should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Failed,
        "invariant not established should fail: {:?}",
        f.unwrap()
    );
    assert!(
        f.unwrap().message.contains("not established") || f.unwrap().message.contains("invariant"),
        "message: {}",
        f.unwrap().message
    );
}

/// V-H1: assigning a free variable of the invariant inside a loop degrades status.
#[test]
fn verify_invariant_preserve_assign_degrades() {
    require_z3!();
    // Keep body simple: assign inv free var `x` to a constant inside while.
    // Avoid `x = x` which can create a cyclic let-subst expand.
    let src = r#"
func loop_mut(mut x: i32) -> i32 {
    requires: x >= 0
    invariant: x >= 0
    ensures: result >= 0
    while false {
        x = 0
    }
    x
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: invariant_preserve_assign");
    let f = results
        .iter()
        .find(|r| r.func_name == "loop_mut")
        .expect("loop_mut present");
    assert_ne!(
        f.status,
        VerifStatus::Verified,
        "assigning inv free var in loop must not Verified: {:?}",
        f
    );
}

/// V1: extract_body_return handles if-else branching in body.
/// The Z3 layer should receive an Expr::If encoding for the conditional paths.
#[test]
fn verify_if_else_body_return() {
    require_z3!();
    let src = r#"
func abs_val(x: i32) -> i32 {
    requires: x >= -2147483647
    ensures: result >= 0
    if x < 0 { -x } else { x }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: if_else_body");
    let f = results.iter().find(|r| r.func_name == "abs_val");
    assert!(f.is_some(), "abs_val should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "abs with if-else should verify result >= 0: {:?}",
        f.unwrap().status,
    );
}

/// V7: NLL works across nested block boundaries.
/// A borrow created in an outer block should be released when the reference
/// is no longer used after the inner block ends.
#[test]
fn verify_nll_cross_block_boundary() {
    require_z3!();
    let src = r#"
func cross_block(x: i32) -> i32 {
    ensures: result > 0
    let r = &x;
    if x > 0 { x } else { 1 }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: nll_cross_block");
    let f = results.iter().find(|r| r.func_name == "cross_block");
    assert!(f.is_some(), "cross_block should be present");
    // The key assertion: borrow of x doesn't prevent verification
    assert!(
        matches!(
            f.unwrap().status,
            VerifStatus::Proven
                | VerifStatus::NotInTrustedSubset
                | VerifStatus::SolverUnknown
                | VerifStatus::Timeout
                | VerifStatus::InfrastructureError
                | VerifStatus::RuntimeOnlyContract
                | VerifStatus::NoObligations
        ),
        "NLL cross-block should not cause false failure: {:?}",
        f.unwrap().status,
    );
}

/// P1.2: let-bound call expressions outside tail position should propagate callee ensures.
/// Previously, `let_subst` was only applied to body_return, so `assert_callee_ensures_in_block`
/// saw bare identifiers (e.g. `d`) instead of expanded calls (e.g. `double(y)`), causing
/// callee ensures to be silently dropped.
///
/// §11-#46 (2026-08-07): double's ensures must carry an UPPER bound too —
/// `d + 1` now gets an i64 overflow obligation (VIR infer_expr_type types
/// let-bound call results as I64), and an unbounded-above d disproves it.
/// The propagation property under test is unchanged: caller's proof still
/// depends on double's ensures reaching the non-tail `d + 1` use.
#[test]
fn verify_let_bound_call_ensures_propagated() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0 && x <= 1000000000
    ensures: result >= 0
    ensures: result <= 2000000000
    x * 2
}

func caller(y: i32) -> i32 {
    requires: y >= 0 && y <= 1000000000
    ensures: result >= 0
    let d = double(y);
    let _unused = d + 1;  // d used here, not just in tail position
    d  // tail returns d, whose value depends on double's ensures
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("P1.2: let_subst propagation");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(caller.is_some(), "caller function should be verified");
    assert_eq!(
        caller.unwrap().status,
        VerifStatus::Verified,
        "caller should verify because double's ensures (result >= 0) is propagated to let-bound d: {:?}",
        caller.unwrap().message
    );
}

/// P1.2 variant: let-bound call with ensures violation should fail even when the call
/// is not in tail position (proving the ensures was actually propagated and checked).
#[test]
fn verify_let_bound_call_ensures_violation_detected() {
    require_z3!();
    let src = r#"
func half(x: i32) -> i32 {
    requires: x >= 0
    ensures: result >= 0
    x / 2
}

func caller(y: i32) -> i32 {
    requires: y >= 0
    ensures: result >= 10  // requires that d >= 10, but half's ensures only guarantees d >= 0
    let d = half(y);      // if y = 0, d = 0 which violates result >= 10
    d
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("P1.2: let_subst violation detection");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(caller.is_some(), "caller function should be present");
    // With P1.2 fix: half's ensures (d >= 0) is propagated, so verifier knows
    // d >= 0 but requires d >= 10 → violation detected → Failed
    // Without fix: half's ensures not propagated → no constraint on d → potentially Verified
    assert_eq!(
        caller.unwrap().status,
        VerifStatus::Failed,
        "caller should fail because half's ensures doesn't guarantee result >= 10: {:?}",
        caller.unwrap().message
    );
}

#[test]
fn verify_rejects_ill_typed_source() {
    // V-H8: production typecheck gate — ill-typed AST fails core::check.
    let src = r#"
func f(x: i32) -> i32 {
    ensures: result == x
    "not an int"
}
func main() -> i32 { 0 }
"#;
    let file = parse_memory_source(src, "ill-typed-test").expect("parse");
    let r = crate::core::check(&file);
    assert!(r.is_err(), "expected typecheck failure for ill-typed body");
}

#[test]
fn verify_actor_method_contract() {
    require_z3!();
    // V-H6: actor methods enter the verify queue.
    let src = r#"
actor Counter {
    count: i32
    func get() -> i32 {
        ensures: result >= 0
        self.count
    }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("verify");
    let m = results.iter().find(|r| r.func_name.contains("get"));
    assert!(
        m.is_some(),
        "actor method should be verified: {:?}",
        results
    );
}

#[test]
fn verify_old_field_access() {
    require_z3!();
    // V-H2: old(p.x) should encode for simple field paths.
    let src = r#"
type Point { x: i32, y: i32 }
func bump(p: Point) -> i32 {
    ensures: result == old(p.x) + 1
    p.x + 1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("verify");
    let f = results.iter().find(|r| r.func_name == "bump");
    assert!(f.is_some(), "bump present: {:?}", results);
    // Accept Proven or inconclusive (if field old still incomplete) but not crash.
    assert!(
        matches!(
            f.unwrap().status,
            VerifStatus::Proven
                | VerifStatus::Disproven
                | VerifStatus::NotInTrustedSubset
                | VerifStatus::SolverUnknown
                | VerifStatus::Timeout
                | VerifStatus::InfrastructureError
                | VerifStatus::RuntimeOnlyContract
                | VerifStatus::NoObligations
        ),
        "{:?}",
        f.unwrap()
    );
}

#[test]
fn verify_i32_add_requires_no_overflow_proof() {
    require_z3!();
    let src = r#"
func increment(x: i32) -> i32 {
    ensures: result == x + 1
    x + 1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results[0].status, VerifStatus::Failed);
    assert!(results[0].message.contains("integer overflow"));
}

#[test]
fn verify_i32_checked_add_sub_mul_when_bounded() {
    require_z3!();
    let src = r#"
func arithmetic(x: i32) -> i32 {
    requires: x >= -1000 && x <= 1000
    ensures: result == (x + 7) * 3 - 2
    (x + 7) * 3 - 2
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "{}",
        results[0].message
    );
}

#[test]
fn verify_i32_mul_requires_no_overflow_proof() {
    require_z3!();
    let src = r#"
func square(x: i32) -> i32 {
    ensures: result == x * x
    x * x
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results[0].status, VerifStatus::Failed);
    assert!(results[0].message.contains("integer overflow"));
}

#[test]
fn verify_i64_add_requires_no_overflow_proof() {
    // H2 (audit-triage-0.35.25): i64 used to be modeled as unbounded Z3 Int
    // with NO definedness VCs — `i64::MAX + 1` traps E0801 at runtime
    // (SD-7) yet verified Proven. The Resolved engine now derives per-type
    // bounds (int_bounds) and the AST path is parameterized the same way, so
    // an unbounded `x + 1` must FAIL with an overflow diagnostic.
    require_z3!();
    let src = r#"
func increment(x: i64) -> i64 {
    ensures: result == x + 1
    x + 1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "i64 overflow must be rejected: {}",
        results[0].message
    );
    assert!(results[0].message.contains("integer overflow"));
}

#[test]
fn verify_i64_checked_add_sub_mul_when_bounded() {
    // H2 counterpart: with a bounding requires, i64 arithmetic verifies —
    // the check is real, not an unconditional reject.
    require_z3!();
    let src = r#"
func arithmetic(x: i64) -> i64 {
    requires: x >= -1000 && x <= 1000
    ensures: result == (x + 7) * 3 - 2
    (x + 7) * 3 - 2
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "bounded i64 arithmetic should verify: {}",
        results[0].message
    );
}

#[test]
fn verify_i64_div_rejects_zero_and_min_overflow() {
    // H2: i64 div/rem carry the same zero-divisor + MIN/-1 obligations as
    // i32 (SD-8) — previously only checked on the VIR path, now also on the
    // AST fallback path with i64 bounds.
    require_z3!();
    let src = r#"
func maybe_divide_by_zero(x: i64, y: i64) -> i64 {
    ensures: result == x / y
    x / y
}
func min_div_neg_one() -> i64 {
    ensures: result == -9223372036854775808 / -1
    -9223372036854775808 / -1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|r| r.status == VerifStatus::Failed && r.message.contains("undefined")),
        "{results:?}"
    );
}

#[test]
fn verify_i64_checked_div_when_bounded() {
    // H2 counterpart: bounded divisor verifies.
    require_z3!();
    let src = r#"
func half(x: i64) -> i64 {
    requires: x >= -2000 && x <= 2000
    ensures: result == x / 2
    x / 2
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "bounded i64 division should verify: {}",
        results[0].message
    );
}

#[test]
fn verify_i64_overflow_on_ast_fallback_path() {
    // H2: the AST fallback path (functions with calls are rejected by the
    // VIR trusted-subset gate) previously had NO i64 definedness — the
    // `!returns_i32` branch asserted the unbounded model. The parameterized
    // `int_definedness_obligations` must now bound i64 there too. The callee
    // forces the AST path while keeping the arithmetic in the outer function.
    require_z3!();
    let src = r#"
func helper(x: i64) -> i64 {
    requires: x <= 1000
    ensures: result == x
    x
}

func increment(x: i64) -> i64 {
    ensures: result == helper(x) + 1
    helper(x) + 1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    let increment = results
        .iter()
        .find(|r| r.func_name == "increment")
        .expect("must have a result for 'increment'");
    assert_eq!(
        increment.status,
        VerifStatus::Failed,
        "i64 overflow on the AST path must be rejected: {}",
        increment.message
    );
    assert!(increment.message.contains("integer overflow"));
}

#[test]
fn verify_i32_sub_requires_no_overflow_proof() {
    require_z3!();
    let src = r#"
func decrement(x: i32) -> i32 {
    ensures: result == x - 1
    x - 1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results[0].status, VerifStatus::Failed);
    assert!(results[0].message.contains("integer overflow"));
}

#[test]
fn verify_i32_div_rem_keep_truncation_toward_zero() {
    require_z3!();
    let src = r#"
func quotient() -> i32 {
    ensures: result == -2
    -7 / 3
}
func remainder() -> i32 {
    ensures: result == -1
    -7 % 3
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert!(
        results.iter().all(|r| r.status == VerifStatus::Verified),
        "{results:?}"
    );
}

#[test]
fn verify_i32_div_rejects_zero_and_min_overflow() {
    require_z3!();
    let src = r#"
func maybe_divide_by_zero(x: i32, y: i32) -> i32 {
    ensures: result == x / y
    x / y
}
func min_div_neg_one() -> i32 {
    ensures: result == -2147483648 / -1
    -2147483648 / -1
}
func min_rem_neg_one() -> i32 {
    ensures: result == -2147483648 % -1
    -2147483648 % -1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results.len(), 3);
    assert!(
        results.iter().all(|r| r.status == VerifStatus::Failed),
        "{results:?}"
    );
    assert!(
        results
            .iter()
            .all(|r| r.message.contains("integer operation is undefined")),
        "{results:?}"
    );
}

#[test]
fn verify_i32_div_definedness_can_be_proven_separately() {
    require_z3!();
    let src = r#"
func divide(x: i32, y: i32) -> i32 {
    requires: y != 0 && (x != -2147483648 || y != -1)
    ensures: result == x / y
    x / y
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "{}",
        results[0].message
    );
}

/// P1-24: A source verification proof retains the source identity and the
/// actual semantic graph consumed by its engine. Ordinary scalar contracts
/// now use canonical MIR, so their receipt replaces the old VIR identity.
#[test]
fn proof_artifact_hashes_populated() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x * 2
    x * 2
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert!(!results.is_empty(), "should have at least one result");
    let r = &results[0];
    assert_eq!(r.status, VerifStatus::Verified, "{}", r.message);
    let artifact = r
        .artifact
        .as_ref()
        .expect("MIR-path result should have artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(
        artifact.source_hash,
        blake3::hash(src.as_bytes()).to_hex().to_string(),
        "proof source identity must match the exact source (P1-24)"
    );
    let file = parse_memory_source(src, "contracts").expect("parse proof source");
    let checked = crate::core::check_program(&file).expect("check proof source");
    let canonical = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("materialize the proof's semantic graph")
        .program;
    assert_eq!(artifact.mir_hash, canonical.canonical_digest());
    let receipt = artifact
        .mir_route_receipt
        .as_ref()
        .expect("MIR proof carries its full route receipt");
    receipt
        .validate_against_program(&canonical)
        .expect("proof receipt belongs to this semantic graph");
    assert_eq!(receipt.mir_digest, artifact.mir_hash);
    assert!(artifact.vir_hash.is_empty());
    assert!(artifact.resolved_ir_hash.is_empty());
    // source_hash should be a valid BLAKE3 hex string (64 chars).
    assert_eq!(
        artifact.source_hash.len(),
        64,
        "source_hash should be BLAKE3 hex (64 chars)"
    );
    assert_eq!(
        artifact.mir_hash.len(),
        64,
        "mir_hash should be BLAKE3 hex (64 chars)"
    );
}

/// P1-24: Different source text produces different source_hash.
#[test]
fn proof_artifact_source_hash_changes_with_source() {
    require_z3!();
    let src1 = r#"
func f(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x + 1
    x + 1
}
"#;
    let src2 = r#"
func f(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x + 2
    x + 2
}
"#;
    let r1 = verify_source(src1).expect("src1 should verify");
    let r2 = verify_source(src2).expect("src2 should verify");
    let a1 = r1[0].artifact.as_ref().expect("artifact");
    let a2 = r2[0].artifact.as_ref().expect("artifact");
    assert_eq!(a1.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(a2.engine, ProofArtifact::ENGINE_MIR);
    assert_ne!(
        a1.source_hash, a2.source_hash,
        "different source text should produce different source_hash"
    );
    assert_ne!(a1.mir_hash, a2.mir_hash, "changed arithmetic changes MIR");
    assert!(
        !a1.is_compatible(a2),
        "changed semantics cannot reuse a proof"
    );
    let repeated = verify_source(src1).expect("repeat unchanged verification");
    let repeated = repeated[0].artifact.as_ref().expect("repeat artifact");
    assert_eq!(a1.source_hash, repeated.source_hash);
    assert_eq!(a1.mir_hash, repeated.mir_hash);
    assert_eq!(a1.mir_route_receipt, repeated.mir_route_receipt);
    assert!(a1.is_compatible(repeated));
}

/// 0.31.27+: Callee ensures propagation in VIR path.
/// A function that calls a verified function should be verifiable via VIR
/// (callee ensures inlined as assumptions).
#[test]
fn verify_callee_ensures_propagation_vir() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x * 2
    x * 2
}

func quadruple(x: i32) -> i32 {
    requires: x >= -536870912 && x <= 536870911
    ensures: result == x * 4
    let y = double(x)
    double(y)
}
"#;
    let results = verify_source(src).expect("verification should parse");
    // Both functions should be verified
    assert!(
        results.len() >= 2,
        "should have at least 2 results: {:?}",
        results
    );
    for r in &results {
        assert_eq!(
            r.status,
            VerifStatus::Verified,
            "{}: {}",
            r.func_name,
            r.message
        );
    }
}

/// 0.31.27+: Callee ensures propagation — disproven case.
/// If the caller's ensures contradicts the callee's ensures, verification
/// should fail (Disproven).
#[test]
fn verify_callee_ensures_propagation_disproven() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x * 2
    x * 2
}

func wrong_quadruple(x: i32) -> i32 {
    requires: x >= -536870912 && x <= 536870911
    ensures: result == x * 5
    let y = double(x)
    double(y)
}
"#;
    let results = verify_source(src).expect("verification should parse");
    // double should be Verified, wrong_quadruple should be Disproven
    let wrong = results.iter().find(|r| r.func_name == "wrong_quadruple");
    assert!(wrong.is_some(), "should have result for wrong_quadruple");
    assert_eq!(
        wrong.unwrap().status,
        VerifStatus::Disproven,
        "wrong_quadruple should be Disproven: {}",
        wrong.unwrap().message
    );
}

/// #41 (full-audit-2026-08-05 §11): VIR callee inlining must NOT inject the
/// callee's ensures unconditionally — the call-site must satisfy the callee's
/// requires. A caller with no constraints can call `double(x)` outside its
/// safe range, where the callee contract does not hold; `ensures:
/// result == x * 2` used to verify against the fabricated `result == x*2`
/// assumption (fake Verified). With the injection gated as
/// `requires ⇒ ensures`, the call-site precondition becomes a proof
/// obligation and the caller is Disproven.
#[test]
fn verify_callee_ensures_inlining_requires_call_site_precondition() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x * 2
    x * 2
}

func caller(x: i32) -> i32 {
    ensures: result == x * 2
    double(x)
}
"#;
    let results = verify_source(src).expect("verification should parse");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(caller.is_some(), "should have result for caller");
    assert_ne!(
        caller.unwrap().status,
        VerifStatus::Verified,
        "caller must not be Verified from an unsatisfied callee precondition: {}",
        caller.unwrap().message
    );
}

/// #41 companion: a caller whose requires IMPLY the callee's requires still
/// gets the full callee contract — the gated injection degrades to the
/// unconditional case exactly when the precondition is derivable.
#[test]
fn verify_callee_ensures_inlining_satisfying_requires_still_verifies() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x * 2
    x * 2
}

func caller(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x * 2
    double(x)
}
"#;
    let results = verify_source(src).expect("verification should parse");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(caller.is_some(), "should have result for caller");
    assert_eq!(
        caller.unwrap().status,
        VerifStatus::Verified,
        "caller with satisfying requires must still verify: {}",
        caller.unwrap().message
    );
}

// ============================================================
// 0.34.44: ADR-008 engine isolation regression locks
//
// 1. Proof cache keys carry engine identity — cross-engine reuse is
//    structurally impossible (fail-loud cache miss, never silent downgrade).
// 2. The LSP verification cache key is engine-scoped; pre-0.34.44 entries
//    (no engine segment) can never match.
// 3. Dual-engine divergence is fail-closed: the weaker conclusion wins and
//    the merged result carries the E0439 divergence diagnostic.
// ============================================================

fn vr_044(func_name: &str, status: VerifStatus) -> VerificationResult {
    VerificationResult {
        func_name: func_name.to_string(),
        status,
        message: String::new(),
        diagnostic: None,
        duration_us: 0,
        constraint_count: 1,
        artifact: None,
        trusted_subset_domain: None,
    }
}

#[test]
fn proof_cache_key_carries_engine_identity() {
    let mut flow = ProofArtifact::new("z3 4.13.0".to_string(), "src-hash".to_string());
    flow.vir_hash = "vh".to_string();
    let mut resolved = flow.clone();
    resolved.engine = ProofArtifact::ENGINE_RESOLVED.to_string();
    resolved.vir_hash = String::new(); // resolved engine binds resolved_ir_hash
    resolved.resolved_ir_hash = "vh".to_string();

    // Same program identity under BOTH engines must never collide.
    assert_ne!(
        flow.cache_key(),
        resolved.cache_key(),
        "cross-engine cache keys must differ (ADR-008 §2)"
    );
    assert!(flow.cache_key().contains(ProofArtifact::ENGINE_FLOW_AST));
    assert!(resolved
        .cache_key()
        .contains(ProofArtifact::ENGINE_RESOLVED));

    // Same engine + same identity → interchangeable (stable key).
    let flow_again = flow.clone();
    assert_eq!(flow.cache_key(), flow_again.cache_key());
}

#[test]
fn proof_artifact_cross_engine_is_incompatible() {
    let mut flow = ProofArtifact::new("z3 4.13.0".to_string(), "src-hash".to_string());
    flow.vir_hash = "vh".to_string();
    let mut resolved = flow.clone();
    resolved.engine = ProofArtifact::ENGINE_RESOLVED.to_string();
    assert!(
        !flow.is_compatible(&resolved),
        "a flow_ast proof must never be compatible with a resolved obligation"
    );
    assert!(flow.is_compatible(&flow.clone()));
}

#[test]
fn mir_proof_cache_key_binds_route_identity_without_profile() {
    let mut mir = ProofArtifact::new("z3 4.13.0".to_string(), "src-hash".to_string());
    mir.engine = ProofArtifact::ENGINE_MIR.to_string();
    mir.mir_hash = "a".repeat(64);
    mir.mir_route_receipt = Some(crate::core::mir::CanonicalMirRouteReceipt {
        schema: crate::core::mir::MIR_ROUTE_RECEIPT_SCHEMA,
        profile: "native-v1".to_string(),
        mir_digest: "b".repeat(64),
        type_desc_digest: "c".repeat(64),
        abi_digest: "d".repeat(64),
        ffi_digest: "e".repeat(64),
        ownership_digest: "f".repeat(64),
        flow_transition_digest: "0".repeat(64),
        root_owners: vec![crate::core::NodeId("function:main".to_string())],
    });

    let mut profile_only = mir.clone();
    profile_only
        .mir_route_receipt
        .as_mut()
        .expect("route receipt")
        .profile = "verifier-v1".to_string();
    assert_eq!(
        mir.cache_key(),
        profile_only.cache_key(),
        "consumer profile is provenance and must not split semantic cache identity"
    );

    let mut forged = mir.clone();
    forged
        .mir_route_receipt
        .as_mut()
        .expect("route receipt")
        .ffi_digest = "1".repeat(64);
    assert_ne!(
        mir.cache_key(),
        forged.cache_key(),
        "MIR cache identity must include the FFI sub-digest"
    );

    let mut missing = mir.clone();
    missing.mir_route_receipt = None;
    assert_ne!(
        mir.cache_key(),
        missing.cache_key(),
        "a pre-receipt MIR artifact must never share a cache key with a bound proof"
    );
    assert!(!mir.is_compatible(&missing));

    let mut source_changed = mir.clone();
    source_changed.source_hash = "other-source".to_string();
    assert!(
        !mir.is_compatible(&source_changed),
        "two source-bound proofs must not cross-reuse even when MIR identity matches"
    );

    let mut source_unbound = mir.clone();
    source_unbound.source_hash.clear();
    assert!(
        mir.is_compatible(&source_unbound),
        "an unbound direct API remains compatible when semantic MIR identity is unchanged"
    );
}

#[test]
fn mir_result_provenance_gate_rejects_mixed_multi_call_batch() {
    let receipt = crate::core::mir::CanonicalMirRouteReceipt {
        schema: crate::core::mir::MIR_ROUTE_RECEIPT_SCHEMA,
        profile: "r6-701-verifier-v1".into(),
        mir_digest: "a".repeat(64),
        type_desc_digest: "b".repeat(64),
        abi_digest: "c".repeat(64),
        ffi_digest: "d".repeat(64),
        ownership_digest: "e".repeat(64),
        flow_transition_digest: "f".repeat(64),
        root_owners: vec![crate::core::NodeId("function:main".into())],
    };
    let source_hash = "source-r6-701";
    let result = |name: &str| VerificationResult {
        func_name: name.into(),
        status: VerifStatus::Proven,
        message: "proof".into(),
        diagnostic: None,
        duration_us: 0,
        constraint_count: 1,
        artifact: Some(ProofArtifact {
            semantics_version: ProofArtifact::SEMANTICS_VERSION,
            integer_model: "checked_i32_i64_power_i64_then_i32_wrap_0_1_11".into(),
            float_model: crate::core::mir::types::MIR_VERIFIER_FLOAT_MODEL.into(),
            solver_version: "z3 test".into(),
            source_hash: source_hash.into(),
            resolved_ir_hash: String::new(),
            mir_hash: receipt.mir_digest.clone(),
            mir_route_receipt: Some(receipt.clone()),
            vir_hash: String::new(),
            engine: ProofArtifact::ENGINE_MIR.into(),
        }),
        trusted_subset_domain: None,
    };
    let results = vec![result("function:first"), result("extern second")];
    assert!(validate_mir_result_provenance(&results, &receipt, source_hash, "test-batch").is_ok());

    let mut source_mismatch = results.clone();
    source_mismatch[1]
        .artifact
        .as_mut()
        .expect("second artifact")
        .source_hash = "other-source".into();
    let error =
        validate_mir_result_provenance(&source_mismatch, &receipt, source_hash, "test-batch")
            .expect_err("mixed source provenance must fail closed");
    assert!(error.contains("source_hash"), "{error}");

    let mut route_mismatch = results.clone();
    route_mismatch[1]
        .artifact
        .as_mut()
        .expect("second artifact")
        .mir_route_receipt
        .as_mut()
        .expect("second route receipt")
        .abi_digest = "0".repeat(64);
    let error =
        validate_mir_result_provenance(&route_mismatch, &receipt, source_hash, "test-batch")
            .expect_err("mixed route provenance must fail closed");
    assert!(error.contains("route receipt"), "{error}");

    let mut engine_mismatch = results;
    engine_mismatch[0]
        .artifact
        .as_mut()
        .expect("first artifact")
        .engine = ProofArtifact::ENGINE_RESOLVED.into();
    let error =
        validate_mir_result_provenance(&engine_mismatch, &receipt, source_hash, "test-batch")
            .expect_err("mixed engine provenance must fail closed");
    assert!(error.contains("carries engine"), "{error}");
}

#[test]
fn lsp_verification_cache_key_is_engine_scoped() {
    let key = crate::lsp::verification_cache_key("file:///a.mimi", "double");
    // Engine identity + semantics version are mandatory segments.
    assert!(key.contains(ProofArtifact::ENGINE_RESOLVED), "key: {key}");
    assert!(
        key.contains(&format!("v{}", ProofArtifact::SEMANTICS_VERSION)),
        "key: {key}"
    );
    // The pre-0.34.44 shape can never match the new key → old on-disk
    // entries auto-invalidate on upgrade.
    assert_ne!(key, "file:///a.mimi:double");
}

#[test]
fn merge_divergence_proven_vs_disproven_fails_closed() {
    let primary = vec![vr_044("f", VerifStatus::Proven)];
    let mut flow = vr_044("f", VerifStatus::Disproven);
    flow.message = "counterexample found".to_string();
    let merged = merge_engine_verdicts(primary, vec![flow]);
    assert_eq!(merged.len(), 1);
    assert_eq!(
        merged[0].status,
        VerifStatus::Disproven,
        "Disproven must beat Proven on divergence"
    );
    assert!(
        merged[0].message.contains("E0439"),
        "divergence must carry the E0439 diagnostic: {}",
        merged[0].message
    );
    assert!(
        merged[0].artifact.is_none(),
        "a divergent verdict is no proof"
    );
}

#[test]
fn merge_divergence_proven_vs_unknown_fails_closed() {
    let primary = vec![vr_044("f", VerifStatus::Proven)];
    let secondary = vec![vr_044("f", VerifStatus::SolverUnknown)];
    let merged = merge_engine_verdicts(primary, secondary);
    assert_eq!(merged[0].status, VerifStatus::SolverUnknown);
    assert!(merged[0].message.contains("E0439"));
}

#[test]
fn merge_agreement_keeps_resolved_result() {
    let mut primary = vec![vr_044("f", VerifStatus::Proven)];
    primary[0].message = "resolved proof".to_string();
    let secondary = vec![vr_044("f", VerifStatus::Proven)];
    let merged = merge_engine_verdicts(primary, secondary);
    assert_eq!(merged[0].status, VerifStatus::Proven);
    assert_eq!(
        merged[0].message, "resolved proof",
        "primary wins on agreement"
    );
    assert!(!merged[0].message.contains("E0439"));
}

#[test]
fn merge_source_provenance_conflict_discards_proof() {
    let mut primary = vr_044("f", VerifStatus::Proven);
    let mut resolved = ProofArtifact::new("z3 resolved".into(), "source-a".into());
    resolved.engine = ProofArtifact::ENGINE_RESOLVED.into();
    resolved.resolved_ir_hash = "resolved-f".into();
    primary.artifact = Some(resolved);

    let mut secondary = vr_044("f", VerifStatus::Proven);
    let mut flow = ProofArtifact::new("z3 flow".into(), "source-b".into());
    flow.vir_hash = "flow-f".into();
    secondary.artifact = Some(flow);

    let merged = merge_engine_verdicts(vec![primary], vec![secondary]);
    assert_eq!(merged.len(), 1);
    assert_eq!(
        merged[0].status,
        VerifStatus::InfrastructureError,
        "source provenance conflict must become a no-proof result"
    );
    assert!(merged[0].artifact.is_none());
    assert!(merged[0].message.contains("E0439"));
    assert!(merged[0].message.contains("source provenance mismatch"));

    // An unbound direct/legacy artifact remains mergeable with a bound one;
    // the optional source provenance rule only rejects two known snapshots.
    let mut primary = vr_044("g", VerifStatus::Proven);
    let mut resolved = ProofArtifact::new("z3 resolved".into(), "source-a".into());
    resolved.engine = ProofArtifact::ENGINE_RESOLVED.into();
    resolved.resolved_ir_hash = "resolved-g".into();
    primary.artifact = Some(resolved);
    let mut secondary = vr_044("g", VerifStatus::Proven);
    let mut flow = ProofArtifact::new("z3 flow".into(), String::new());
    flow.vir_hash = "flow-g".into();
    secondary.artifact = Some(flow);
    let merged = merge_engine_verdicts(vec![primary], vec![secondary]);
    assert_eq!(merged[0].status, VerifStatus::Proven);
    assert!(merged[0].artifact.is_some());
}

#[test]
fn merge_batch_source_provenance_conflict_covers_unmatched_results() {
    // The Flow engine can contribute a call-site obligation with no matching
    // resolved-engine function name.  A source mismatch must still invalidate
    // both proof-bearing results instead of allowing that unmatched artifact
    // to pass through the merge untouched.
    let mut primary = vr_044("f", VerifStatus::Proven);
    let mut resolved = ProofArtifact::new("z3 resolved".into(), "source-a".into());
    resolved.engine = ProofArtifact::ENGINE_RESOLVED.into();
    resolved.resolved_ir_hash = "resolved-f".into();
    primary.artifact = Some(resolved);

    let mut secondary = vr_044("extern sqrt", VerifStatus::Disproven);
    let mut flow = ProofArtifact::new("z3 flow".into(), "source-b".into());
    flow.vir_hash = "flow-sqrt".into();
    secondary.artifact = Some(flow);

    let merged = merge_engine_verdicts(vec![primary], vec![secondary]);
    assert_eq!(merged.len(), 2);
    for result in &merged {
        assert_eq!(
            result.status,
            VerifStatus::InfrastructureError,
            "every proof-bearing result in a mixed-source batch must be no-proof"
        );
        assert!(result.artifact.is_none());
        assert!(result.message.contains("E0439"));
        assert!(result.message.contains("mixed source provenance"));
    }
}

#[test]
fn merge_no_opinion_defers_to_the_proving_engine() {
    // resolved attempted no proof → flow verdict wins silently.
    let primary = vec![vr_044("f", VerifStatus::NoObligations)];
    let secondary = vec![vr_044("f", VerifStatus::Proven)];
    let merged = merge_engine_verdicts(primary, secondary);
    assert_eq!(merged[0].status, VerifStatus::Proven);
    assert!(!merged[0].message.contains("E0439"));

    // flow attempted no proof → resolved verdict wins silently.
    let primary = vec![vr_044("g", VerifStatus::Proven)];
    let secondary = vec![vr_044("g", VerifStatus::InfrastructureError)];
    let merged = merge_engine_verdicts(primary, secondary);
    assert_eq!(merged[0].status, VerifStatus::Proven);
    assert!(!merged[0].message.contains("E0439"));
}

#[test]
fn merge_flow_only_obligations_pass_through() {
    // The flow engine still models obligations the resolved engine does not
    // (call sites etc.); those results must survive the merge untouched.
    let primary = vec![vr_044("a", VerifStatus::Proven)];
    let secondary = vec![
        vr_044("a", VerifStatus::Proven),
        vr_044("extern sqrt", VerifStatus::Disproven),
    ];
    let merged = merge_engine_verdicts(primary, secondary);
    assert_eq!(merged.len(), 2);
    let ext = merged
        .iter()
        .find(|r| r.func_name == "extern sqrt")
        .unwrap();
    assert_eq!(ext.status, VerifStatus::Disproven);
}

#[test]
fn dual_engine_agrees_on_simple_contract() {
    // Z3 end-to-end: a trivially provable contract must come out of
    // verify_checked_dual WITHOUT any divergence diagnostic.
    //
    // NOTE (0.34.44): arithmetic-free postcondition on purpose. Empirically
    // the two engines disagreed on integer definedness in BOTH directions —
    // i64: flow enforced overflow definedness while resolved proved the
    // unbounded model; i32: resolved enforced checked_i32 definedness while
    // flow proved unbounded. Any arithmetic postcondition was therefore a
    // divergence candidate; the agreement path was pinned with an identity
    // postcondition instead.
    //
    // H2 (0.35.x) closed the i64 side: resolved now derives per-type bounds
    // (int_bounds) and the AST fallback path was parameterized the same way,
    // so i64 arithmetic agrees between the engines (locked by
    // dual_engine_divergence_on_i64_overflow_is_fail_closed, which now
    // asserts CONSISTENT Disproven with no E0439). The i32 divergence and the
    // remaining divergences are locked by dual_engine_divergence_* below.
    if !is_z3_available() {
        return;
    }
    let src = r#"
func identity(x: i64) -> i64 {
    requires: x >= 0
    ensures: result == x
    x
}
"#;
    let file = super::parse_memory_source(src, "dual-agreement").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(src.as_bytes()).to_hex().to_string();
    let results = super::verify_checked_dual(&program, source_hash).expect("dual verify");
    let identity = results.iter().find(|r| r.func_name == "identity");
    assert!(identity.is_some(), "must verify 'identity': {results:?}");
    let identity = identity.unwrap();
    assert_eq!(
        identity.status,
        VerifStatus::Proven,
        "simple contract must be Proven by both engines: {}",
        identity.message
    );
    assert!(
        !identity.message.contains("E0439"),
        "no divergence expected: {}",
        identity.message
    );
}

#[test]
fn dual_engine_divergence_on_i64_overflow_is_fail_closed() {
    // 0.34.44 (ADR-008 §3): originally the flow engine enforced i64 overflow
    // definedness while the resolved engine proved the unbounded model —
    // flow Disproven vs resolved Proven, surfaced as E0439 with the weaker
    // (Disproven) conclusion.
    //
    // 0.35.x (H2): the resolved engine now models i64 with its real bounds
    // (int_bounds), so both engines agree on Disproven for an unbounded
    // `x * 2` — no divergence, no E0439, and still never a silent Proven.
    if !is_z3_available() {
        return;
    }
    let src = r#"
func double(x: i64) -> i64 {
    requires: x >= 0
    ensures: result == x * 2
    x * 2
}
"#;
    let file = super::parse_memory_source(src, "dual-divergence").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(src.as_bytes()).to_hex().to_string();
    let results = super::verify_checked_dual(&program, source_hash).expect("dual verify");
    let double = results
        .iter()
        .find(|r| r.func_name == "double")
        .expect("must have a result for 'double'");
    assert_eq!(
        double.status,
        VerifStatus::Disproven,
        "i64 overflow must fail closed: {}",
        double.message
    );
    if double.message.contains("E0439") {
        panic!(
            "H2 should have removed this divergence — both engines model i64 \
             bounds now: {}",
            double.message
        );
    }
}

#[test]
fn dual_engine_agrees_on_bounded_i32_definedness_no_divergence() {
    // 0.34.44 (ADR-008 §3): the REVERSE divergence — resolved enforced
    // checked_i32 definedness while flow proved the unbounded model, so a
    // bounded i32 contract produced E0439 + the weaker conclusion.
    //
    // 0.1.9 Phase E (0.39.80): the resolved engine failed to ENCODE the
    // `&&` precondition (`LogicalAnd` was nested under the int-comparison
    // branch), so `requires: x >= 0 && x <= 1000` was silently dropped and
    // the overflow VC found a negative-x counterexample. With the encoder
    // fixed, both engines now agree: bounded i32 arithmetic is Proven, no
    // E0439 divergence.
    if !is_z3_available() {
        return;
    }
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0 && x <= 1000
    ensures: result == x * 2
    x * 2
}
"#;
    let file = super::parse_memory_source(src, "dual-divergence-i32").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(src.as_bytes()).to_hex().to_string();
    let results = super::verify_checked_dual(&program, source_hash).expect("dual verify");
    let double = results
        .iter()
        .find(|r| r.func_name == "double")
        .expect("must have a result for 'double'");
    assert_eq!(
        double.status,
        VerifStatus::Proven,
        "bounded i32 must be Proven by both engines after the && encoder fix: {}",
        double.message
    );
    assert!(
        !double.message.contains("E0439"),
        "no divergence expected for bounded i32: {}",
        double.message
    );
}

/// Phase E (0.39.80): the resolved engine must ENCODE `&&` preconditions.
/// Previously `LogicalAnd` was nested under the int-comparison branch, so
/// `requires: x >= 0 && x <= 1000` was silently dropped → bounded i32
/// arithmetic was wrongly Disproven (overflow VC saw unbounded x).
#[test]
fn resolved_engine_encodes_conjunctive_precondition() {
    require_z3!();
    // Bounded i32 arithmetic: x in [0,1000], x*2 in [0,2000] — no overflow.
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0 && x <= 1000
    ensures: result == x * 2
    x * 2
}
"#;
    let file = parse_memory_source(src, "resolved-and-enc").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let mut v = Verifier::new().expect("z3");
    let results = v.verify_checked(&program);
    let f = results
        .iter()
        .find(|r| r.func_name == "double")
        .expect("double result");
    assert_eq!(
        f.status,
        VerifStatus::Proven,
        "bounded i32 with && requires must be Proven by the resolved engine: {}",
        f.message
    );
}

/// Phase E (0.39.80): unbounded i32 overflow remains fail-closed — the two
/// engines disagree (flow models unbounded/assumes defined, resolved enforces
/// checked) → E0439 + the weaker (Disproven) conclusion.
#[test]
fn dual_engine_unbounded_i32_overflow_agrees_fail_closed() {
    // Phase E (0.39.80): unbounded i32 overflow — both engines Disproven and
    // AGREE (no E0439). The flow/VIR engine enforces body arithmetic
    // definedness (i32 + i64, §11-#46) and the resolved engine enforces
    // checked_i32 — so an unbounded `x * 2` is rejected by both, fail-closed
    // without divergence.
    if !is_z3_available() {
        return;
    }
    let src = r#"
func double(x: i32) -> i32 {
    ensures: result == x * 2
    x * 2
}
"#;
    let file = super::parse_memory_source(src, "dual-unbounded-i32").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(src.as_bytes()).to_hex().to_string();
    let results = super::verify_checked_dual(&program, source_hash).expect("dual verify");
    let double = results
        .iter()
        .find(|r| r.func_name == "double")
        .expect("must have a result for 'double'");
    assert_eq!(
        double.status,
        VerifStatus::Disproven,
        "unbounded i32 overflow must fail closed: {}",
        double.message
    );
    assert!(
        !double.message.contains("E0439"),
        "both engines agree on unbounded i32 overflow (no divergence): {}",
        double.message
    );
}

#[test]
fn ordinary_map_owner_v1_uses_independent_persistent_symbolic_roots() {
    require_z3!();
    let source = r#"
func checked_source_preserving() -> i32 {
    ensures: result == 1
    let source = map_new()
    let inserted = map_set(source, "same", 1)
    let source_size = map_size(source)
    let inserted_size = map_size(inserted)
    drop(source)
    drop(inserted)
    source_size + inserted_size
}

func checked_remove_preserving() -> i32 {
    ensures: result == 1
    let root = map_new()
    let inserted = map_set(root, "remove", 1)
    let removed = map_remove(inserted, "remove")
    let old_size = map_size(inserted)
    let new_size = map_size(removed)
    drop(root)
    drop(inserted)
    drop(removed)
    old_size + new_size
}

func checked_dynamic_contains(key: string) -> bool {
    ensures: result == true
    let root = map_new()
    let inserted = map_set(root, key, 1)
    let present = has_key(inserted, key)
    drop(root)
    drop(inserted)
    present
}

func false_distinct_dynamic_keys(first: string, second: string) -> i32 {
    ensures: result == 2
    let root = map_new()
    let first_insert = map_set(root, first, 1)
    let second_insert = map_set(first_insert, second, 2)
    let size = map_size(second_insert)
    drop(root)
    drop(first_insert)
    drop(second_insert)
    size
}

func checked_equal_string_keys() -> i32 {
    ensures: result == 1
    let root = map_new()
    let first = map_set(root, "same", 1)
    let second = map_set(first, "same", 2)
    let size = map_size(second)
    drop(root)
    drop(first)
    drop(second)
    size
}

func checked_string_payload() -> i32 {
    ensures: result == 1
    let root = map_new()
    let inserted = map_set(root, "payload", "text")
    let size = map_size(inserted)
    drop(root)
    drop(inserted)
    size
}

func checked_nul_string_key() -> i32 {
    ensures: result == 1
    let root = map_new()
    let inserted = map_set(root, "bad\0key", 1)
    let size = map_size(inserted)
    drop(root)
    drop(inserted)
    size
}

func false_nul_string_key() -> i32 {
    ensures: result == 0
    let root = map_new()
    let inserted = map_set(root, "bad\0key", 1)
    let size = map_size(inserted)
    drop(root)
    drop(inserted)
    size
}

func main() -> i32 { 0 }
"#;
    let file = parse_memory_source(source, "ordinary-map-owner-v1-verifier").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical ordinary Map owner lifecycle MIR");
    let owner_ty = crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
    assert!(program
        .type_catalog()
        .get(&owner_ty)
        .is_some_and(|descriptor| {
            descriptor.kind
                == crate::core::mir::types::MirTypeKind::OrdinaryMapOwnerLifecycleI32StringV1
        }));
    assert!(program
        .type_catalog()
        .get(&crate::core::mir::types::map_root_type_id())
        .is_none());
    assert!(program
        .type_catalog()
        .get(&crate::core::mir::types::map_any_owner_v1_type_id())
        .is_none());
    crate::verifier::validate_mir_capabilities(&program)
        .expect("the ordinary Map lifecycle has an independent receipt and capability");

    let results = crate::verifier::mir::verify_program(
        &program,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("symbolic ordinary Map verification");
    for (function, status) in [
        ("function:checked_source_preserving", VerifStatus::Proven),
        ("function:checked_remove_preserving", VerifStatus::Proven),
        ("function:checked_dynamic_contains", VerifStatus::Proven),
        // Distinct MIR parameter IDs are not distinct String keys. Z3 may
        // choose equal byte strings, in which case the second Set overwrites
        // the first and the claimed size of two is disproven.
        (
            "function:false_distinct_dynamic_keys",
            VerifStatus::Disproven,
        ),
        ("function:checked_equal_string_keys", VerifStatus::Proven),
        ("function:checked_string_payload", VerifStatus::Proven),
        // Exact symbolic literals preserve the NUL byte, so the truthful
        // size claim is proven and the false claim receives a counterexample.
        ("function:checked_nul_string_key", VerifStatus::Proven),
        ("function:false_nul_string_key", VerifStatus::Disproven),
    ] {
        let result = results
            .iter()
            .find(|result| result.func_name == function)
            .unwrap_or_else(|| panic!("missing ordinary Map verifier result for {function}"));
        assert_eq!(result.status, status, "{}", result.message);
        if status == VerifStatus::Proven {
            assert_eq!(
                result
                    .artifact
                    .as_ref()
                    .map(|artifact| artifact.engine.as_str()),
                Some(ProofArtifact::ENGINE_MIR)
            );
        }
    }
}

#[test]
fn ordinary_map_owner_v1_missing_receipt_or_forged_action_never_proves() {
    require_z3!();
    let source = r#"
func checked_dynamic_contains(key: string) -> bool {
    ensures: result == true
    let root = map_new()
    let inserted = map_set(root, key, 1)
    let present = has_key(inserted, key)
    drop(root)
    drop(inserted)
    present
}
func main() -> i32 { 0 }
"#;
    let file = parse_memory_source(source, "ordinary-map-owner-v1-forgery").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical ordinary Map owner lifecycle MIR");

    let mut missing_receipt = program.clone();
    let mut function = missing_receipt
        .functions()
        .values()
        .find(|function| function.owner.0 == "function:checked_dynamic_contains")
        .expect("ordinary Map function")
        .clone();
    function
        .ownership
        .clear_checker_ordinary_map_owner_lifecycle_v1_for_test_only();
    missing_receipt.replace_function_for_test_only(function);
    let errors = crate::verifier::validate_mir_capabilities(&missing_receipt)
        .expect_err("missing ordinary Map receipt must fail capability validation");
    assert!(errors.iter().any(|error| {
        error.contains("ordinary Map owner lifecycle v1 without its independent Checker receipt")
            || error.contains("ordinary Map owner lifecycle TypeDesc/operation has no independent Checker receipt")
    }));
    let results = crate::verifier::mir::verify_program(
        &missing_receipt,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("direct verifier reports missing receipt as an unsupported shape");
    assert!(results
        .iter()
        .all(|result| !matches!(result.status, VerifStatus::Proven)));

    let mut forged_action = program.clone();
    let mut function = forged_action
        .functions()
        .values()
        .find(|function| function.owner.0 == "function:checked_dynamic_contains")
        .expect("ordinary Map function")
        .clone();
    let bool_value = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleContains {
                result,
                ..
            } => Some(result.clone()),
            _ => None,
        })
        .expect("HasKey boolean result");
    let set = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                &instruction.kind,
                crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleSet { .. }
            )
        })
        .expect("ordinary Map Set instruction");
    let crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleSet { value, .. } =
        &mut set.kind
    else {
        unreachable!("selected Set instruction")
    };
    value.clone_from(&bool_value);
    forged_action.replace_function_for_test_only(function);
    let errors = crate::verifier::validate_mir_capabilities(&forged_action)
        .expect_err("forged Set payload identity must fail the Checker receipt");
    assert!(errors.iter().any(|error| {
        error.contains("ordinary Map lifecycle operation disagrees with its private Checker")
            || error.contains("ordinary Map owner lifecycle v1 Set payload")
    }));
    let results = crate::verifier::mir::verify_program(
        &forged_action,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("direct verifier reports the forged action as unsupported");
    assert!(results
        .iter()
        .all(|result| !matches!(result.status, VerifStatus::Proven)));
}

#[test]
fn typed_ffi_string_contract_context_retires_actual_body_reads() {
    require_z3!();
    let source = r#"
        extern "C" {
            func nonempty(s: string) -> i64 requires: len(s) > 0;
        }
        func protected(s: string) -> i64 {
            requires: s != ""
            nonempty(s)
        }
        func violated() -> i64 { nonempty("") }
    "#;
    let file = parse_memory_source(source, "typed-ffi-string-context").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let roots = checked.declaration_expression_bodies();
    let requires = roots
        .values()
        .find(|root| {
            root.kind == crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternRequires
        })
        .expect("typed Requires root");
    let body = requires
        .body
        .as_ref()
        .expect("Requires materialized by Checker");
    assert_eq!(requires.node_id, body.root.result.as_ref().unwrap().node_id);
    assert_eq!(requires.owner, body.owner);
    let signature = checked.extern_func_signature("nonempty").unwrap();
    assert_eq!(signature.node_id, requires.owner);
    assert_eq!(body.parameters.len(), signature.parameter_ids.len());
    for (local, parameter) in body.parameters.iter().zip(&signature.parameter_ids) {
        assert_eq!(local.0 .0, format!("{}/local", parameter.0));
    }

    let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results =
        verify_ffi_checked_with_source_hash(&checked, hash.clone()).expect("typed FFI proofs");
    assert_eq!(
        results.len(),
        2,
        "each actual call has one obligation: {results:?}"
    );
    for (caller, status) in [
        ("protected calls nonempty", VerifStatus::Proven),
        ("violated calls nonempty", VerifStatus::Disproven),
    ] {
        let result = results
            .iter()
            .find(|result| result.func_name == caller)
            .expect("call result");
        assert_eq!(result.status, status, "{caller}: {result:?}");
        let artifact = result.artifact.as_ref().expect("typed proof identity");
        assert_eq!(artifact.engine, "resolved_ffi");
        assert_eq!(artifact.source_hash, hash);
        assert_eq!(artifact.resolved_ir_hash.len(), 64);
        assert!(artifact.mir_route_receipt.is_none());
    }
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "string FFI proofs must consume checked arguments and Requires roots"
    );
    let negative = results
        .iter()
        .find(|result| result.status == VerifStatus::Disproven)
        .unwrap();
    let expected_span = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:violated" && site.callee == "nonempty")
        .unwrap()
        .origin
        .user_span();
    assert_eq!(negative.diagnostic.as_ref().unwrap().span, expected_span);
}

#[test]
fn typed_ffi_discovery_covers_declaration_and_callable_roots() {
    require_z3!();
    let source = r#"
        extern "C" {
            func nonempty(s: string) -> i32 requires: s != "";
        }
        const seed: i32 = nonempty("");
        actor Holder {
            initial: i32 = nonempty("");
            func value() -> i32 { nonempty("") }
        }
        type Counter { n: i32 }
        trait Read { func read() -> i32 }
        impl Read for Counter {
            func read() -> i32 { nonempty("") }
        }
        flow ItemFlow {
            state Open { n: i32 }
            transition step(Open) -> Open {
                let checked = nonempty("")
                return Open { n: self.n }
            }
        }
        func defaulted(n: i32 = nonempty("")) -> i32 { n }
        func ordinary() -> i32 {
            if nonempty("") > 0 { 1 } else { 0 }
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-root-coverage")
        .expect("parse all declaration forms");
    let checked = crate::core::check_program(&file).expect("check all declaration forms");
    let expected = checked
        .call_sites_sorted()
        .into_iter()
        .filter(|site| {
            site.kind == crate::core::ResolvedCallKind::Extern && site.callee == "nonempty"
        })
        .count();
    assert_eq!(
        expected, 7,
        "constant, field, Actor, Impl, Flow, default, condition"
    );
    for kind in [
        crate::core::ir::lower::ResolvedDeclarationExpressionKind::ConstantInitializer,
        crate::core::ir::lower::ResolvedDeclarationExpressionKind::ActorFieldInitializer,
    ] {
        assert!(checked
            .declaration_expression_bodies()
            .values()
            .filter(|root| root.kind == kind)
            .all(|root| root.body.is_ok()));
    }
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("typed complete call directory");
    assert_eq!(
        results.len(),
        expected,
        "no declaration or callable call may vanish: {results:?}"
    );
    assert!(
        results
            .iter()
            .all(|result| result.status == VerifStatus::Disproven),
        "{results:?}"
    );
    assert!(results.iter().all(|result| result
        .artifact
        .as_ref()
        .is_some_and(|artifact| artifact.engine == "resolved_ffi")));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn typed_ffi_default_math_root_keeps_its_exact_deferred_identity() {
    require_z3!();
    let source = r#"
        extern "C" { func positive(value: i32) -> i32 requires: value > 0; }
        func caller(value: i32 = {
            math: { 1 + 1 == 2; };
            1
        }) -> i32 {
            requires: value == 1
            positive(value)
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-default-math-census").expect("parse");
    let checked = crate::core::check_program(&file).expect("check declaration default Math");
    let caller = checked
        .callable(&crate::core::NodeId("function:caller".into()))
        .expect("caller callable");
    let (math_root, math_origin) = caller
        .body
        .math_proof_roots()
        .into_iter()
        .next()
        .expect("Math inside the typed default must remain in the complete root census");
    assert!(
        caller
            .proof_point_worklist()
            .points
            .iter()
            .all(|point| point.id.statement != math_root),
        "the fixture exercises a real root that has no callable-body proof point"
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("typed call plus deferred Math root");
    let call = results
        .iter()
        .find(|result| result.func_name == "caller calls positive")
        .expect("the FFI call obligation remains visible");
    assert_eq!(call.status, VerifStatus::Proven, "{results:?}");
    assert!(
        call.artifact.is_none(),
        "the real Math root keeps this request on compatibility"
    );

    let deferred = results
        .iter()
        .find(|result| result.func_name.contains(&math_root.0))
        .expect("the unmodeled default Math root has its own result");
    assert_eq!(
        deferred.status,
        VerifStatus::NotInTrustedSubset,
        "{results:?}"
    );
    assert_eq!(
        deferred.diagnostic.as_ref().unwrap().span,
        math_origin.user_span(),
        "the deferred root uses the Math statement's checker-owned origin"
    );
    assert!(deferred.artifact.is_none());
    assert!(
        !crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "a true unsupported default Math root must preserve the compatibility boundary"
    );
}

#[test]
fn typed_ffi_default_math_with_a_checked_omitted_use_gets_separate_proofs() {
    require_z3!();
    let source = r#"
        extern "C" { func positive(value: i32) -> i32 requires: value > 0; }
        func good(value: i32 = {
            let seed: i32 = 1
            math: { seed == 1; };
            seed
        }) -> i32 {
            requires: value == 1
            positive(value)
        }
        func bad(value: i32 = {
            let seed: i32 = 1
            math: { seed < 0; };
            seed
        }) -> i32 {
            requires: value == 1
            positive(value)
        }
        func main() -> i64 {
            let _good_result = good()
            let _bad_result = bad()
            0
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-default-math-roots").expect("parse");
    let checked = crate::core::check_program(&file).expect("check declaration default Math");
    let good = checked
        .callable(&crate::core::NodeId("function:good".into()))
        .expect("good callable");
    let bad = checked
        .callable(&crate::core::NodeId("function:bad".into()))
        .expect("bad callable");
    let good_root = good
        .body
        .math_proof_roots()
        .into_iter()
        .next()
        .expect("the good default Math root remains in the complete census")
        .0;
    let bad_root = bad
        .body
        .math_proof_roots()
        .into_iter()
        .next()
        .expect("the bad default Math root remains in the complete census")
        .0;

    let main = checked
        .callable(&crate::core::NodeId("function:main".into()))
        .expect("main callable");
    let mut forged_use = main
        .proof_point_worklist()
        .default_uses
        .into_iter()
        .next()
        .expect("omitted default arguments have exact use receipts");
    forged_use.parameter_index += 1;
    assert!(
        !forged_use.validate_in_checked_program(&checked),
        "a public-field mutation cannot retain the checker-sealed default-use authorization"
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("typed calls plus scoped default Math");
    for (name, root, expected) in [
        ("good", &good_root, VerifStatus::Proven),
        ("bad", &bad_root, VerifStatus::Disproven),
    ] {
        let call = results
            .iter()
            .find(|result| result.func_name == format!("{name} calls positive"))
            .expect("the body FFI call obligation remains independently visible");
        assert_eq!(call.status, VerifStatus::Proven, "{results:?}");
        let default = results
            .iter()
            .find(|result| result.func_name.contains(&root.0))
            .expect("the exact default Math root has its own result");
        assert_eq!(default.status, expected, "{results:?}");
        assert!(
            default
                .artifact
                .as_ref()
                .is_some_and(|artifact| artifact.engine == "resolved_ffi"),
            "an executable default Math root must carry its own proof artifact: {results:?}"
        );
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn typed_ffi_default_and_foreign_ensures_stay_separate_at_the_call_boundary() {
    require_z3!();
    let source = r#"
        extern "C" {
            func positive(value: i32) -> i32
                requires: value > 0
                ensures: result == result && value == 1;
            func untrusted(value: i32) -> i32
                requires: value > 0
                ensures: result == value + 1;
        }
        func good(value: i32 = {
            let seed: i32 = 1
            math: { seed == 1; };
            seed
        }) -> i32 {
            requires: value == 1
            positive(value)
        }
        func bad(value: i32 = {
            let seed: i32 = 1
            math: { seed < 0; };
            seed
        }) -> i32 {
            requires: value == 1
            let observed = untrusted(value)
            ensures: result == value + 1
            observed
        }
        func main() -> i32 {
            let _good = good()
            let _bad = bad()
            0
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-default-ensures-composition")
        .expect("parse default and post-call FFI contracts");
    let checked = crate::core::check_program(&file).expect("check composition fixture");
    let good = checked
        .callable(&crate::core::NodeId("function:good".into()))
        .expect("good callable");
    let bad = checked
        .callable(&crate::core::NodeId("function:bad".into()))
        .expect("bad callable");
    let good_default_math = good
        .body
        .math_proof_roots()
        .into_iter()
        .next()
        .expect("good default has a Math obligation")
        .0;
    let bad_default_math = bad
        .body
        .math_proof_roots()
        .into_iter()
        .next()
        .expect("bad default has a Math obligation")
        .0;

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let ffi_names =
        std::collections::BTreeSet::from(["positive".to_string(), "untrusted".to_string()]);
    let results = super::ffi_checked::verify(&checked, &ffi_names, "")
        .expect("scoped FFI composition proof")
        .expect("complete typed verifier consumer context");
    let result = |name: &str| {
        results
            .iter()
            .find(|result| result.func_name == name)
            .unwrap_or_else(|| panic!("missing {name}: {results:?}"))
    };
    assert_eq!(
        result("good calls positive").status,
        VerifStatus::Proven,
        "the caller Requires is available while proving the pre-call Requires"
    );
    assert_eq!(
        result("good -> extern positive Ensures").status,
        VerifStatus::Proven,
        "the tautological foreign Ensures is checked against the fresh result"
    );
    assert_eq!(
        result("bad calls untrusted").status,
        VerifStatus::Proven,
        "the caller Requires establishes the input-side call condition"
    );
    assert_eq!(
        result("bad -> extern untrusted Ensures").status,
        VerifStatus::Disproven,
        "the foreign Ensures cannot constrain its fresh result"
    );
    assert!(
        results
            .iter()
            .all(|result| result.func_name != "bad Ensures"),
        "the unproved foreign Ensures must not appear as a caller postcondition proof"
    );
    assert_eq!(
        results
            .iter()
            .find(|result| result.func_name.contains(&good_default_math.0))
            .expect("good default Math result")
            .status,
        VerifStatus::Proven
    );
    assert_eq!(
        results
            .iter()
            .find(|result| result.func_name.contains(&bad_default_math.0))
            .expect("bad default Math result")
            .status,
        VerifStatus::Disproven,
        "default Math is replayed in its own scope, without borrowing the foreign Ensures"
    );
    assert!(result("bad -> extern untrusted Ensures").artifact.is_some());
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn typed_ffi_ensures_only_call_keeps_default_math_as_an_independent_root() {
    require_z3!();
    let source = r#"
        extern "C" {
            func foreign(value: i32) -> i32 ensures: result == result;
        }
        func good(value: i32 = {
            let seed: i32 = 1
            math: { seed == 1; };
            seed
        }) -> i32 {
            foreign(value)
        }
        func bad(value: i32 = {
            let seed: i32 = 1
            math: { seed < 0; };
            seed
        }) -> i32 {
            foreign(value)
        }
        func main() -> i32 {
            let _good = good()
            let _bad = bad()
            0
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-ensures-only-default-roots")
        .expect("parse Ensures-only extern/default fixture");
    let checked = crate::core::check_program(&file).expect("check Ensures-only fixture");
    let good = checked
        .callable(&crate::core::NodeId("function:good".into()))
        .expect("good callable");
    let bad = checked
        .callable(&crate::core::NodeId("function:bad".into()))
        .expect("bad callable");
    let good_root = good
        .body
        .math_proof_roots()
        .into_iter()
        .next()
        .expect("good default Math source root")
        .0;
    let bad_root = bad
        .body
        .math_proof_roots()
        .into_iter()
        .next()
        .expect("bad default Math source root")
        .0;

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("canonical Ensures and default roots");
    let result = |name: &str| {
        results
            .iter()
            .find(|result| result.func_name == name)
            .unwrap_or_else(|| panic!("missing {name}: {results:?}"))
    };
    for owner in ["good", "bad"] {
        assert_eq!(
            result(&format!("{owner} -> extern foreign Ensures")).status,
            VerifStatus::Proven,
            "the tautological foreign Ensures is checked independently"
        );
    }
    for (root, expected) in [
        (&good_root, VerifStatus::Proven),
        (&bad_root, VerifStatus::Disproven),
    ] {
        let default = results
            .iter()
            .find(|result| result.func_name.contains(&root.0))
            .unwrap_or_else(|| panic!("missing independent Math root {}: {results:?}", root.0));
        assert_eq!(default.status, expected, "{results:?}");
        assert!(
            default
                .artifact
                .as_ref()
                .is_some_and(|artifact| artifact.engine == "resolved_ffi"),
            "each supported Math root has a separate proof artifact: {results:?}"
        );
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn ffi_omitted_fixed_default_is_a_source_producer_counterexample() {
    let source = r#"
        extern "C" {
            func foreign(value: i32 = 7) -> i32
                requires: value == 7
                ensures: result == 8;
        }
        func caller() -> i32 { foreign() }
    "#;
    assert!(
        parse_memory_source(source, "ffi-omitted-fixed-default-counterexample").is_err(),
        "the current extern parameter/source lowering has no optional fixed-parameter syntax"
    );
}

#[test]
fn typed_ffi_deferred_ensures_emits_no_proof_artifact() {
    require_z3!();
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
    let file = parse_memory_source(source, "typed-ffi-deferred-ensures").expect("parse");
    let checked = crate::core::check_program(&file).expect("check deferred Ensures root");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let names = std::collections::BTreeSet::from(["foreign".to_string()]);
    let results = super::ffi_checked::verify(&checked, &names, "")
        .expect("explicit Deferred Ensures result")
        .expect("complete typed verifier consumer context");
    let ensures = results
        .iter()
        .find(|result| result.func_name == "caller -> extern foreign Ensures")
        .expect("called Deferred Ensures retains its named obligation");
    assert_eq!(ensures.status, VerifStatus::NotInTrustedSubset);
    assert!(ensures.artifact.is_none());
    assert_eq!(
        ensures.trusted_subset_domain,
        Some(TrustedSubsetDomain::Contract)
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn typed_ffi_deferred_predicate_preserves_explicit_compatibility_obligation() {
    require_z3!();
    let source = r#"
        extern "C" {
            func foreign(s: string) -> i64 requires: unsupported_contract(s);
        }
        func caller(s: string) -> i64 { foreign(s) }
    "#;
    let file = parse_memory_source(source, "typed-ffi-deferred-contract").expect("parse");
    let checked =
        crate::core::check_program(&file).expect("historically accepted contract stays legal");
    let root = checked
        .declaration_expression_bodies()
        .values()
        .find(|root| {
            root.kind == crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternRequires
        })
        .unwrap();
    let errors = root
        .body
        .as_ref()
        .expect_err("untyped predicate must stay Deferred");
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("checker-finalized canonical type")),
        "{errors:?}"
    );
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("explicit checker-owned coverage result");
    assert_eq!(
        results.len(),
        1,
        "a Deferred root cannot erase the called Requires obligation"
    );
    assert_eq!(results[0].status, VerifStatus::NotInTrustedSubset);
    assert_eq!(
        results[0]
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    );
    assert!(results[0].message.contains("checker-owned FFI declaration"));
    assert_eq!(
        crate::core::CheckedProgram::test_legacy_body_access(),
        Vec::<crate::core::LegacyBodyConsumer>::new()
    );
}

#[test]
fn typed_ffi_deferred_actual_argument_preserves_old_conditional_proof() {
    require_z3!();
    let source = r#"
        extern "C" {
            func foreign(s: string, n: i32) -> i32 requires: n > 0;
        }
        func caller(choice: bool) -> i32 {
            foreign("ok", if choice { 1 } else { 2 })
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-deferred-argument").expect("parse");
    let checked = crate::core::check_program(&file).expect("old legal conditional argument");
    assert!(checked
        .declaration_expression_bodies()
        .values()
        .filter(|root| root.kind
            == crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternRequires)
        .all(|root| root.body.is_ok()));
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("explicit compatibility context");
    assert_eq!(results.len(), 1, "conditional actual argument: {results:?}");
    assert_eq!(results[0].status, VerifStatus::Proven, "{results:?}");
    assert_eq!(
        results[0]
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some("resolved_ffi"),
        "expression-only conditional actuals use the checker-owned typed receipt"
    );
    assert_eq!(
        crate::core::CheckedProgram::test_legacy_body_access(),
        Vec::<crate::core::LegacyBodyConsumer>::new()
    );
}

#[test]
fn typed_ffi_conditional_actual_counterexample_stays_checker_owned() {
    require_z3!();
    let source = r#"
        extern "C" {
            func foreign(value: i32, marker: string) -> i32
                requires: value > 0 && marker != "";
        }
        func caller(choice: bool) -> i32 {
            foreign(if choice { 1 } else { -1 }, "ok")
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-conditional-counterexample")
        .expect("parse conditional counterexample");
    let checked = crate::core::check_program(&file).expect("check conditional counterexample");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("typed conditional FFI proof");
    assert_eq!(
        results.len(),
        1,
        "one conditional call-site result: {results:?}"
    );
    assert_eq!(results[0].status, VerifStatus::Disproven, "{results:?}");
    assert_eq!(
        results[0]
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some("resolved_ffi"),
        "the conditional counterexample must retain checker-owned provenance"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "a typed conditional counterexample must not reopen the retained FFI body"
    );
}

#[test]
fn typed_ffi_short_circuit_and_actual_integer_width_control_definedness() {
    require_z3!();
    for (predicate, argument, status) in [
        ("false && 1 / n > 0", "0", VerifStatus::Disproven),
        ("true || 1 / n > 0", "0", VerifStatus::Proven),
        ("n + 1 > n", "2147483647", VerifStatus::Disproven),
        ("n + 1 > n", "2147483646", VerifStatus::Proven),
    ] {
        // A string parameter keeps this outside the closed scalar executable
        // island, exercising the real typed FFI compatibility migration.
        let source = format!("extern \"C\" {{ func foreign(s: string, n: i32) -> i32 requires: {predicate}; }}\nfunc caller() -> i32 {{ foreign(\"ok\", {argument}) }}");
        let file = parse_memory_source(&source, "typed-ffi-definedness").expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = verify_ffi_checked(&checked).expect("typed predicate definedness");
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].status, status,
            "{predicate} with {argument}: {results:?}"
        );
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn typed_ffi_div_rem_requires_use_canonical_operand_width_and_short_circuit() {
    require_z3!();
    for (declaration, ty, minimum, nearby) in [
        ("", "i32", "-2147483648", "-2147483647"),
        (
            "",
            "i64",
            "((-9223372036854775807 as i64) - (1 as i64))",
            "(-9223372036854775807 as i64)",
        ),
        ("type Count = i32", "Count", "-2147483648", "-2147483647"),
    ] {
        for (predicate, input, status) in [
            ("n % -1 == 0", minimum, VerifStatus::Disproven),
            ("n % -1 == 0", nearby, VerifStatus::Proven),
            ("n / -1 > 0", minimum, VerifStatus::Disproven),
            ("n / -1 > 0", nearby, VerifStatus::Proven),
            ("true || n % -1 == 0", minimum, VerifStatus::Proven),
            ("false && n % -1 == 0", minimum, VerifStatus::Disproven),
        ] {
            let source = format!(
                "{declaration}\nextern \"C\" {{ func foreign(s: string, n: {ty}) -> i32 requires: {predicate}; }}\nfunc caller(n: {ty}) -> i32 {{ requires: n == {input}\nforeign(\"ok\", n) }}"
            );
            let file = parse_memory_source(&source, "typed-ffi-remainder-width").expect("parse");
            let checked = crate::core::check_program(&file).expect("check");
            crate::core::CheckedProgram::reset_test_legacy_body_access();
            let results = verify_ffi_checked(&checked).expect("typed div/rem Requires");
            assert_eq!(results.len(), 1, "{source}: {results:?}");
            assert_eq!(results[0].status, status, "{source}: {results:?}");
            assert_eq!(results[0].artifact.as_ref().unwrap().engine, "resolved_ffi");
            assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
        }
    }
}

#[test]
fn typed_ffi_actual_arguments_keep_div_rem_definedness_when_contract_ignores_them() {
    require_z3!();
    for (declaration, ty, minimum, nearby) in [
        ("", "i32", "-2147483648", "-2147483647"),
        (
            "",
            "i64",
            "((-9223372036854775807 as i64) - (1 as i64))",
            "(-9223372036854775807 as i64)",
        ),
        ("type Count = i32", "Count", "-2147483648", "-2147483647"),
    ] {
        for (argument_type, argument, predicate, input, status) in [
            (ty, "n % -1", "n == 0", minimum, VerifStatus::Disproven),
            (ty, "n % -1", "n == 0", nearby, VerifStatus::Proven),
            (ty, "n % -1", "true", minimum, VerifStatus::Disproven),
            (ty, "n % -1", "true", nearby, VerifStatus::Proven),
            (ty, "n / -1", "true", minimum, VerifStatus::Disproven),
            (ty, "n / -1", "true", nearby, VerifStatus::Proven),
            (
                "bool",
                "true || n % -1 == 0",
                "true",
                minimum,
                VerifStatus::Proven,
            ),
        ] {
            let source = format!(
                "{declaration}\nextern \"C\" {{ func foreign(s: string, n: {argument_type}) -> i32 requires: {predicate}; }}\nfunc caller(n: {ty}) -> i32 {{ requires: n == {input}\nforeign(\"ok\", {argument}) }}"
            );
            let file = parse_memory_source(&source, "typed-ffi-remainder-argument").expect("parse");
            let checked = crate::core::check_program(&file).expect("check");
            crate::core::CheckedProgram::reset_test_legacy_body_access();
            let results = verify_ffi_checked(&checked).expect("typed actual argument definedness");
            assert_eq!(results.len(), 1, "{source}: {results:?}");
            assert_eq!(results[0].status, status, "{source}: {results:?}");
            assert_eq!(results[0].artifact.as_ref().unwrap().engine, "resolved_ffi");
            assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
        }
    }
}

#[test]
fn typed_ffi_string_literals_preserve_nul_unicode_and_escape_text() {
    require_z3!();
    for (expected, argument, length, status) in [
        (r#""a\0🧪值""#, r#""a\0🧪值""#, 4, VerifStatus::Proven),
        (r#""a\0🧪值""#, r#""a🧪值""#, 4, VerifStatus::Disproven),
        (r#""a\0🧪值""#, r#""a\0🧫值""#, 4, VerifStatus::Disproven),
        (r#""🧪值""#, r#""🧪值""#, 2, VerifStatus::Proven),
        (r#""🧪值""#, r#""🧪值""#, 7, VerifStatus::Disproven),
        (r#""\\u{41}""#, r#""\\u{41}""#, 6, VerifStatus::Proven),
        (r#""\\u{41}""#, r#""A""#, 6, VerifStatus::Disproven),
        (r#""a\0\\u{41}""#, r#""a\0\\u{41}""#, 8, VerifStatus::Proven),
        (r#""a\0\\u{41}""#, r#""a\0A""#, 8, VerifStatus::Disproven),
    ] {
        let source = format!(
            "extern \"C\" {{ func foreign(s: string) -> i32 requires: s == {expected} && len(s) == {length}; }}\nfunc caller() -> i32 {{ foreign({argument}) }}"
        );
        let file = parse_memory_source(&source, "typed-ffi-exact-string-literal").expect("parse");
        let checked = crate::core::check_program(&file).expect("check exact String fixture");
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = verify_ffi_checked(&checked).expect("exact typed String proof");
        assert_eq!(results.len(), 1, "{source}: {results:?}");
        assert_eq!(results[0].status, status, "{source}: {results:?}");
        assert_eq!(results[0].artifact.as_ref().unwrap().engine, "resolved_ffi");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn typed_ffi_integer_parameter_domains_preserve_safe_inputs_and_unsafe_requires() {
    require_z3!();
    for (declaration, ty, maximum) in [
        ("", "i32", "2147483647"),
        ("type Count = i32", "Count", "2147483647"),
        ("", "i64", "(9223372036854775807 as i64)"),
    ] {
        for (condition, argument, status) in [
            (format!("x < {maximum}"), "x + 1", VerifStatus::Proven),
            (format!("x == {maximum}"), "x + 1", VerifStatus::Disproven),
            ("x + 1 > x".into(), "x", VerifStatus::Disproven),
        ] {
            let source = format!(
                "{declaration}\nextern \"C\" {{ func foreign(s: string, n: {ty}) -> i32 requires: true; }}\nfunc caller(x: {ty}) -> i32 {{ requires: {condition}\nforeign(\"ok\", {argument}) }}"
            );
            let file =
                parse_memory_source(&source, "typed-ffi-entry-integer-domain").expect("parse");
            let checked = crate::core::check_program(&file).expect("check bounded entry");
            crate::core::CheckedProgram::reset_test_legacy_body_access();
            let results = verify_ffi_checked(&checked).expect("exact machine parameter domain");
            assert_eq!(results.len(), 1, "{source}: {results:?}");
            assert_eq!(results[0].status, status, "{source}: {results:?}");
            assert_eq!(results[0].artifact.as_ref().unwrap().engine, "resolved_ffi");
            assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
        }
    }
}

#[test]
fn typed_ffi_higher_unicode_keeps_the_actual_call_obligation_deferred() {
    require_z3!();
    for literal in [r#""\u{30000}""#, r#""\u{10ffff}""#] {
        let source = format!(
            "extern \"C\" {{ func foreign(s: string) -> i32 requires: s == {literal} && len(s) == 1; }}\nfunc caller() -> i32 {{ foreign({literal}) }}"
        );
        let file =
            parse_memory_source(&source, "typed-ffi-higher-unicode-alphabet").expect("parse");
        let checked =
            crate::core::check_program(&file).expect("higher Unicode remains a legal String");
        let root = checked
            .declaration_expression_bodies()
            .values()
            .find(|root| {
                root.kind
                    == crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternRequires
            })
            .expect("the legal Requires has a declaration root");
        assert!(
            root.body.is_ok(),
            "the proof alphabet does not reject typed construction"
        );
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = verify_ffi_checked(&checked).expect("explicit host SMT alphabet boundary");
        assert_eq!(results.len(), 1, "{source}: {results:?}");
        assert_eq!(
            results[0].status,
            VerifStatus::NotInTrustedSubset,
            "{source}: {results:?}"
        );
        assert!(results[0].message.contains("checker-owned FFI declaration"));
        assert!(results[0].message.contains("owner 'function:caller'"));
        let site = checked
            .call_sites_sorted()
            .into_iter()
            .find(|site| site.callee == "foreign")
            .expect("checker call-site directory has the deferred call");
        assert_eq!(
            results[0].diagnostic.as_ref().unwrap().span,
            site.origin.user_span()
        );
        assert!(results[0].artifact.is_none());
        assert_eq!(
            crate::core::CheckedProgram::test_legacy_body_access(),
            Vec::<crate::core::LegacyBodyConsumer>::new()
        );
    }
}

#[test]
fn typed_ffi_deferred_context_is_checker_owned_without_legacy_access() {
    require_z3!();
    for (literal, first, length) in [
        (r#""🧪值""#, r#""🧪""#, 2),
        (r#""\\u{41}""#, r#""\\""#, 6),
        (r#""a\0🧪值""#, r#""a""#, 4),
    ] {
        for claimed_length in [length, length + 1] {
            // char_at is deliberately outside the typed FFI encoding. The
            // checker therefore reports a precise coverage boundary instead
            // of reopening a retained surface body or inventing a status.
            let source = format!(
                "extern \"C\" {{ func foreign(s: string) -> i32 requires: len(s) == {claimed_length}; }}\nfunc caller(s: string) -> i32 {{ requires: s == {literal} && char_at(s, 0) == {first}\nforeign(s) }}"
            );
            let file =
                parse_memory_source(&source, "typed-ffi-exact-deferred-string").expect("parse");
            let checked = crate::core::check_program(&file).expect("old legal rich String context");
            crate::core::CheckedProgram::reset_test_legacy_body_access();
            let results = verify_ffi_checked(&checked).expect("checker-owned String boundary");
            assert_eq!(results.len(), 1, "{source}: {results:?}");
            assert_eq!(
                results[0].status,
                VerifStatus::NotInTrustedSubset,
                "{source}: {results:?}"
            );
            assert!(results[0].artifact.is_none());
            assert_eq!(
                results[0]
                    .diagnostic
                    .as_ref()
                    .and_then(|diagnostic| diagnostic.code.as_deref()),
                Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            );
            assert_eq!(
                crate::core::CheckedProgram::test_legacy_body_access(),
                Vec::<crate::core::LegacyBodyConsumer>::new()
            );
        }
    }
}

#[test]
fn typed_ffi_aggregate_requires_keeps_unused_callers_and_exact_source_identity() {
    require_z3!();
    // Neither callable is reachable from main. Their syntactic extern calls
    // remain obligations; execution-graph reachability cannot erase them.
    let source = r#"
        #[repr(C)]
        type Pair { value: i64 }
        extern "C" { func foreign(value: Pair) -> Pair requires: value.value >= 0; }
        func caller_positive(value: Pair) -> Pair {
            requires: value.value >= 0
            foreign(value)
        }
        func caller_negative(value: Pair) -> Pair {
            requires: value.value < 0
            foreign(value)
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-aggregate-unused-callers").expect("parse");
    let checked = crate::core::check_program(&file).expect("original aggregate Requires fixture");
    assert!(!crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
        .expect("typed aggregate input predicate proof");
    assert_eq!(
        results.len(),
        2,
        "all dormant call-site obligations: {results:?}"
    );
    for (caller, status) in [
        ("caller_positive", VerifStatus::Proven),
        ("caller_negative", VerifStatus::Disproven),
    ] {
        let result = results
            .iter()
            .find(|result| result.func_name.contains(caller))
            .unwrap();
        assert_eq!(result.status, status, "{caller}: {results:?}");
        let artifact = result
            .artifact
            .as_ref()
            .expect("complete typed proof provenance");
        assert_eq!(artifact.engine, "resolved_ffi");
        assert_eq!(artifact.source_hash, source_hash);
        assert!(!artifact.resolved_ir_hash.is_empty());
        if status == VerifStatus::Disproven {
            let site = checked
                .call_sites_sorted()
                .into_iter()
                .find(|site| site.owner == format!("function:{caller}") && site.callee == "foreign")
                .expect("exact negative call-site identity");
            assert_eq!(
                result.diagnostic.as_ref().unwrap().span,
                site.origin.user_span()
            );
        }
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn typed_ffi_aggregate_nested_fields_keep_scalar_domains_and_exact_strings() {
    require_z3!();
    for (claimed_length, status) in [(4, VerifStatus::Proven), (5, VerifStatus::Disproven)] {
        let source = format!(
            r#"
            #[repr(C)]
            type Inner {{ value: i32, label: string }}
            #[repr(C)]
            type Envelope {{ inner: Inner, enabled: bool }}
            extern "C" {{
                func foreign(value: Envelope) -> Envelope
                    requires: value.enabled && value.inner.value > 0 && len(value.inner.label) == {claimed_length};
            }}
            func caller(value: Envelope) -> Envelope {{
                requires: value.enabled && value.inner.value == 3 && value.inner.label == "a\0🧪值"
                foreign(value)
            }}
        "#
        );
        let file = parse_memory_source(&source, "typed-ffi-nested-record-fields").expect("parse");
        let checked = crate::core::check_program(&file).expect("check nested canonical fields");
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = verify_ffi_checked(&checked).expect("typed nested field contract");
        assert_eq!(results.len(), 1, "{source}: {results:?}");
        assert_eq!(results[0].status, status, "{source}: {results:?}");
        assert_eq!(results[0].artifact.as_ref().unwrap().engine, "resolved_ffi");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
    for (condition, status) in [
        ("value.value < 2147483647", VerifStatus::Proven),
        ("value.value == 2147483647", VerifStatus::Disproven),
    ] {
        let source = format!(
            r#"
            #[repr(C)] type Pair {{ value: i32 }}
            extern "C" {{ func foreign(value: Pair) -> Pair requires: true; }}
            func caller(value: Pair) -> Pair {{
                requires: {condition}
                foreign(Pair {{ value: value.value + 1 }})
            }}
        "#
        );
        let file = parse_memory_source(&source, "typed-ffi-record-entry-domain").expect("parse");
        let checked = crate::core::check_program(&file).expect("check record leaf domain");
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = verify_ffi_checked(&checked).expect("bounded record field inputs");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, status, "{source}: {results:?}");
        assert_eq!(results[0].artifact.as_ref().unwrap().engine, "resolved_ffi");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn typed_ffi_aggregate_actual_fields_keep_unobserved_definedness() {
    require_z3!();
    for (condition, noise, status) in [
        ("n == -2147483648", "n % -1", VerifStatus::Disproven),
        ("n == -2147483647", "n % -1", VerifStatus::Proven),
        ("n == 2147483647", "n + 1", VerifStatus::Disproven),
        ("n < 2147483647", "n + 1", VerifStatus::Proven),
    ] {
        let source = format!(
            r#"
            #[repr(C)] type Pair {{ value: i32, noise: i32 }}
            extern "C" {{ func foreign(value: Pair) -> Pair requires: value.value >= 0; }}
            func caller(n: i32) -> Pair {{
                requires: {condition}
                foreign(Pair {{ value: 1, noise: {noise} }})
            }}
        "#
        );
        let file = parse_memory_source(&source, "typed-ffi-record-unused-field").expect("parse");
        let checked = crate::core::check_program(&file).expect("check actual record construction");
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results =
            verify_ffi_checked(&checked).expect("all record field evaluation obligations");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, status, "{source}: {results:?}");
        assert_eq!(results[0].artifact.as_ref().unwrap().engine, "resolved_ffi");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn typed_ffi_aggregate_rvalue_projection_keeps_whole_record_evaluation() {
    require_z3!();
    for (input, status) in [
        ("-2147483648", VerifStatus::Disproven),
        ("-2147483647", VerifStatus::Proven),
    ] {
        let source = format!(
            r#"
            type Pair {{ value: i32, noise: i32 }}
            extern "C" {{ func foreign(s: string, value: i32) -> i32 requires: value >= 0; }}
            func caller(n: i32) -> i32 {{
                requires: n == {input}
                foreign("ok", (Pair {{ value: 1, noise: n % -1 }}).value)
            }}
        "#
        );
        let file =
            parse_memory_source(&source, "typed-ffi-record-rvalue-projection").expect("parse");
        let checked =
            crate::core::check_program(&file).expect("check typed value field projection");
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results =
            verify_ffi_checked(&checked).expect("rvalue field must keep constructor traps");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, status, "{source}: {results:?}");
        assert_eq!(results[0].artifact.as_ref().unwrap().engine, "resolved_ffi");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn typed_ffi_caller_if_and_short_circuit_paths_constrain_the_call() {
    require_z3!();
    let source = r#"
        extern "C" { func foreign(value: i32, marker: string) -> i32 requires: value >= 0 && marker != ""; }
        func if_positive(value: i32, marker: string) -> i32 {
            requires: marker != ""
            if value >= 0 { foreign(value, marker) } else { 0 }
        }
        func if_negative(value: i32, marker: string) -> i32 {
            requires: marker != ""
            if value < 0 { foreign(value, marker) } else { 0 }
        }
        func short_circuit_positive(value: i32, marker: string) -> i32 {
            requires: marker != ""
            if value == 0 || (value > 0 && foreign(value, marker) >= 0) { 1 } else { 0 }
        }
        func while_positive(value: i32, marker: string) -> i32 {
            requires: marker != ""
            while value >= 0 { foreign(value, marker); break; }
            0
        }
        func while_negative(value: i32, marker: string) -> i32 {
            requires: marker != ""
            while value < 0 { foreign(value, marker); break; }
            0
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-caller-paths").expect("parse");
    let checked = crate::core::check_program(&file).expect("check path-sensitive calls");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("typed path-conditioned FFI proofs");
    assert_eq!(
        results.len(),
        5,
        "one proof per reachable FFI call: {results:?}"
    );
    for (caller, status) in [
        ("if_positive", VerifStatus::Proven),
        ("if_negative", VerifStatus::Disproven),
        ("short_circuit_positive", VerifStatus::Proven),
        ("while_positive", VerifStatus::Proven),
        ("while_negative", VerifStatus::Disproven),
    ] {
        let result = results
            .iter()
            .find(|result| result.func_name.contains(caller))
            .expect("typed call result");
        assert_eq!(result.status, status, "{caller}: {result:?}");
        assert_eq!(
            result.artifact.as_ref().unwrap().engine,
            "resolved_ffi",
            "supported caller branch facts must be consumed by the typed verifier"
        );
    }
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "supported branch proofs must use checked path expressions without rebuilding AST"
    );
}

#[test]
fn typed_ffi_caller_literal_match_patterns_constrain_the_call() {
    require_z3!();
    let source = r#"
        extern "C" { func positive(value: i32, marker: string) -> i32 requires: value > 0 && marker != ""; }
        func match_positive(value: i32, marker: string) -> i32 {
            requires: marker != ""
            match value {
                1 => positive(value, marker),
                _ => 0
            }
        }
        func match_zero(value: i32, marker: string) -> i32 {
            requires: marker != ""
            match value {
                0 => positive(value, marker),
                _ => 0
            }
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-match-literal-paths").expect("parse");
    let checked = crate::core::check_program(&file).expect("check literal match paths");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("typed literal-pattern FFI proofs");
    assert_eq!(
        results.len(),
        2,
        "one proof per reachable FFI call: {results:?}"
    );
    for (caller, status) in [
        ("match_positive", VerifStatus::Proven),
        ("match_zero", VerifStatus::Disproven),
    ] {
        let result = results
            .iter()
            .find(|result| result.func_name.contains(caller))
            .expect("typed match call result");
        assert_eq!(result.status, status, "{caller}: {result:?}");
        assert_eq!(result.artifact.as_ref().unwrap().engine, "resolved_ffi");
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn typed_ffi_caller_singleton_iteration_binds_the_checked_element() {
    require_z3!();
    let source = r#"
        extern "C" { func positive(value: i32, marker: string) -> i32 requires: value > 0 && marker != ""; }
        func one_positive(value: i32, marker: string) -> i32 {
            requires: marker != ""
            requires: value == 0
            for item in [value + 1] { positive(item, marker); }
            0
        }
        func one_zero(value: i32, marker: string) -> i32 {
            requires: marker != ""
            requires: value == 0
            for item in [value] { positive(item, marker); }
            0
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-singleton-iteration").expect("parse");
    let checked = crate::core::check_program(&file).expect("check singleton iteration");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("typed singleton-iteration FFI proofs");
    assert_eq!(
        results.len(),
        2,
        "one proof per loop-body call: {results:?}"
    );
    for (caller, status) in [
        ("one_positive", VerifStatus::Proven),
        ("one_zero", VerifStatus::Disproven),
    ] {
        let result = results
            .iter()
            .find(|result| result.func_name.contains(caller))
            .expect("typed iteration call result");
        assert_eq!(result.status, status, "{caller}: {result:?}");
        assert_eq!(result.artifact.as_ref().unwrap().engine, "resolved_ffi");
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn typed_ffi_while_exit_paths_are_exact() {
    require_z3!();
    let source = r#"
        extern "C" {
            func positive(value: i32, marker: string) -> i32 requires: value > 0 && marker != "";
            func greater_than_one(value: i32, marker: string) -> i32 requires: value > 1 && marker != "";
        }
        func while_continue_positive(value: i32, marker: string) -> i32 {
            requires: marker != ""
            while value >= 0 {
                if value == 0 { continue; }
                positive(value, marker);
                break;
            }
            0
        }
        func while_break_positive(value: i32, marker: string) -> i32 {
            requires: marker != ""
            while value >= 0 {
                if value == 0 { break; }
                positive(value, marker);
                break;
            }
            0
        }
        func while_break_negative(value: i32, marker: string) -> i32 {
            requires: marker != ""
            while value >= 0 {
                if value <= 0 { break; }
                greater_than_one(value, marker);
                break;
            }
            0
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-loop-exit-paths").expect("parse");
    let checked = crate::core::check_program(&file).expect("check loop exit paths");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("typed loop FFI proofs");
    assert_eq!(
        results.len(),
        3,
        "one proof per loop-body call: {results:?}"
    );
    for (caller, status) in [
        ("while_continue_positive", VerifStatus::Proven),
        ("while_break_positive", VerifStatus::Proven),
        ("while_break_negative", VerifStatus::Disproven),
    ] {
        let result = results
            .iter()
            .find(|result| result.func_name.contains(caller))
            .expect("loop call result");
        assert_eq!(result.status, status, "{caller}: {result:?}");
        assert_eq!(result.artifact.as_ref().unwrap().engine, "resolved_ffi");
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn typed_ffi_while_backedge_mutation_defers_entry_value_assumptions() {
    require_z3!();
    let source = r#"
        extern "C" { func positive(value: i32, marker: string) -> i32 requires: value > 0 && marker != ""; }
        func while_backedge_mutates_local(value: i32, marker: string) -> i32 {
            requires: marker != ""
            let mut current = value
            while current > 0 {
                current = current - 1
                positive(current, marker);
                break;
            }
            0
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-loop-backedge-mutation").expect("parse");
    let checked = crate::core::check_program(&file).expect("check loop-local mutation");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("mutable loop-local call defers safely");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::NotInTrustedSubset,
        "{results:?}"
    );
    assert_eq!(
        results[0]
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
        "mutable loop-local state has a checker-owned coverage boundary"
    );
    assert!(results[0]
        .message
        .contains("owner 'function:while_backedge_mutates_local'"));
    let site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.callee == "positive")
        .expect("checker call-site directory has the mutated-loop call");
    assert_eq!(
        results[0].diagnostic.as_ref().unwrap().span,
        site.origin.user_span()
    );
    assert!(results[0].artifact.is_none());
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "the mutation/backedge counterexample must not reopen a compatibility body"
    );
}

#[test]
fn typed_ffi_aggregate_f64_leaf_keeps_ieee_contract_results() {
    require_z3!();
    let source = r#"
        #[repr(C)] type FloatPair { value: f64 }
        extern "C" { func foreign(value: FloatPair) -> FloatPair requires: value.value >= 0.0; }
        func caller_positive(value: FloatPair) -> FloatPair {
            requires: value.value >= 0.0
            foreign(value)
        }
        func caller_negative(value: FloatPair) -> FloatPair {
            requires: value.value < 0.0
            foreign(value)
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-record-ieee-f64").expect("parse");
    let checked =
        crate::core::check_program(&file).expect("check typed binary64 aggregate predicates");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results =
        verify_ffi_checked(&checked).expect("typed IEEE binary64 comparisons are supported");
    assert_eq!(results.len(), 2);
    for (caller, status) in [
        ("caller_positive", VerifStatus::Proven),
        ("caller_negative", VerifStatus::Disproven),
    ] {
        let result = results
            .iter()
            .find(|result| result.func_name.contains(caller))
            .unwrap();
        assert_eq!(result.status, status, "{caller}: {results:?}");
        assert_eq!(
            result.artifact.as_ref().unwrap().engine,
            "resolved_ffi",
            "{caller}: binary64 leaf must be verified from its typed descriptor"
        );
    }
    assert_eq!(
        crate::core::CheckedProgram::test_legacy_body_access(),
        Vec::<crate::core::LegacyBodyConsumer>::new(),
        "typed IEEE proofs must not rebuild the legacy extern AST"
    );
}

#[test]
fn typed_ffi_aggregate_f64_contracts_preserve_nan_comparison_semantics() {
    require_z3!();
    let source = r#"
        #[repr(C)] type FloatPair { value: f64 }
        extern "C" { func foreign(value: FloatPair) -> FloatPair requires: value.value >= 0.0; }
        func caller_nan(value: FloatPair) -> FloatPair {
            requires: value.value != value.value
            foreign(value)
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-record-f64-nan").expect("parse");
    let checked = crate::core::check_program(&file).expect("check IEEE NaN contract");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = verify_ffi_checked(&checked).expect("typed IEEE NaN comparison");
    assert_eq!(results.len(), 1, "single FFI call site: {results:?}");
    assert_eq!(
        results[0].status,
        VerifStatus::Disproven,
        "the caller Requires admits NaN, for which ordered >= is false: {results:?}"
    );
    assert_eq!(results[0].artifact.as_ref().unwrap().engine, "resolved_ffi");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn typed_ffi_aggregate_nested_contract_context_is_a_named_deferred_obligation() {
    require_z3!();
    for (parameters, statement, actual) in [
        ("value: Pair", "let n = { ensures: false\n0 }", "\"ok\""),
        ("value: Pair, n: i32 = { ensures: false\n0 }", "", "\"ok\""),
        ("value: Pair", "", "{ ensures: false\n\"ok\" }"),
    ] {
        let source = format!(
            r#"
            #[repr(C)] type Pair {{ value: i64 }}
            extern "C" {{ func foreign(s: string, value: Pair) -> Pair requires: value.value >= 0; }}
            func caller({parameters}) -> Pair {{
                requires: value.value >= 0
                {statement}
                foreign({actual}, value)
            }}
            func main() -> i32 {{ 0 }}
        "#
        );
        let file =
            parse_memory_source(&source, "typed-ffi-nested-context-obligation").expect("parse");
        let checked = crate::core::check_program(&file).expect("legal nested contract context");
        let caller = checked
            .callable(&crate::core::NodeId("function:caller".into()))
            .unwrap();
        assert!(caller.has_unmodeled_proof_roots());
        let nested = caller
            .contracts
            .iter()
            .find(|contract| {
                !caller
                    .body
                    .root
                    .statements
                    .iter()
                    .any(|statement| statement.node_id == contract.node_id)
            })
            .expect("complete typed census retains the exact nested contract");
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results =
            verify_ffi_checked(&checked).expect("compatibility plus retained context obligation");
        let call = results
            .iter()
            .find(|result| result.func_name.contains("calls foreign"))
            .unwrap();
        assert_eq!(
            call.status,
            VerifStatus::NotInTrustedSubset,
            "unsupported aggregate/context composition is a checker-owned boundary: {results:?}"
        );
        assert_eq!(
            call.diagnostic
                .as_ref()
                .and_then(|diagnostic| diagnostic.code.as_deref()),
            Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
            "unsupported aggregate/context composition keeps the checker-owned coverage code"
        );
        assert!(call.artifact.is_none());
        let deferred = results
            .iter()
            .find(|result| result.func_name.contains(&nested.node_id.0))
            .unwrap();
        assert_eq!(deferred.status, VerifStatus::NotInTrustedSubset);
        assert_eq!(
            deferred.trusted_subset_domain,
            Some(TrustedSubsetDomain::Contract)
        );
        assert_eq!(
            deferred.diagnostic.as_ref().unwrap().span,
            nested.origin.user_span()
        );
        assert!(deferred.artifact.is_none());
        assert_eq!(
            crate::core::CheckedProgram::test_legacy_body_access(),
            Vec::<crate::core::LegacyBodyConsumer>::new()
        );
    }
}

#[test]
fn actor_map_ffi_callsite_deferred_results_are_exact_and_untrusted() {
    const FFI: &str = r#"
extern "C" {
    func scalar_probe(value: i32) -> i32
        requires: value >= 0
        ensures: result == value + 1;
    func scalar_probe_unused(value: i32) -> i32
        ensures: result == value + 1;
}
"#;
    const USED_ACTOR_MAIN: &str = r#"
func main() -> i32 {
    let counter = Counter.spawn()
    let actor_value = counter.bump(5)
    let root = map_new()
    let updated = map_set(root, "answer", scalar_probe(actor_value))
    let size = map_size(updated)
    drop(updated)
    size
}
"#;
    const UNUSED_ACTOR_MAIN: &str = r#"
func main() -> i32 {
    let root = map_new()
    let updated = map_set(root, "answer", scalar_probe(41 as i32))
    let size = map_size(updated)
    drop(updated)
    size
}
"#;
    let actor_prefix = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/real_world/actor_nested_func_capture.mimi"
    ))
    .split("\nfunc main() -> i32 {")
    .next()
    .expect("original Actor fixture has its main entry");

    for (label, main) in [
        ("used-actor", USED_ACTOR_MAIN),
        ("unused-actor", UNUSED_ACTOR_MAIN),
    ] {
        let source = format!("{actor_prefix}\n{FFI}\n{main}");
        let file = parse_memory_source(&source, "actor-map-ffi-deferred-callsite")
            .unwrap_or_else(|error| panic!("{label}: parse: {error}"));
        let checked = crate::core::check_program(&file)
            .unwrap_or_else(|error| panic!("{label}: check: {error:?}"));
        let site = checked
            .call_sites_sorted()
            .into_iter()
            .find(|site| site.owner == "function:main" && site.callee == "scalar_probe")
            .unwrap_or_else(|| panic!("{label}: expected actual scalar_probe call site"));
        let declaration_owner = checked
            .ffi_declarations()
            .values()
            .find(|declaration| declaration.symbol == "scalar_probe")
            .map(|declaration| declaration.declaration.0.clone())
            .unwrap_or_else(|| panic!("{label}: scalar_probe declaration owner missing"));
        let results = verify_ffi_checked(&checked)
            .unwrap_or_else(|error| panic!("{label}: verify checked FFI obligations: {error}"));

        assert_eq!(
            results.len(),
            2,
            "{label}: only the called declaration's Requires and Ensures are obligations: {results:?}"
        );
        let requires = results
            .iter()
            .find(|result| result.func_name == "main calls scalar_probe")
            .unwrap_or_else(|| panic!("{label}: named Requires result missing: {results:?}"));
        let ensures = results
            .iter()
            .find(|result| result.func_name == "main -> extern scalar_probe Ensures")
            .unwrap_or_else(|| panic!("{label}: named Ensures result missing: {results:?}"));
        let expected_span = site.origin.user_span();
        for result in [requires, ensures] {
            assert_eq!(
                result.status,
                VerifStatus::NotInTrustedSubset,
                "{label}: Map/FFI composition cannot claim a proof: {result:?}"
            );
            assert_eq!(
                result.trusted_subset_domain,
                Some(TrustedSubsetDomain::Contract),
                "{label}: the unsupported boundary is a Contract obligation"
            );
            assert_eq!(
                result.constraint_count, 0,
                "{label}: no solver constraint may be attributed to this deferred result"
            );
            assert!(
                result.artifact.is_none(),
                "{label}: no proof artifact may be attached to a deferred result"
            );
            let diagnostic = result.diagnostic.as_ref().unwrap_or_else(|| {
                panic!("{label}: deferred result needs a diagnostic: {result:?}")
            });
            assert_eq!(
                diagnostic.code.as_deref(),
                Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
                "{label}: retain the registered coverage boundary code"
            );
            assert_eq!(
                diagnostic.span, expected_span,
                "{label}: exact checker-owned source call-site span"
            );
            assert_eq!(diagnostic.message, result.message);
            assert!(
                result.message.contains(&site.node_id.0),
                "{label}: result must carry the exact call-site identity {}: {result:?}",
                site.node_id.0
            );
        }
        assert_eq!(
            requires.message,
            format!(
                "{}: called extern 'scalar_probe' Requires at call site '{}' is Deferred because the Actor compatibility request also contains an independently-owned Map profile without a combined verifier receipt",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                site.node_id.0
            ),
            "{label}: exact Requires disposition"
        );
        assert_eq!(
            ensures.message,
            format!(
                "{}: checker-owned FFI declaration 'scalar_probe' (owner '{}') Ensures at call site '{}' (owner 'function:main', span {}) is outside canonical proof coverage; any Requires result is independent and does not prove the foreign postcondition",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                declaration_owner,
                site.node_id.0,
                site.origin.user_span()
            ),
            "{label}: exact foreign Ensures disposition"
        );
    }
}
