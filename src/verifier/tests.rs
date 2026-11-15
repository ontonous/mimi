use super::helpers::*;
use super::*;
use crate::ast::*;

#[test]
fn dual_coverage_keeps_mir_only_owner_visible() {
    let source = "func main() -> i32 { 0 }";
    let file = parse_memory_source(source, "dual-coverage-missing-peer").expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let primary = vec![VerificationResult {
        func_name: "resolved_owner".into(),
        status: VerifStatus::Proven,
        message: "Resolved proof".into(),
        diagnostic: None,
        duration_us: 0,
        constraint_count: 1,
        artifact: None,
        trusted_subset_domain: None,
    }];
    let boundaries = [crate::core::mir::verification::MirVerificationBoundary {
        owner: crate::core::NodeId("mir_only_owner".into()),
        point: None,
        reason: "canonical MIR proof result has no Resolved peer result".into(),
    }];

    let results = dual_verifier_coverage_results(&program, primary, &boundaries, "coverage-hash");
    let resolved = results
        .iter()
        .find(|result| result.func_name == "resolved_owner")
        .expect("Resolved owner remains visible");
    let mir = results
        .iter()
        .find(|result| result.func_name == "mir_only_owner")
        .expect("MIR-only owner remains visible");
    assert_eq!(resolved.status, VerifStatus::NotInTrustedSubset);
    assert_eq!(mir.status, VerifStatus::NotInTrustedSubset);
    assert_eq!(results.len(), 2);
    assert!(mir.message.contains("mir_only_owner"));
    assert!(mir.artifact.is_none());
}

#[test]
fn dual_coverage_never_erases_a_primary_counterexample() {
    let source = "func main() -> i32 { 0 }";
    let file = parse_memory_source(source, "dual-coverage-counterexample").expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let primary = vec![VerificationResult {
        func_name: "resolved_owner".into(),
        status: VerifStatus::Disproven,
        message: "Resolved counterexample".into(),
        diagnostic: None,
        duration_us: 0,
        constraint_count: 1,
        artifact: None,
        trusted_subset_domain: None,
    }];
    let boundary = [crate::core::mir::verification::MirVerificationBoundary {
        owner: crate::core::NodeId("resolved_owner".into()),
        point: None,
        reason: "canonical MIR peer result is missing".into(),
    }];

    let results = dual_verifier_coverage_results(&program, primary, &boundary, "coverage-hash");
    let rejected = results
        .iter()
        .find(|result| result.func_name == "resolved_owner")
        .expect("counterexample owner remains visible");
    assert_eq!(rejected.status, VerifStatus::Disproven);
    assert_eq!(rejected.message, "Resolved counterexample");
}

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
    assert_eq!(
        dual.status, exact_mir.status,
        "{}: dual={:?} ({}) exact_mir={:?} ({})",
        dual.func_name, dual.status, dual.message, exact_mir.status, exact_mir.message
    );
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

fn assert_route_mir_artifact(
    result: &VerificationResult,
    source_hash: &str,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) {
    let artifact = result
        .artifact
        .as_ref()
        .unwrap_or_else(|| panic!("{} has no MIR proof artifact", result.func_name));
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
        Some(receipt),
        "{}",
        result.func_name
    );
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
fn legacy_verifier_marks_nonready_v2_owners_not_in_trusted_subset() {
    require_z3!();
    use crate::core::mir::OrdinaryMapOwnedAnyV2Admission as Admission;

    let cases = [
        (
            Admission::StagedGetValues,
            r#"
type Label { text: string }
func v2_owner() -> i32 {
    ensures: result == 0
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    let pair = map_get(root, "label")
    let _found = pair.0
    drop(pair)
    drop(empty)
    drop(root)
    drop(label)
    0
}
func unrelated(x: i32) -> i32 {
    requires: x >= 0 && x < 2147483647
    ensures: result > x
    x + 1
}
func main() -> i32 { 0 }
"#,
            "v2_owner",
        ),
        (
            Admission::CompleteCoverage,
            r#"
type Label { text: string }
func v2_owner() -> i32 {
    ensures: result == 0
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    drop(empty)
    drop(root)
    drop(label)
    0
}
func unrelated(x: i32) -> i32 {
    requires: x >= 0 && x < 2147483647
    ensures: result > x
    x + 1
}
func main() -> i32 { 0 }
"#,
            "v2_owner",
        ),
        (
            Admission::IncompleteCoverage,
            r#"
type Label { text: string }
func v2_owner() -> i32 {
    ensures: result == 0
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    let unsupported = keys(root)
    drop(unsupported)
    drop(empty)
    drop(root)
    drop(label)
    0
}
func unrelated(x: i32) -> i32 {
    requires: x >= 0 && x < 2147483647
    ensures: result > x
    x + 1
}
func main() -> i32 { 0 }
"#,
            "v2_owner",
        ),
    ];

    for (expected_admission, source, owner_name) in cases {
        let file = parse_memory_source(source, "legacy-v2-coverage").expect("parse");
        let checked = crate::core::check_program(&file).expect("typecheck");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            expected_admission
        );

        let mut verifier = Verifier::new().expect("Z3 verifier");
        let results = verifier.verify_checked(&checked);
        let owner = results
            .iter()
            .find(|result| result.func_name == owner_name)
            .unwrap_or_else(|| panic!("missing owner-scoped V2 coverage result: {results:#?}"));
        assert_eq!(
            results
                .iter()
                .filter(|result| result.func_name == owner_name)
                .count(),
            1,
            "the V2 owner should receive exactly one coverage row: {results:#?}"
        );
        assert_eq!(owner.status, VerifStatus::NotInTrustedSubset, "{owner:#?}");
        assert!(
            owner.artifact.is_none(),
            "downgraded owner kept a proof artifact: {owner:#?}"
        );
        assert!(owner.message.contains("owner-v2"), "{owner:#?}");
        assert!(
            owner.diagnostic.as_ref().is_some_and(|diagnostic| {
                diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            }),
            "owner result must carry the route-coverage diagnostic: {owner:#?}"
        );

        let unrelated = results
            .iter()
            .find(|result| result.func_name == "unrelated")
            .expect("unrelated callable result remains in the batch");
        assert_eq!(unrelated.status, VerifStatus::Proven, "{unrelated:#?}");
        assert!(
            unrelated.artifact.is_some(),
            "unrelated proof artifact was lost"
        );
    }
}

#[test]
fn complete_v2_mir_analysis_cannot_be_composed_into_production_ready_route() {
    require_z3!();
    const {
        assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };

    let source = r#"
type Label { text: string }
func v2_owner() -> i32 {
    ensures: result == 0
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    drop(empty)
    drop(root)
    drop(label)
    0
}
func unrelated(x: i32) -> i32 {
    requires: x >= 0
    ensures: result > x
    x + 1
}
func main() -> i32 { 0 }
"#;
    let file = parse_memory_source(source, "complete-v2-ready-boundary").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owned_any_v2,
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage
    );

    let public_error = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect_err("public canonical route stays closed while V2 readiness is false");
    assert!(public_error
        .to_string()
        .contains("production-readiness gate is closed"));

    // This low-level MIR constructor is available for analysis and evidence.
    // The public proof API must still mark its V2 owner NITS so its result
    // cannot satisfy the production execution-readiness predicate.
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("low-level MIR analyzer input");
    let mismatched_receipt = canonical.route_receipt("verifier-mir-v1");
    let profile_error = verify_mir_with_route_receipt(
        &canonical,
        &mismatched_receipt,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect_err("a generic profile cannot hide a V2 MIR program");
    assert!(
        profile_error.contains("MIR-COVERAGE-001"),
        "{profile_error}"
    );

    let receipt = canonical.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
    let results = verify_mir_with_route_receipt(
        &canonical,
        &receipt,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("low-level analyzer returns diagnostic observations");

    let v2_owner_id = canonical
        .functions()
        .values()
        .find(|function| {
            function
                .ownership
                .has_checker_ordinary_map_owned_any_v2_actions()
        })
        .expect("complete V2 owner in materialized MIR")
        .owner
        .0
        .clone();
    let owner = results
        .iter()
        .find(|result| result.func_name == v2_owner_id)
        .expect("V2 owner remains visible in diagnostic results");
    assert_eq!(owner.status, VerifStatus::NotInTrustedSubset, "{owner:#?}");
    assert!(owner.artifact.is_none(), "{owner:#?}");
    assert!(owner.message.contains("diagnostic-only"), "{owner:#?}");
    assert_eq!(
        results
            .iter()
            .filter(|result| result.func_name == v2_owner_id)
            .count(),
        1,
        "public MIR verification must emit exactly one V2 owner boundary"
    );
    assert!(!canonical_execution_route_verifier_ready(
        &results, false, false
    ));
    let v2_receipt_proofs = results
        .iter()
        .filter(|result| result.func_name != v2_owner_id && result.artifact.is_some())
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        !v2_receipt_proofs.is_empty(),
        "unrelated proof rows retain their V2 route receipt"
    );
    assert!(!canonical_execution_route_verifier_ready(
        &v2_receipt_proofs,
        false,
        false
    ));

    let default_profile_results = verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("default MIR verifier binds V2 to its V2 receipt profile");
    let default_profile_owner = default_profile_results
        .iter()
        .find(|result| result.func_name == v2_owner_id)
        .expect("default MIR verifier keeps the V2 owner visible");
    assert_eq!(
        default_profile_owner.status,
        VerifStatus::NotInTrustedSubset,
        "{default_profile_owner:#?}"
    );
    assert!(default_profile_owner.artifact.is_none());
    assert!(!canonical_execution_route_verifier_ready(
        &default_profile_results,
        false,
        false
    ));

    let dual_results = verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("dual checked verifier uses the internal diagnostic route");
    let dual_owner = dual_results
        .iter()
        .find(|result| result.func_name == "v2_owner")
        .expect("dual checked verifier keeps the source-level V2 owner visible");
    assert_eq!(dual_owner.status, VerifStatus::NotInTrustedSubset);
    assert!(dual_owner.artifact.is_none());
    assert!(!canonical_execution_route_verifier_ready(
        &dual_results,
        false,
        false
    ));

    let unrelated = results
        .iter()
        .find(|result| result.func_name.ends_with("unrelated"))
        .expect("unrelated contract remains visible");
    assert_eq!(unrelated.status, VerifStatus::Proven, "{unrelated:#?}");
    assert!(unrelated.artifact.is_some());

    let ordinary_source = r#"
func ordinary(x: i32) -> i32 {
    requires: x >= 0
    ensures: result > x
    x + 1
}
func main() -> i32 { 0 }
"#;
    let ordinary_file = parse_memory_source(ordinary_source, "ordinary-ready-boundary")
        .expect("parse ordinary source");
    let ordinary_checked = crate::core::check_program(&ordinary_file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_canonical_mir_route_admission(&ordinary_checked)
            .ordinary_map_owned_any_v2,
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::OutsideProfile
    );
    let ordinary_route = crate::core::mir::materialize_canonical_mir_route(&ordinary_checked, None)
        .expect("ordinary canonical MIR route remains materializable");
    let ordinary_results = verify_mir(
        &ordinary_route.program,
        blake3::hash(ordinary_source.as_bytes())
            .to_hex()
            .to_string(),
    )
    .expect("ordinary MIR remains verifiable");
    assert!(canonical_execution_route_verifier_ready(
        &ordinary_results,
        false,
        false
    ));
    assert!(ordinary_results.iter().any(|result| {
        result.func_name.ends_with("ordinary") && result.status == VerifStatus::Proven
    }));
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
        .contains("MapAnyOwner v1 profile cannot be combined"));
    assert!(result.message.contains("scalar C export"));
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
    let file = parse_memory_source(src, "ffi-no-requires").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let call_sites = checked
        .call_sites_sorted()
        .into_iter()
        .filter(|site| site.kind == crate::core::ResolvedCallKind::Extern)
        .collect::<Vec<_>>();
    assert_eq!(
        call_sites.len(),
        1,
        "the source has one checked extern call"
    );
    assert_eq!(call_sites[0].callee, "get_value");
    let results = verify_ffi_checked(&checked).expect("checked FFI verification");
    assert!(
        results.is_empty(),
        "no-contract extern has no obligations: {results:?}"
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
    let literal_profile =
        crate::core::mir::reference::from_checked_program_ffi_literal_contracts(&checked)
            .expect("Checker literal profile construction")
            .expect("literal FFI profile is admitted");
    assert_eq!(
        literal_profile.program().purpose(),
        crate::core::mir::reference::MirProgramPurpose::VerifierOnly
    );
    assert_eq!(
        literal_profile.route_receipt().profile.as_str(),
        crate::core::mir::reference::MIR_FFI_LITERAL_CONTRACT_PROFILE_V1
    );

    let results = verify_checked_dual(&checked, source_hash.clone())
        .expect("literal non-scalar FFI dual verification");
    assert_eq!(results.len(), 1, "declaration-level result: {results:?}");
    assert_eq!(results[0].func_name, "extern host_text");
    assert_eq!(results[0].status, VerifStatus::Proven);
    assert_eq!(results[0].constraint_count, 1);
    assert!(results[0].diagnostic.is_none());
    let artifact = results[0].artifact.as_ref().expect("literal artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(
        artifact.proof_profile,
        ProofArtifact::PROFILE_MIR_FFI_EXPERIMENTAL_V1
    );
    assert_eq!(
        artifact.obligation_scope,
        ProofArtifact::SCOPE_LITERAL_FFI_DECLARATION_V1
    );
    assert_eq!(
        artifact
            .mir_route_receipt
            .as_ref()
            .map(|receipt| receipt.profile.as_str()),
        Some(crate::core::mir::reference::MIR_FFI_LITERAL_CONTRACT_PROFILE_V1)
    );
    assert_eq!(artifact.source_hash, source_hash);
    assert_eq!(artifact.integer_model, "literal-boolean");
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
    let results = verify_checked_dual(&checked, "dual-scalar-literal-ffi".into())
        .expect("scalar FFI dual verification");
    assert_eq!(results.len(), 1, "canonical scalar result: {results:?}");
    let artifact = results[0].artifact.as_ref().expect("MIR artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
}

#[test]
fn scalar_ffi_literal_contract_rejects_map_owner_v2_and_observer_composition() {
    require_z3!();

    // A scalar-only literal FFI request keeps its existing canonical MIR
    // behavior; the owner composition guard must be specific to mixed roots.
    let pure_ffi_source = r#"
extern "C" {
    func host_i32(value: i32) -> i32
        requires: true;
}
func main() -> i32 { host_i32(1) }
"#;
    let pure_ffi_file = parse_memory_source(pure_ffi_source, "pure-scalar-ffi-literal")
        .expect("parse pure scalar FFI source");
    let pure_ffi_checked =
        crate::core::check_program(&pure_ffi_file).expect("check pure scalar FFI source");
    let pure_results = verify_ffi_checked_with_source_hash(
        &pure_ffi_checked,
        blake3::hash(pure_ffi_source.as_bytes())
            .to_hex()
            .to_string(),
    )
    .expect("pure scalar FFI literal contract remains admitted");
    assert_eq!(pure_results.len(), 1, "pure FFI result: {pure_results:?}");
    assert_eq!(
        pure_results[0]
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR),
        "pure scalar FFI keeps its canonical MIR receipt"
    );

    let ffi_source = r#"
extern "C" {
    func host_i32(value: i32) -> i32
        requires: true;
}
func ffi_probe() -> i32 { host_i32(1) }
"#;
    let map_cases = [
        (
            "observer-v1",
            include_str!("../../tests/fixtures/mir_ordinary_map_owner_observer_v1.mimi"),
        ),
        (
            "owner-v2",
            include_str!("../../tests/fixtures/mir_ordinary_map_owned_any_v2_clone.mimi"),
        ),
    ];

    for (label, map_source) in map_cases {
        let source = format!("{ffi_source}\n{map_source}");
        let file = parse_memory_source(&source, label).expect("parse mixed Map/FFI source");
        let checked = crate::core::check_program(&file).expect("check mixed Map/FFI source");
        let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(
            admission.scalar_ffi,
            "{label}: scalar FFI signal missing: {admission:?}"
        );
        assert!(
            match label {
                "observer-v1" => admission
                    .ordinary_map_owner_lifecycle_observer_v1
                    .has_profile_signal(),
                "owner-v2" => admission.ordinary_map_owned_any_v2.has_profile_signal(),
                _ => unreachable!("known test case"),
            },
            "{label}: expected Map profile signal: {admission:?}"
        );

        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        match super::verify_dual_isolated_ffi_mir_route(&checked, &source_hash) {
            Some(Err(error)) => assert!(
                error.contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
                "{label}: expected a route-composition diagnostic, got {error}"
            ),
            Some(Ok(results)) => {
                panic!("{label}: isolated FFI route returned FFI-only success: {results:?}")
            }
            None => panic!("{label}: mixed literal FFI route was not rejected"),
        }

        let single_error = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
            .expect_err("single FFI verifier must reject mixed Map owner composition");
        assert!(
            single_error.contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
            "{label}: {single_error}"
        );
        let dual_error = verify_checked_dual(&checked, source_hash)
            .expect_err("dual verifier must reject mixed Map owner composition");
        assert!(
            dual_error.contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
            "{label}: {dual_error}"
        );
    }
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
    let file = parse_memory_source(source, "literal-non-scalar-ffi").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let call_sites = checked
        .call_sites_sorted()
        .into_iter()
        .filter(|site| site.kind == crate::core::ResolvedCallKind::Extern)
        .collect::<Vec<_>>();
    assert_eq!(call_sites.len(), 1, "the literal root has one checked call");
    let roots = checked.declaration_expression_bodies();
    assert!(roots.values().any(|root| {
        root.kind == crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternRequires
            && root.body.is_ok()
    }));
    let results = verify_ffi_checked(&checked).expect("literal non-scalar FFI verification");
    assert_eq!(results.len(), 1, "declaration-level result: {results:?}");
    assert_eq!(results[0].func_name, "extern host_text");
    assert_eq!(results[0].status, VerifStatus::Proven);
    assert_eq!(results[0].constraint_count, 1);
    assert!(results[0].diagnostic.is_none());
    let artifact = results[0].artifact.as_ref().expect("literal artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(
        artifact.proof_profile,
        ProofArtifact::PROFILE_MIR_FFI_EXPERIMENTAL_V1
    );
    assert_eq!(
        artifact
            .mir_route_receipt
            .as_ref()
            .map(|receipt| receipt.profile.as_str()),
        Some(crate::core::mir::reference::MIR_FFI_LITERAL_CONTRACT_PROFILE_V1)
    );
    assert_eq!(artifact.integer_model, "literal-boolean");
    assert_eq!(
        artifact.float_model,
        crate::core::mir::types::MIR_FFI_VERIFIER_FLOAT_MODEL
    );
}

#[test]
fn false_literal_non_scalar_ffi_is_disproven_without_legacy() {
    let source = r#"
extern "C" {
    func host_text(value: string) -> i32
        requires: false;
}
func main() -> i32 { host_text("ok") }
"#;
    let file = parse_memory_source(source, "false-literal-non-scalar-ffi").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let call_site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:main" && site.callee == "host_text")
        .expect("checker-owned callsite for the false literal contract");
    let results = verify_ffi_checked(&checked).expect("false literal FFI verification");
    assert_eq!(results.len(), 1, "declaration-level result: {results:?}");
    assert_eq!(results[0].func_name, "extern host_text");
    assert_eq!(results[0].status, VerifStatus::Disproven);
    assert_eq!(results[0].constraint_count, 1);
    assert!(results[0]
        .message
        .contains("unsatisfiable literal requires"));
    assert!(results[0].message.contains("owner 'function:main'"));
    let diagnostic = results[0]
        .diagnostic
        .as_ref()
        .expect("call-site diagnostic");
    assert_eq!(diagnostic.span.start_line, 6);
    assert_eq!(diagnostic.span, call_site.origin.user_span());
}

#[test]
fn false_literal_non_scalar_ffi_ensures_is_disproven_without_legacy() {
    let source = r#"
extern "C" {
    func host_text(value: string) -> i32
        ensures: false;
}
func main() -> i32 { host_text("ok") }
"#;
    let file = parse_memory_source(source, "false-literal-non-scalar-ensures").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let call_site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:main" && site.callee == "host_text")
        .expect("checker-owned callsite for the false literal Ensures");
    let results = verify_ffi_checked(&checked).expect("false literal Ensures verification");
    assert_eq!(results.len(), 1, "declaration-level result: {results:?}");
    assert_eq!(results[0].func_name, "extern host_text");
    assert_eq!(results[0].status, VerifStatus::Disproven);
    assert_eq!(results[0].constraint_count, 1);
    assert!(
        results[0]
            .message
            .contains("literal declaration ensures contract is disproven"),
        "{}",
        results[0].message
    );
    let diagnostic = results[0]
        .diagnostic
        .as_ref()
        .expect("call-site diagnostic");
    assert_eq!(diagnostic.span.start_line, 6);
    assert_eq!(diagnostic.span, call_site.origin.user_span());
    let artifact = results[0].artifact.as_ref().expect("literal artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(
        artifact
            .mir_route_receipt
            .as_ref()
            .map(|receipt| receipt.profile.as_str()),
        Some(crate::core::mir::reference::MIR_FFI_LITERAL_CONTRACT_PROFILE_V1)
    );
    assert_eq!(artifact.integer_model, "literal-boolean");
}

#[test]
fn no_z3_ffi_presence_census_uses_typed_contract_roots() {
    let source = r#"
extern "C" {
    func called(value: string) -> i32 requires: true;
    func dormant(value: string) -> i32 ensures: false;
}
func main() -> i32 { called("ok") }
"#;
    let file = parse_memory_source(source, "no-z3-ffi-roots").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let results = super::ctx::mock_verify_checked(&checked);
    let ffi_results = results
        .iter()
        .filter(|result| result.func_name.starts_with("extern "))
        .collect::<Vec<_>>();
    assert_eq!(ffi_results.len(), 2, "all declared roots are counted");
    assert!(ffi_results.iter().all(|result| {
        result.status == VerifStatus::InfrastructureError
            && result.message == "Z3 solver not available"
            && result.constraint_count == 0
    }));
    assert!(ffi_results
        .iter()
        .any(|result| result.func_name == "extern called"));
    assert!(ffi_results
        .iter()
        .any(|result| result.func_name == "extern dormant"));
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
    let error = verify_checked_dual(&checked, "dual-nonliteral-ffi".into())
        .expect_err("non-literal non-scalar FFI must remain outside the slice");
    assert!(
        error.contains(crate::core::mir::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE),
        "{error}"
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

    let single_error = verify_checked(&checked, source_hash.clone())
        .expect_err("single verifier must reject unsupported called extern ABI");
    assert!(
        single_error.contains(crate::core::mir::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE),
        "{single_error}"
    );

    let dual_error = verify_checked_dual(&checked, source_hash)
        .expect_err("dual verifier must reject unsupported called extern ABI");
    assert_eq!(single_error, dual_error);
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
            include_str!(
                "../../tests/fixtures/mir_native_generic_record_projection_eight_field_rejected.mimi"
            ),
            "generic record projection candidate",
        ),
        (
            "unsupported generic record update",
            include_str!(
                "../../tests/fixtures/mir_native_generic_record_update_owned_copy_residual_f64_rejected.mimi"
            ),
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

        let single_error = match verify_checked(&checked, source_hash.clone()) {
            Ok(_) => panic!("single verifier accepted {label}"),
            Err(error) => error,
        };
        assert!(
            single_error.contains(expected_reason),
            "{label}: {single_error}"
        );

        let dual_error = match verify_checked_dual(&checked, source_hash) {
            Ok(_) => panic!("dual verifier accepted {label}"),
            Err(error) => error,
        };
        assert_eq!(single_error, dual_error, "{label}");
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
    let file = parse_memory_source(src, "ffi-requires-always-satisfied").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let call_site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:caller" && site.callee == "read")
        .expect("checker-owned read callsite");
    let source_hash = blake3::hash(src.as_bytes()).to_hex().to_string();
    let results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
        .expect("checked FFI verification");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Proven,
        "requires fd >= 0 && size > 0 should satisfy read's preconditions: {}",
        results[0].message
    );
    assert_eq!(results[0].func_name, "function:caller");
    assert!(results[0].message.contains(&call_site.node_id.0));
    let artifact = results[0]
        .artifact
        .as_ref()
        .expect("canonical FFI proof artifact");
    assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.mir_hash.len(), 64);
    assert_eq!(artifact.source_hash, source_hash);
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
    let file = parse_memory_source(src, "ffi-requires-violated").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let call_site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:bad_caller" && site.callee == "read")
        .expect("checker-owned failed read callsite");
    let source_hash = blake3::hash(src.as_bytes()).to_hex().to_string();
    let results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
        .expect("checked FFI verification");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Disproven,
        "read(-1, 0, size) should fail: fd is negative"
    );
    assert_eq!(results[0].func_name, "function:bad_caller");
    assert!(results[0].message.contains(&call_site.node_id.0));
    let artifact = results[0]
        .artifact
        .as_ref()
        .expect("canonical FFI counterexample artifact");
    assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.mir_hash.len(), 64);
    assert_eq!(artifact.source_hash, source_hash);
    let diagnostic = results[0]
        .diagnostic
        .as_ref()
        .expect("extern call-site diagnostic");
    assert_eq!(diagnostic.span.start_line, 7);
    assert_eq!(diagnostic.span.start_col, 5);
    assert_eq!(diagnostic.span.end_line, 7);
    assert_eq!(diagnostic.span.end_col, 22);
    assert_eq!(diagnostic.span, call_site.origin.user_span());
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

        // The public CheckedProgram APIs must pick this same checker-owned
        // scalar FFI MIR route before their retained AST verifier owners.
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        crate::core::mir::reset_test_route_materialization_count();
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

        crate::core::mir::reset_test_route_materialization_count();
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
    }
}

#[test]
fn scalar_ffi_mir_proofs_preserve_path_facts_and_fresh_external_results() {
    require_z3!();
    for (body, proven, disproven) in [
        (
            "let x = unknown(); if x > (0 as i64) { positive(x) } else { positive(1 as i64) }",
            2,
            0,
        ),
        (
            "let x = unknown(); if x > (0 as i64) { positive(x) } else { positive(x) }",
            1,
            1,
        ),
        (
            "let first = unknown(); let second = unknown(); if first == second { positive(1 as i64) } else { positive(-1 as i64) }",
            1,
            1,
        ),
        ("ensures: true\npositive(-1 as i64)", 1, 1),
        ("unknown()", 0, 0),
    ] {
        let source = format!(
            "extern \"C\" {{ func unknown() -> i64; func positive(x: i64) -> i64 requires: x > 0; }} func main() -> i64 {{ {body} }}"
        );
        let file =
            crate::parser::Parser::new(crate::lexer::Lexer::new(&source).tokenize().unwrap())
                .parse_file()
                .unwrap();
        let checked = crate::core::check_program(&file).unwrap();
        let program =
            crate::core::mir::reference::MirProgram::from_checked_program(&checked).unwrap();
        let results = verify_mir(&program, "ffi-paths".into()).unwrap();
        assert_eq!(
            results
                .iter()
                .filter(|r| r.status == VerifStatus::Proven)
                .count(),
            proven,
            "{body}: {results:?}"
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| r.status == VerifStatus::Disproven)
                .count(),
            disproven,
            "{body}: {results:?}"
        );
        assert_eq!(results.len(), proven + disproven, "{body}: {results:?}");
        assert!(
            results
                .iter()
                .all(|result| result.func_name == "function:main"),
            "only per-callsite FFI results belong in this batch: {body}: {results:?}"
        );
        assert!(results.iter().all(|result| {
            result.artifact.as_ref().is_some_and(|artifact| {
                artifact.engine == ProofArtifact::ENGINE_MIR
                    && artifact.mir_hash == program.canonical_digest()
            })
        }));
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
        ("x < 0 && 1 / 0 > 0", 4, VerifStatus::NotInTrustedSubset),
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
        if expected == VerifStatus::NotInTrustedSubset {
            assert!(results[0].artifact.is_none(), "{condition}: {results:?}");
            assert!(results[0]
                .message
                .contains("typed extern requires root is Deferred"));
        } else {
            assert_eq!(
                results[0].artifact.as_ref().unwrap().mir_hash,
                program.canonical_digest()
            );
        }
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
fn checked_ffi_unconstrained_symbolic_string_disproves_and_unsupported_caller_defers() {
    require_z3!();
    let source = r#"
extern "C" {
    func strlen(s: string) -> i64
        requires: s != "";
}
func caller(s: string) -> i64 {
    strlen(s)
}
"#;
    let file = parse_memory_source(source, "checked-ffi-unconstrained-string").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let call_site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:caller" && site.callee == "strlen")
        .expect("checker-owned symbolic String callsite");
    let results = verify_ffi_checked(&checked).expect("checked String FFI verification");
    assert_eq!(
        results.len(),
        1,
        "one obligation remains visible: {results:?}"
    );
    assert_eq!(results[0].status, VerifStatus::Disproven, "{results:?}");
    let artifact = results[0]
        .artifact
        .as_ref()
        .expect("counterexample keeps canonical MIR owner-slice artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(
        artifact
            .mir_route_receipt
            .as_ref()
            .map(|receipt| receipt.profile.as_str()),
        Some("verifier-ffi-owner-slice-v1")
    );
    assert_eq!(
        results[0].diagnostic.as_ref().unwrap().span,
        call_site.origin.user_span(),
        "the counterexample belongs to the actual checked callsite"
    );

    // Unsupported caller-side String equality predicates keep the explicit boundary.
    let deferred_source = r#"
extern "C" {
    func strlen(s: string) -> i64
        requires: s != "";
}
func caller(s: string) -> i64 {
    requires: s != "ok"
    strlen(s)
}
"#;
    let deferred_file =
        parse_memory_source(deferred_source, "checked-ffi-unsupported-string-caller")
            .expect("parse");
    let deferred_checked = crate::core::check_program(&deferred_file).expect("check");
    let deferred_call_site = deferred_checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:caller" && site.callee == "strlen")
        .expect("checker-owned deferred String callsite");
    let deferred_results =
        verify_ffi_checked(&deferred_checked).expect("checked String FFI boundary");
    assert_eq!(
        deferred_results.len(),
        1,
        "one obligation remains visible: {deferred_results:?}"
    );
    assert_eq!(
        deferred_results[0].status,
        VerifStatus::NotInTrustedSubset,
        "{deferred_results:?}"
    );
    assert!(
        deferred_results[0].artifact.is_none(),
        "deferred proofs have no artifact"
    );
    assert!(
        deferred_results[0]
            .message
            .contains("String-valued FFI caller predicates"),
        "the deferral must state its semantic boundary: {deferred_results:?}"
    );
    assert_eq!(
        deferred_results[0]
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    );
    assert_eq!(
        deferred_results[0].diagnostic.as_ref().unwrap().span,
        deferred_call_site.origin.user_span(),
        "the explicit boundary belongs to the actual checked callsite"
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
    let file = parse_memory_source(src, "ffi-string-empty-protected").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let call_site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:caller" && site.callee == "strlen")
        .expect("checker-owned protected strlen callsite");
    let source_hash = blake3::hash(src.as_bytes()).to_hex().to_string();
    let results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
        .expect("checked FFI verification");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Proven,
        "strlen(s) with guard should be Proven: {}",
        results[0].message
    );
    assert!(results[0].message.contains(&call_site.node_id.0));
    let artifact = results[0].artifact.as_ref().expect("checked MIR proof");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.source_hash, source_hash);
    assert!(results[0].diagnostic.is_none());
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
    let file = parse_memory_source(src, "ffi-multiple-externs").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let call_sites = checked
        .call_sites_sorted()
        .into_iter()
        .filter(|site| site.kind == crate::core::ResolvedCallKind::Extern)
        .collect::<Vec<_>>();
    assert_eq!(
        call_sites.len(),
        4,
        "both declarations are called at each owner"
    );
    let source_hash = blake3::hash(src.as_bytes()).to_hex().to_string();
    let results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
        .expect("checked multi-extern FFI verification");
    assert_eq!(results.len(), 4);
    let ok_results: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.ends_with("ok_caller"))
        .collect();
    assert_eq!(ok_results.len(), 2);
    assert!(
        ok_results.iter().all(|r| r.status == VerifStatus::Proven),
        "ok_caller should pass: {:?}",
        ok_results
    );
    let bad_results: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.ends_with("bad_caller"))
        .collect();
    assert_eq!(bad_results.len(), 2);
    assert!(
        bad_results
            .iter()
            .any(|r| r.status == VerifStatus::Disproven),
        "bad_caller should have at least one failure: {:?}",
        bad_results
    );
    for result in &results {
        let site = call_sites
            .iter()
            .find(|site| result.message.contains(&site.node_id.0))
            .unwrap_or_else(|| {
                panic!("result must identify exactly one checked callsite: {result:?}")
            });
        assert_eq!(
            call_sites
                .iter()
                .filter(|candidate| result.message.contains(&candidate.node_id.0))
                .count(),
            1,
            "one proof result must not alias multiple callsites: {result:?}"
        );
        let artifact = result.artifact.as_ref().expect("MIR callsite artifact");
        assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.source_hash, source_hash);
        if result.status == VerifStatus::Disproven {
            assert_eq!(
                result.diagnostic.as_ref().unwrap().span,
                site.origin.user_span(),
                "the failed call must retain its exact callsite span: {result:?}"
            );
        }
    }
    for site in &call_sites {
        assert_eq!(
            results
                .iter()
                .filter(|result| result.message.contains(&site.node_id.0))
                .count(),
            1,
            "every checked FFI callsite must have one result: {site:?}, {results:?}"
        );
    }
    assert!(
        results.iter().all(|result| {
            result.artifact.as_ref().is_some_and(|artifact| {
                artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
            })
        }),
        "all direct scalar multi-FFI results must be MIR artifacts: {results:?}"
    );
}

fn verify_entry_invariant_without_legacy_access(source: &str) -> Vec<VerificationResult> {
    let file = parse_memory_source(source, "entry-invariant-owner-retirement").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let single = verify_checked(&checked, source_hash.clone()).expect("typed entry invariant");
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
    let dual =
        verify_checked_dual(&checked, source_hash).expect("independent typed invariant dual");
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

    let verify_result = verify_checked(&program, source_hash.clone()).expect("MIR verify");
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

    let dual_result = verify_checked_dual(&program, source_hash.clone()).expect("MIR dual verify");
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
        let checked_results = crate::verifier::verify_checked(&checked, source_hash.clone())
            .unwrap_or_else(|error| panic!("verify_checked {}: {error}", profile.as_str()));
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

        let dual_results = crate::verifier::verify_checked_dual(&checked, source_hash)
            .unwrap_or_else(|error| panic!("verify_checked_dual {}: {error}", profile.as_str()));
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
        // This reaches only the existing no-FFI preguard. The dual route
        // materialization counter below locks the normal canonical path.
        assert!(
            super::verify_dual_isolated_ffi_mir_route(&checked, &source_hash).is_none(),
            "an FFI-free generic identity request must stay with the ordinary dual route"
        );

        // This profile is deliberately contract-free: adding a contract
        // would widen its exact whole-program identity shape. The route
        // materialization counter therefore proves the public verifier chose
        // and validated the canonical profile before returning no obligations.
        crate::core::mir::reset_test_route_materialization_count();
        let checked_results = crate::verifier::verify_checked(&checked, source_hash.clone())
            .expect("public checked generic identity verifier");
        assert!(checked_results.is_empty(), "identity has no contracts");
        assert_eq!(crate::core::mir::test_route_materialization_count(), 1);

        crate::core::mir::reset_test_route_materialization_count();
        let dual_results = crate::verifier::verify_checked_dual(&checked, source_hash)
            .expect("public dual checked generic identity verifier");
        assert!(dual_results.is_empty(), "identity has no contracts");
        assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
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
        let (exact_mir_results, typed_receipt) =
            verify_exact_typed_mir_request_for_test(&checked, &source_hash);

        for dual in [false, true] {
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
            if has_proof {
                let exact_mir = exact_mir_result_for_test(&exact_mir_results, "unwrap_copy");
                assert_eq!(
                    exact_mir.status,
                    VerifStatus::Proven,
                    "{} exact MIR profile proof: {}",
                    profile.as_str(),
                    exact_mir.message
                );
                assert_single_exact_mir_artifact(exact_mir, &source_hash, &typed_receipt);
                let result = results
                    .iter()
                    .find(|result| result.func_name == "unwrap_copy")
                    .unwrap_or_else(|| {
                        panic!("{} omitted unwrap_copy: {results:?}", profile.as_str())
                    });
                assert_eq!(result.status, exact_mir.status, "{}", result.message);
                if dual {
                    // Dual verification preserves the Resolved primary artifact
                    // when its verdict agrees with canonical MIR. Check the
                    // independent MIR proof directly above, then assert that
                    // the merged result keeps the agreed verdict and a valid
                    // source-bound primary artifact.
                    assert_dual_result_keeps_the_agreed_engine_artifact(
                        result,
                        exact_mir,
                        &source_hash,
                        &typed_receipt,
                    );
                } else {
                    assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);
                }
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
fn canonical_mir_bool_result_postconditions_keep_flow_verdicts_through_dual() {
    require_z3!();
    // These six outcomes were established by the Flow/VIR reference before
    // its test-only AST walker was retired. Keep each positive and negative
    // result on the canonical MIR path instead of forcing this fixture into
    // compatibility merely to exercise the old engine.
    let source = r#"
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
    let file = parse_memory_source(source, "bool-result-mir").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (exact_mir, receipt) = verify_exact_typed_mir_request_for_test(&checked, &source_hash);

    // Observe the Resolved primary directly as well as the independent MIR
    // request below. A merged dual result alone can inherit the MIR verdict
    // when Resolved is NoOpinion, so it cannot establish that both engines
    // actually judged these six owners.
    let mut resolved_verifier = Verifier::new().expect("Resolved verifier");
    resolved_verifier.set_source_hash(source_hash.clone());
    let resolved_results = resolved_verifier.verify_checked(&checked);

    for dual in [false, true] {
        let public = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .unwrap_or_else(|error| {
            panic!(
                "{} bool-result verifier: {error}",
                if dual { "dual" } else { "single" }
            )
        });

        for (name, expected) in [
            ("true_value", VerifStatus::Proven),
            ("false_claim", VerifStatus::Disproven),
            ("not_false", VerifStatus::Proven),
            ("same", VerifStatus::Proven),
            ("flip", VerifStatus::Proven),
            ("integer_claim", VerifStatus::Disproven),
        ] {
            let result = exact_mir_result_for_test(&public, name);
            let exact = exact_mir_result_for_test(&exact_mir, name);
            let resolved = exact_mir_result_for_test(&resolved_results, name);
            assert_eq!(
                exact.status, expected,
                "exact MIR {name}: {}",
                exact.message
            );
            assert_eq!(
                resolved.status, expected,
                "Resolved {name}: {}",
                resolved.message
            );
            assert_single_exact_mir_artifact(exact, &source_hash, &receipt);
            let resolved_artifact = resolved
                .artifact
                .as_ref()
                .unwrap_or_else(|| panic!("Resolved {name} must carry proof provenance"));
            assert_eq!(
                resolved_artifact.engine,
                ProofArtifact::ENGINE_RESOLVED,
                "{name}"
            );
            assert_eq!(resolved_artifact.source_hash, source_hash, "{name}");
            assert!(
                !resolved_artifact.resolved_ir_hash.is_empty(),
                "Resolved {name} must carry its own IR identity"
            );
            assert!(resolved_artifact.mir_hash.is_empty(), "{name}");
            assert!(resolved_artifact.mir_route_receipt.is_none(), "{name}");
            assert_eq!(result.status, expected, "public {name}: {}", result.message);
            if dual {
                assert_eq!(result.status, resolved.status, "dual vs Resolved {name}");
                assert_eq!(result.status, exact.status, "dual vs MIR {name}");
                let dual_artifact = result
                    .artifact
                    .as_ref()
                    .unwrap_or_else(|| panic!("dual {name} must retain agreed provenance"));
                assert_eq!(
                    dual_artifact.engine,
                    ProofArtifact::ENGINE_RESOLVED,
                    "dual {name} must retain the observed Resolved primary"
                );
                assert_eq!(
                    dual_artifact.source_hash, resolved_artifact.source_hash,
                    "{name}"
                );
                assert_eq!(
                    dual_artifact.resolved_ir_hash, resolved_artifact.resolved_ir_hash,
                    "dual {name} must retain the independently observed Resolved identity"
                );
                assert_dual_result_keeps_the_agreed_engine_artifact(
                    result,
                    exact,
                    &source_hash,
                    &receipt,
                );
            } else {
                assert_single_exact_mir_artifact(result, &source_hash, &receipt);
            }
        }
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
        let public_results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("public checked verifier");
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
    let mut direct_native =
        crate::codegen::CodeGenerator::new(&context, "checked_scalar_contracts");
    direct_native
        .compile_checked(&checked)
        .expect("direct native shared route");
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
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("verify library without AST fallback");
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
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("verify canonical helper graph");
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
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("verify canonical unit returns");
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
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("verify actual expression types");
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
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("verify short-circuit guard obligations");
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
        let results = if dual {
            verify_checked_dual(&checked, source_hash.clone())
        } else {
            verify_checked(&checked, source_hash.clone())
        }
        .expect("verify argument-specific contract safety");
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
fn nested_requires_in_expression_block_is_rejected_before_verifier_routing() {
    let source = r#"
        func guarded(value: i32) -> i32 {
            ensures: value > 0
            value + {
                requires: value > 0
                0
            }
        }

        func nested_ensures(value: i32) -> i32 {
            if value > 0 {
                ensures: true
                value
            } else {
                value
            }
        }

        func main() -> i32 { guarded(1) }
    "#;
    let file = parse_memory_source(source, "legacy-body-owner-audit").expect("parse");
    let diagnostics = crate::core::check_program(&file)
        .expect_err("nested Requires has no scoped semantics in the language contract");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0503)
                && diagnostic.message.contains("requires")
        }),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0503)
                && diagnostic.message.contains("ensures")
        }),
        "{diagnostics:?}"
    );
    let source_error = verify_source(source).expect_err("single source verifier checks first");
    assert!(
        source_error.contains("top-level callable-body contract"),
        "{source_error}"
    );
}

#[test]
fn nested_function_contract_scan_uses_a_fresh_callable_root() {
    let source = r#"
func outer(value: i32) -> i32 {
    func inner(input: i32) -> i32 {
        requires: input >= 0
        input + {
            ensures: true
            0
        }
    }
    inner(value)
}
func main() -> i32 { outer(1) }
"#;
    let file = parse_memory_source(source, "nested-function-contract-scope").expect("parse");
    let diagnostics = crate::core::check_program(&file)
        .expect_err("the nested body block contract must be rejected");
    let nested_contracts: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0503))
        .collect();
    assert_eq!(nested_contracts.len(), 1, "{diagnostics:?}");
    assert!(
        nested_contracts[0].message.contains("ensures"),
        "the nested function's root Requires remains valid: {diagnostics:?}"
    );
}

#[test]
fn fstring_interpolation_nested_contract_is_rejected() {
    let source = r#"
func render(value: i32) -> string {
    f"{ { requires: false; value } }"
}
func main() -> i32 { 0 }
"#;
    let file = parse_memory_source(source, "fstring-interpolation-contract-scope").expect("parse");
    let diagnostics = crate::core::check_program(&file)
        .expect_err("the interpolation block contract must be rejected");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0503)
                && diagnostic.message.contains("requires")
        }),
        "{diagnostics:?}"
    );
}

#[test]
fn parameter_default_nested_contract_is_rejected_and_plain_default_remains_valid() {
    let valid_source = r#"
func identity(value: i32 = 7) -> i32 { value }
func main() -> i32 { identity() }
"#;
    let valid_file = parse_memory_source(valid_source, "plain-parameter-default").expect("parse");
    crate::core::check_program(&valid_file).expect("ordinary parameter defaults remain valid");

    let invalid_source = r#"
func invalid(value: i32 = { requires: false; 7 }) -> i32 { value }
func main() -> i32 { 0 }
"#;
    let invalid_file =
        parse_memory_source(invalid_source, "nested-contract-parameter-default").expect("parse");
    let diagnostics = crate::core::check_program(&invalid_file)
        .expect_err("contracts inside a parameter default are outside the callable body");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0503)
                && diagnostic.message.contains("requires")
        }),
        "{diagnostics:?}"
    );
}

#[test]
fn lambda_parameter_default_nested_contract_is_rejected() {
    let source = r#"
func apply() -> i32 {
    let identity = fn(value: i32 = { requires: false; 7 }) -> i32 { value }
    identity()
}
func main() -> i32 { apply() }
"#;
    let file = parse_memory_source(source, "lambda-parameter-default-contract").expect("parse");
    let diagnostics = crate::core::check_program(&file)
        .expect_err("lambda parameter defaults have no callable-body contract root");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0503)
                && diagnostic.message.contains("requires")
        }),
        "{diagnostics:?}"
    );
}

#[test]
fn non_callable_expression_roots_reject_nested_contracts() {
    let cases = [
        (
            "extern requires",
            r#"
extern "C" {
    func foreign(value: i32) -> i32 requires: { requires: true; value > 0; };
}
func main() -> i32 { 0 }
"#,
            "requires",
        ),
        (
            "extern ensures",
            r#"
extern "C" {
    func foreign(value: i32) -> i32 ensures: { ensures: true; result > 0; };
}
func main() -> i32 { 0 }
"#,
            "ensures",
        ),
        (
            "actor field initializer",
            r#"
actor Initialized {
    value: i32 = { requires: true; 1; }
}
func main() -> i32 { 0 }
"#,
            "requires",
        ),
        (
            "constant initializer",
            r#"
const INITIAL: i32 = { ensures: true; 1; }
func main() -> i32 { 0 }
"#,
            "ensures",
        ),
    ];

    for (name, source, contract) in cases {
        let file =
            parse_memory_source(source, name).unwrap_or_else(|error| panic!("{name}: {error}"));
        let diagnostics = match crate::core::check_program(&file) {
            Err(diagnostics) => diagnostics,
            Ok(_) => panic!("{name}: nested contract was accepted"),
        };
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0503)
                    && diagnostic.message.contains(contract)
            }),
            "{name}: {diagnostics:?}"
        );
    }
}

#[test]
fn top_level_requires_and_ensures_remain_valid_for_single_and_dual_verification() {
    require_z3!();
    let source = r#"
func guarded(value: i32) -> i32 {
    requires: value > 0
    ensures: result == value
    value
}
func main() -> i32 { guarded(1) }
"#;
    let file = parse_memory_source(source, "top-level-contract-fields").expect("parse");
    let checked = crate::core::check_program(&file).expect("top-level contracts are valid");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    for (engine, results) in [
        (
            "single",
            verify_checked(&checked, source_hash.clone()).expect("single verify"),
        ),
        (
            "dual",
            verify_checked_dual(&checked, source_hash.clone()).expect("dual verify"),
        ),
    ] {
        let guarded = results
            .iter()
            .find(|result| result.func_name == "guarded")
            .unwrap_or_else(|| panic!("{engine} verifier omitted guarded: {results:?}"));
        assert_eq!(
            guarded.status,
            VerifStatus::Proven,
            "{engine}: {}",
            guarded.message
        );
        assert!(
            guarded.artifact.is_some(),
            "{engine} must return proof evidence"
        );
    }
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

    let verify_result = verify_checked(&program, source_hash.clone()).expect("MIR verify");
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

    let dual_result = verify_checked_dual(&program, source_hash.clone()).expect("MIR dual verify");
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

    let results =
        verify_checked(&program, source_hash.clone()).expect("nested Option tuple MIR verify");
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

    let dual = verify_checked_dual(&program, source_hash).expect("nested Option tuple dual verify");
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

    let results = verify_checked(&program, source_hash.clone()).expect("public MIR verify");
    let result = exact_mir_result_for_test(&results, "main");
    assert_eq!(result.status, VerifStatus::Proven);
    assert!(result.constraint_count > 0);
    assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);

    let dual = verify_checked_dual(&program, source_hash.clone()).expect("public dual MIR verify");
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
        if dual {
            // This pins the existing no-FFI preguard; the route-materialization
            // assertion below proves dual falls through to canonical MIR.
            assert!(
                super::verify_dual_isolated_ffi_mir_route(&checked, &source_hash).is_none(),
                "an FFI-free canonical Flow request must stay with the ordinary dual route"
            );
        }
        crate::core::mir::reset_test_route_materialization_count();
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

    let results = verify_checked(&program, source_hash.clone()).expect("public MIR counterexample");
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

    let results =
        verify_checked(&program, source_hash.clone()).expect("public multifield MIR verify");
    let result = exact_mir_result_for_test(&results, "main");
    assert_eq!(result.status, VerifStatus::Proven);
    assert!(result.constraint_count > 0);
    assert_single_exact_mir_artifact(result, &source_hash, &typed_receipt);

    let dual_results = crate::verifier::verify_checked_dual(&program, source_hash.clone())
        .expect("public dual multifield MIR verify");
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

    let results = verify_checked(&program, source_hash).expect("public f64 Flow MIR verify");
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
fn public_checked_verifier_keeps_complete_owner_v2_diagnostic_until_readiness() {
    require_z3!();
    const {
        assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };

    let source = r#"
type Label { text: string }
func v2_owner() -> i32 {
    ensures: result == 0
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    drop(empty)
    drop(root)
    drop(label)
    0
}
func unrelated(value: i32) -> i32 {
    requires: value >= 0
    ensures: result == value
    value
}
func main() -> i32 { 0 }
"#;
    let file = parse_memory_source(source, "owner-v2-public-single-verifier").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owned_any_v2,
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage
    );

    let results = super::verify_checked(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("single checked verifier returns its diagnostic batch");
    let owner = results
        .iter()
        .find(|result| result.func_name == "v2_owner")
        .expect("V2 proof owner remains visible");
    assert_eq!(owner.status, VerifStatus::NotInTrustedSubset);
    assert_eq!(owner.constraint_count, 0);
    assert!(owner.artifact.is_none());
    assert!(owner.message.contains("MIR-COVERAGE-001"), "{owner:#?}");
    assert_eq!(
        owner
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    );
    let unrelated = results
        .iter()
        .find(|result| result.func_name == "unrelated")
        .expect("unrelated proof owner remains visible");
    assert_eq!(unrelated.status, VerifStatus::Proven, "{unrelated:#?}");
    assert!(unrelated.artifact.is_some());
    assert!(!super::canonical_execution_route_verifier_ready(
        &results, false, false
    ));
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
        "func multiply() -> f64 { 1.0 * 2.0 }\nfunc main() -> i32 { 0 }",
        "mir-f64-multiply-capability",
    )
    .expect("parse multiply");
    let checked = crate::core::check_program(&multiply).expect("typecheck multiply");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical f64 multiply MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("finite-only f64 Multiply capability");

    let divide = parse_memory_source(
        "func divide() -> f64 { 1.0 / 2.0 }\nfunc main() -> i32 { 0 }",
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
    let error = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect_err("nested managed bool Result fallback must fail before legacy verifier");
    assert!(
        error.contains("generic MIR instance") || error.contains("generic Result"),
        "{error}"
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
    let error = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect_err("unsupported generic Result unwrap_or must not fall through to AST verifier");
    assert!(
        error.contains("generic Result fallback projection"),
        "{error}"
    );
}

#[test]
fn generic_result_projection_is_rejected_before_legacy_verifier_fallback() {
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_rejected.mimi");
    let file = parse_memory_source(source, "mir-generic-result-rejected").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let single_error = crate::verifier::verify_checked(&checked, source_hash.clone())
        .expect_err("single verifier must reject unsupported generic Result projection");
    assert!(
        single_error.contains("generic-result-projection-v1")
            || single_error.contains("generic Result projection"),
        "{single_error}"
    );

    let error = crate::verifier::verify_checked_dual(&checked, source_hash)
        .expect_err("unsupported generic Result projection must not fall through to AST verifier");
    assert!(
        error.contains("generic-result-projection-v1")
            || error.contains("generic Result projection"),
        "{error}"
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
    let error = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect_err("generic Option unwrap_or must not fall through to AST verifier");
    assert!(error.contains("generic Option projection"), "{error}");
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
    crate::core::mir::reset_test_route_materialization_count();
    assert!(
        crate::verifier::verify_checked(&checked, source_hash.clone())
            .expect("public verifier")
            .is_empty()
    );
    assert_eq!(crate::core::mir::test_route_materialization_count(), 1);

    crate::core::mir::reset_test_route_materialization_count();
    assert!(crate::verifier::verify_checked_dual(&checked, source_hash)
        .expect("dual public verifier")
        .is_empty());
    assert_eq!(crate::core::mir::test_route_materialization_count(), 1);

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
    receipt
        .validate_against_program(&route.program)
        .expect("MapAnyOwner v1 receipt binds the exact route graph");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let exact_mir_results = crate::verifier::verify_mir_with_route_receipt(
        &route.program,
        &receipt,
        source_hash.clone(),
    )
    .expect("exact MapAnyOwner v1 route verifier");

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
        assert_route_mir_artifact(exact_mir, &source_hash, &receipt);
        let public = exact_mir_result_for_test(&results, function);
        assert_eq!(
            public.status, status,
            "public {function}: {}",
            public.message
        );
        assert_route_mir_artifact(public, &source_hash, &receipt);
    }

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
            &receipt,
        );
    }
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
    let exact_mir_results = map_results;
    for (function, status) in [
        ("checked_owner_size", VerifStatus::Proven),
        ("false_owner_size", VerifStatus::Disproven),
    ] {
        let map_result = exact_mir_result_for_test(&exact_mir_results, function);
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
            "exact MapAnyOwner route {function}: {}",
            exact_mir.message
        );
        assert_route_mir_artifact(exact_mir, &source_hash, &map_receipt);
    }

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
        assert_route_mir_artifact(result, &source_hash, &map_receipt);
    }
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
    let verify_result = verify_checked(&program, source_hash.clone()).expect("MIR verify");
    assert!(verify_result.is_empty());
    assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
    assert!(mir_route.unwrap().is_empty());

    crate::core::mir::reset_test_route_materialization_count();
    let dual_result = verify_checked_dual(&program, source_hash).expect("MIR dual verify");
    assert!(dual_result.is_empty());
    assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
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
func main() -> i32 { 0 }
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
func main() -> i32 { 0 }
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
fn mir_proof_cache_key_binds_proof_profile_scope_and_route_identity() {
    let mut mir = ProofArtifact::new("z3 4.13.0".to_string(), "src-hash".to_string());
    mir.engine = ProofArtifact::ENGINE_MIR.to_string();
    mir.proof_profile = ProofArtifact::PROFILE_MIR_V1.to_string();
    mir.obligation_scope = ProofArtifact::SCOPE_MIR_FUNCTION_CONTRACTS_V1.to_string();
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

    let mut different_proof_profile = mir.clone();
    different_proof_profile.proof_profile =
        ProofArtifact::PROFILE_MIR_FFI_EXPERIMENTAL_V1.to_string();
    assert_ne!(
        mir.cache_key(),
        different_proof_profile.cache_key(),
        "MIR FFI semantics must not reuse ordinary MIR proof cache entries"
    );

    let mut different_scope = mir.clone();
    different_scope.obligation_scope = ProofArtifact::SCOPE_FFI_REQUIRES_ONLY_V1.to_string();
    assert_ne!(
        mir.cache_key(),
        different_scope.cache_key(),
        "obligation scope must participate in the proof cache identity"
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
            proof_profile: ProofArtifact::PROFILE_MIR_V1.into(),
            obligation_scope: ProofArtifact::SCOPE_MIR_FUNCTION_CONTRACTS_V1.into(),
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

    let mut ffi_profile = results.clone();
    let ffi_artifact = ffi_profile[0]
        .artifact
        .as_mut()
        .expect("first MIR artifact");
    ffi_artifact.proof_profile = ProofArtifact::PROFILE_MIR_FFI_EXPERIMENTAL_V1.into();
    ffi_artifact.obligation_scope = ProofArtifact::SCOPE_FFI_REQUIRES_ENSURES_V1.into();
    ffi_artifact.float_model = crate::core::mir::types::MIR_FFI_VERIFIER_FLOAT_MODEL.into();
    assert!(
        validate_mir_result_provenance(&ffi_profile, &receipt, source_hash, "test-batch").is_ok()
    );

    let mut vcore_profile = results.clone();
    let vcore_artifact = vcore_profile[0]
        .artifact
        .as_mut()
        .expect("first MIR artifact");
    vcore_artifact.proof_profile = ProofArtifact::PROFILE_VERIFIED_CORE_1.into();
    vcore_artifact.float_model = "f64_rejected".into();
    let error = validate_mir_result_provenance(&vcore_profile, &receipt, source_hash, "test-batch")
        .expect_err("a MIR FFI proof cannot be relabeled as Verified Core 1");
    assert!(error.contains("unsupported proof identity"), "{error}");

    let mut old_semantics = results.clone();
    old_semantics[0]
        .artifact
        .as_mut()
        .expect("first MIR artifact")
        .semantics_version = 2;
    let error = validate_mir_result_provenance(&old_semantics, &receipt, source_hash, "test-batch")
        .expect_err("an older artifact schema/semantics version is not accepted");
    assert!(error.contains("unsupported semantics_version"), "{error}");

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
fn canonical_mir_secondary_divergence_fails_closed_with_valid_provenance() {
    require_z3!();

    // Keep this as a merge-policy regression: both engine observations and
    // their proof identities come from their independent consumers, then the
    // Resolved verdict is changed at the seam to force the otherwise rare
    // disagreement branch deterministically. The MIR observation and its
    // canonical route receipt remain untouched.
    //
    // The secondary verifier here is canonical MIR; the former Flow AST and
    // secondary VIR compatibility owners have been physically retired.
    let source = r#"
func identity(value: i32) -> i32 {
    ensures: result == value
    value
}
"#;
    let file = parse_memory_source(source, "canonical-mir-merge-divergence").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let (mir_results, receipt) = verify_exact_typed_mir_request_for_test(&checked, &source_hash);
    let mut mir_secondary = exact_mir_result_for_test(&mir_results, "identity").clone();
    assert_eq!(mir_secondary.status, VerifStatus::Proven);
    let mir_artifact = mir_secondary
        .artifact
        .as_ref()
        .expect("independent MIR secondary artifact");
    assert_eq!(mir_artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(mir_artifact.source_hash, source_hash);
    assert_eq!(mir_artifact.mir_hash, receipt.mir_digest);
    assert_eq!(mir_artifact.mir_route_receipt.as_ref(), Some(&receipt));
    validate_mir_result_provenance(
        std::slice::from_ref(&mir_secondary),
        &receipt,
        &source_hash,
        "canonical_mir_merge_test",
    )
    .expect("secondary must carry valid canonical MIR provenance before merging");

    // Keep a negative lock for the provenance checker while exercising the
    // independent engine merge below.
    let mut forged_mir_secondary = mir_secondary.clone();
    forged_mir_secondary
        .artifact
        .as_mut()
        .expect("MIR artifact")
        .mir_route_receipt
        .as_mut()
        .expect("MIR route receipt")
        .abi_digest = "0".repeat(64);
    let provenance_error = validate_mir_result_provenance(
        &[forged_mir_secondary],
        &receipt,
        &source_hash,
        "canonical_mir_merge_test",
    )
    .expect_err("a forged route receipt must be rejected before proof use");
    assert!(
        provenance_error.contains("route receipt"),
        "{provenance_error}"
    );

    let mut resolved_engine = Verifier::new().expect("Resolved solver");
    resolved_engine.set_source_hash(source_hash);
    let resolved_results = resolved_engine.verify_checked(&checked);
    let mut resolved_primary = resolved_results
        .into_iter()
        .find(|result| result.func_name == "identity" || result.func_name == "function:identity")
        .expect("independent Resolved primary result");
    assert_eq!(resolved_primary.status, VerifStatus::Proven);
    let resolved_artifact = resolved_primary
        .artifact
        .as_ref()
        .expect("Resolved primary artifact");
    assert_eq!(resolved_artifact.engine, ProofArtifact::ENGINE_RESOLVED);
    assert!(!resolved_artifact.resolved_ir_hash.is_empty());

    // Normalize the identity just as the production dual route does before
    // invoking its shared merge policy. Keep both real engine artifacts, but
    // inject a deterministic primary verdict mismatch at this test seam.
    resolved_primary.func_name = mir_secondary.func_name.clone();
    resolved_primary.status = VerifStatus::Disproven;
    resolved_primary.message = "test-injected Resolved disagreement".to_string();
    mir_secondary.message = "canonical MIR proof remains Proven".to_string();

    let merged = merge_engine_verdicts_with_secondary_label(
        vec![resolved_primary],
        vec![mir_secondary],
        "canonical_mir",
    );
    assert_eq!(merged.len(), 1);
    assert_eq!(
        merged[0].status,
        VerifStatus::Disproven,
        "the weaker Disproven conclusion must win over canonical MIR Proven"
    );
    assert!(merged[0].message.contains("E0439"), "{}", merged[0].message);
    assert!(
        merged[0].message.contains("canonical_mir"),
        "the divergence must identify the independent MIR secondary: {}",
        merged[0].message
    );
    let diagnostic = merged[0]
        .diagnostic
        .as_ref()
        .expect("divergence diagnostic");
    assert!(diagnostic.message.contains("E0439"), "{diagnostic:?}");
    assert!(
        diagnostic.message.contains("canonical_mir"),
        "{diagnostic:?}"
    );
    assert!(
        merged[0].artifact.is_none(),
        "a divergent result must not retain either engine's proof artifact"
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
    let source_preserving_owner = crate::core::NodeId("function:checked_source_preserving".into());
    let source_preserving_mir = program
        .functions()
        .get(&source_preserving_owner)
        .expect("checked_source_preserving MIR function");
    let entry = source_preserving_mir
        .blocks
        .get(&source_preserving_mir.entry)
        .expect("checked_source_preserving entry block");
    let first_map_instruction = entry
        .instructions
        .iter()
        .find(|instruction| {
            matches!(
                &instruction.kind,
                crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleNew { .. }
                    | crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleSet { .. }
                    | crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { .. }
                    | crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleSize { .. }
                    | crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleContains { .. }
                    | crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. }
            )
        })
        .expect("checked_source_preserving has an ordinary Map lifecycle operation");
    assert!(matches!(
        &first_map_instruction.kind,
        crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleNew { .. }
    ));
    let new_capacity_result = results
        .iter()
        .find(|result| result.func_name == source_preserving_owner.0)
        .expect("checked_source_preserving verification result");
    assert_eq!(new_capacity_result.status, VerifStatus::Disproven);
    assert!(new_capacity_result
        .message
        .contains("canonical MIR body can reach trap 'E0800' under requires"));
    let new_capacity_diagnostic = new_capacity_result
        .diagnostic
        .as_ref()
        .expect("New capacity edge has a structured diagnostic");
    assert!(new_capacity_diagnostic
        .message
        .contains("canonical MIR body can reach trap 'E0800' under requires"));
    let new_capacity_origin = first_map_instruction
        .id
        .as_str()
        .splitn(3, ':')
        .nth(2)
        .expect("the New instruction id retains its exact Checker source call");
    assert_eq!(
        new_capacity_diagnostic
            .origin
            .as_ref()
            .and_then(|origin| origin.parent_node_id.as_deref()),
        Some(new_capacity_origin),
        "the New capacity failure remains attached to the exact Checker New call"
    );

    for (function, status) in [
        ("function:checked_source_preserving", VerifStatus::Disproven),
        ("function:checked_remove_preserving", VerifStatus::Disproven),
        ("function:checked_dynamic_contains", VerifStatus::Disproven),
        // Keep the dynamic-key false claim in the matrix. Its outcome, like
        // the other Map contracts, is already disproven by the first reachable
        // capacity trap, before the key-collision counterexample is examined.
        (
            "function:false_distinct_dynamic_keys",
            VerifStatus::Disproven,
        ),
        ("function:checked_equal_string_keys", VerifStatus::Disproven),
        ("function:checked_string_payload", VerifStatus::Disproven),
        // Retain both NUL-key claims; the reachable New capacity failure
        // prevents either result from being reported as Proven.
        ("function:checked_nul_string_key", VerifStatus::Disproven),
        ("function:false_nul_string_key", VerifStatus::Disproven),
    ] {
        let result = results
            .iter()
            .find(|result| result.func_name == function)
            .unwrap_or_else(|| panic!("missing ordinary Map verifier result for {function}"));
        assert_eq!(result.status, status, "{}", result.message);
        // MIR artifacts bind the semantic context for definitive disproofs as
        // well; the status must never report these trap-bearing contracts as
        // Proven.
        assert_ne!(result.status, VerifStatus::Proven);
        assert!(
            result
                .message
                .contains("canonical MIR body can reach trap 'E0800' under requires"),
            "{function}: Map capacity trap must remain visible: {}",
            result.message
        );
        assert!(
            result.diagnostic.as_ref().is_some_and(|diagnostic| {
                diagnostic
                    .message
                    .contains("canonical MIR body can reach trap 'E0800' under requires")
            }),
            "{function}: reachable E0800 must have a diagnostic"
        );
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

fn ordinary_map_observer_v1_program_for_test(
    source: &str,
    source_name: &str,
) -> crate::core::mir::reference::MirProgram {
    let file =
        parse_memory_source(source, source_name).expect("parse ordinary Map observer source");
    let checked =
        crate::core::check_program(&file).expect("typecheck ordinary Map observer source");
    crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical ordinary Map observer MIR")
}

fn assert_observer_v1_nits_without_artifact(
    results: &[VerificationResult],
    message_fragment: &str,
) {
    let result = results
        .iter()
        .find(|result| matches!(result.func_name.as_str(), "main" | "function:main"))
        .unwrap_or_else(|| {
            panic!("ordinary Map observer must have an owner-scoped result: {results:#?}")
        });
    assert_eq!(result.status, VerifStatus::NotInTrustedSubset, "{result:?}");
    assert_eq!(result.constraint_count, 0, "{result:?}");
    assert!(result.artifact.is_none(), "{result:?}");
    assert!(result.message.contains(message_fragment), "{result:?}");
}

fn assert_observer_v1_e0800_with_exact_artifact(
    results: &[VerificationResult],
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: &str,
) {
    let result = results
        .iter()
        .find(|result| matches!(result.func_name.as_str(), "main" | "function:main"))
        .unwrap_or_else(|| {
            panic!("ordinary Map observer must have an owner-scoped result: {results:#?}")
        });
    assert_eq!(result.status, VerifStatus::Disproven, "{result:?}");
    assert!(
        result.message.contains("can reach trap 'E0800'"),
        "{result:?}"
    );
    assert!(result.constraint_count > 0, "{result:?}");
    let artifact = result
        .artifact
        .as_ref()
        .expect("a definitive trap disproof retains its replayable verifier artifact");
    assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.source_hash, source_hash);
    assert_eq!(artifact.mir_hash, receipt.mir_digest);
    assert_eq!(artifact.mir_route_receipt.as_ref(), Some(receipt));
}

#[test]
fn ordinary_map_owner_observer_v1_reports_reachable_capacity_after_exact_json_projection() {
    require_z3!();
    let source = r#"
func main() -> i32 {
    let m0 = map_new()
    let m1 = map_set(m0, "雪", "payload")
    let m2 = map_set(m1, "a\0", "雪\0\n\t\u{1}")
    let m3 = map_set(m2, "a", 17)
    let snapshot = to_json(m3)
    println(m3)
    println(snapshot)
    let size = map_size(m3)
    drop(m0)
    drop(m1)
    drop(m2)
    drop(m3)
    drop(snapshot)
    size
}
"#;
    let program =
        ordinary_map_observer_v1_program_for_test(source, "ordinary-map-observer-v1-json");
    let owner_type = crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
    let observer_receipts = program
        .ordinary_map_owner_lifecycle_observer_v1_receipts()
        .expect("independent Checker/MIR observer projection");
    assert_eq!(observer_receipts.len(), 2);
    assert!(observer_receipts
        .iter()
        .any(|receipt| receipt.builtin() == "to_json"));
    assert!(observer_receipts
        .iter()
        .any(|receipt| receipt.builtin() == "println"));
    let route_receipt = program
        .ordinary_map_owner_lifecycle_observer_v1_route_receipt()
        .expect("exact observer-v1 route receipt");
    assert_eq!(
        route_receipt.profile,
        crate::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE
    );
    crate::verifier::validate_mir_capabilities(&program)
        .expect("observer profile has a dedicated receipt-backed verifier capability");

    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let results = crate::verifier::verify_mir_with_route_receipt(
        &program,
        &route_receipt,
        source_hash.clone(),
    )
    .expect("exact observer-v1 MIR route verification");
    assert_observer_v1_e0800_with_exact_artifact(&results, &route_receipt, &source_hash);

    let convenience_results = crate::verifier::verify_mir(&program, source_hash.clone())
        .expect("the MIR convenience selector chooses the exact observer profile");
    assert_observer_v1_e0800_with_exact_artifact(
        &convenience_results,
        &route_receipt,
        &source_hash,
    );
    let file = parse_memory_source(source, "ordinary-map-observer-v1-checked-dispatch")
        .expect("parse observer route dispatch source");
    let checked = crate::core::check_program(&file).expect("check observer route dispatch source");
    let checked_mir = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("materialize checked-dispatch observer MIR");
    let checked_route_receipt = checked_mir
        .ordinary_map_owner_lifecycle_observer_v1_route_receipt()
        .expect("checked-dispatch observer MIR has its own exact route receipt");
    let checked_results = crate::verifier::verify_checked(&checked, source_hash.clone())
        .expect("checked verifier dispatches the exact observer profile");
    assert_observer_v1_e0800_with_exact_artifact(
        &checked_results,
        &checked_route_receipt,
        &source_hash,
    );
    let dual_results = crate::verifier::verify_checked_dual(&checked, source_hash.clone())
        .expect("dual verifier dispatches the exact observer profile");
    assert_observer_v1_e0800_with_exact_artifact(
        &dual_results,
        &checked_route_receipt,
        &source_hash,
    );

    let main = program
        .functions()
        .values()
        .find(|function| function.owner.0 == "function:main")
        .expect("main MIR function");
    let instructions = main
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .collect::<Vec<_>>();
    let (json_position, json_root, json_result) = instructions
        .iter()
        .enumerate()
        .find_map(|(index, instruction)| match &instruction.kind {
            crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson {
                result,
                root,
                ..
            } => Some((index, root, result)),
            _ => None,
        })
        .expect("to_json observer MIR instruction");
    let (println_position, println_root, println_result) = instructions
        .iter()
        .enumerate()
        .find_map(|(index, instruction)| match &instruction.kind {
            crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln {
                result,
                root,
                ..
            } => Some((index, root, result)),
            _ => None,
        })
        .expect("borrowed println observer MIR instruction");
    let (size_position, size_root) = instructions
        .iter()
        .enumerate()
        .find_map(|(index, instruction)| match &instruction.kind {
            crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleSize {
                root, ..
            } => Some((index, root)),
            _ => None,
        })
        .expect("later map_size instruction");
    let drop_position = instructions
        .iter()
        .enumerate()
        .find_map(|(index, instruction)| match &instruction.kind {
            crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { root }
                if root == println_root =>
            {
                Some(index)
            }
            _ => None,
        })
        .expect("drop of the exact observed Map root");
    assert!(json_position < println_position);
    assert!(println_position < size_position && size_position < drop_position);
    assert_eq!(json_root, println_root, "to_json borrows the live root");
    assert_eq!(
        size_root, println_root,
        "size must observe the same borrowed root"
    );
    assert_eq!(
        main.values.get(println_root).map(|value| &value.ty),
        Some(&owner_type),
        "println borrows the owner TypeDesc"
    );
    let json_result_type = &main
        .values
        .get(json_result)
        .expect("owned to_json result")
        .ty;
    assert!(program
        .type_catalog()
        .validate_owned_string(json_result_type)
        .is_ok());
    assert!(main
        .values
        .get(println_result)
        .and_then(|value| program.type_catalog().get(&value.ty))
        .is_some_and(|descriptor| descriptor.is_canonical_ffi_unit()));
}

#[test]
fn ordinary_map_owner_observer_v1_rejects_unknown_keys_and_values_without_proof() {
    require_z3!();
    for (source_name, source, expected) in [
        (
            "ordinary-map-observer-v1-dynamic-key",
            r#"
func read_dynamic_key() -> string { input() }
func main() -> i32 {
    let dynamic_key = read_dynamic_key()
    let root = map_new()
    let inserted = map_set(root, dynamic_key, 1)
    let snapshot = to_json(inserted)
    drop(root)
    drop(inserted)
    drop(snapshot)
    0
}
"#,
            "exact literal keys",
        ),
        (
            "ordinary-map-observer-v1-unknown-value",
            r#"
func read_unknown_payload() -> i32 {
    if input() == "zero" { 0 } else { 1 }
}
func main() -> i32 {
    let payload = read_unknown_payload()
    let root = map_new()
    let inserted = map_set(root, "known", payload)
    let snapshot = to_json(inserted)
    drop(root)
    drop(inserted)
    drop(snapshot)
    0
}
"#,
            "unknown payload value",
        ),
    ] {
        let program = ordinary_map_observer_v1_program_for_test(source, source_name);
        let receipt = program
            .ordinary_map_owner_lifecycle_observer_v1_route_receipt()
            .expect("exact observer route receipt");
        crate::verifier::validate_mir_capabilities(&program)
            .expect("the source shape has exact observer/lifecycle Checker receipts");
        let results = crate::verifier::verify_mir_with_route_receipt(
            &program,
            &receipt,
            blake3::hash(source.as_bytes()).to_hex().to_string(),
        )
        .expect("unsupported observer payload is an owner-scoped NITS result");
        assert_observer_v1_nits_without_artifact(&results, expected);
    }
}

#[test]
fn ordinary_map_owner_observer_v1_rejects_mismatched_or_tampered_route_receipts() {
    require_z3!();
    let source = r#"
func main() -> i32 {
    let wrong_type = true
    let root = map_new()
    let inserted = map_set(root, "one", 1)
    let snapshot = to_json(inserted)
    drop(root)
    drop(inserted)
    drop(snapshot)
    if wrong_type { 0 } else { 1 }
}
"#;
    let program =
        ordinary_map_observer_v1_program_for_test(source, "ordinary-map-observer-v1-receipt");
    let receipt = program
        .ordinary_map_owner_lifecycle_observer_v1_route_receipt()
        .expect("exact observer route receipt");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let mut wrong_profile = receipt.clone();
    wrong_profile.profile = "verifier-mir-v1".into();
    let results = crate::verifier::mir::verify_program_with_route_receipt(
        &program,
        source_hash.clone(),
        &wrong_profile,
    )
    .expect("wrong profile becomes an owner-scoped coverage rejection");
    assert_observer_v1_nits_without_artifact(&results, "exact");

    let mut forged_digest = receipt;
    forged_digest.mir_digest = "0".repeat(64);
    let results = crate::verifier::mir::verify_program_with_route_receipt(
        &program,
        source_hash,
        &forged_digest,
    )
    .expect("stale receipt becomes an owner-scoped coverage rejection");
    assert_observer_v1_nits_without_artifact(&results, "route receipt rejected");
}

#[test]
fn ordinary_map_owner_observer_v1_rejects_tampered_checker_mir_projection() {
    require_z3!();
    let source = r#"
func main() -> i32 {
    let wrong_type = true
    let root = map_new()
    let inserted = map_set(root, "one", 1)
    let snapshot = to_json(inserted)
    drop(root)
    drop(inserted)
    drop(snapshot)
    if wrong_type { 0 } else { 1 }
}
"#;
    let program =
        ordinary_map_observer_v1_program_for_test(source, "ordinary-map-observer-v1-forged");
    let mut tampered = program.clone();
    let mut function = tampered
        .functions()
        .values()
        .find(|function| function.owner.0 == "function:main")
        .expect("main observer owner")
        .clone();
    let observer = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                &instruction.kind,
                crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson { .. }
            )
        })
        .expect("to_json observer instruction");
    let crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson {
        callsite,
        ..
    } = &mut observer.kind
    else {
        unreachable!("selected observer instruction")
    };
    *callsite = crate::core::NodeId(format!("{}:forged", callsite.0));
    tampered.replace_function_for_test_only(function);
    let receipt =
        tampered.route_receipt(crate::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE);
    let results = crate::verifier::mir::verify_program_with_route_receipt(
        &tampered,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
        &receipt,
    )
    .expect("tampered Checker/MIR projection becomes a coverage rejection");
    assert_observer_v1_nits_without_artifact(&results, "observer");

    let mut missing_lifecycle_receipt = program.clone();
    let mut function = missing_lifecycle_receipt
        .functions()
        .values()
        .find(|function| function.owner.0 == "function:main")
        .expect("main observer owner")
        .clone();
    function
        .ownership
        .clear_checker_ordinary_map_owner_lifecycle_v1_for_test_only();
    missing_lifecycle_receipt.replace_function_for_test_only(function);
    let receipt = missing_lifecycle_receipt
        .route_receipt(crate::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE);
    let results = crate::verifier::mir::verify_program_with_route_receipt(
        &missing_lifecycle_receipt,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
        &receipt,
    )
    .expect("missing Checker lifecycle receipt becomes a coverage rejection");
    assert_observer_v1_nits_without_artifact(&results, "observer");

    let mut wrong_type_mir = program.clone();
    let mut function = wrong_type_mir
        .functions()
        .values()
        .find(|function| function.owner.0 == "function:main")
        .expect("main observer owner")
        .clone();
    let bool_type = wrong_type_mir
        .type_catalog()
        .iter()
        .find_map(|(ty, descriptor)| {
            (descriptor.kind
                == crate::core::mir::types::MirTypeKind::Primitive(
                    crate::core::PrimitiveType::Bool,
                ))
            .then(|| ty.clone())
        })
        .expect("canonical Bool TypeDesc");
    let bool_value = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Const { result, .. }
                if function.values.get(result).map(|value| &value.ty) == Some(&bool_type) =>
            {
                Some(result.clone())
            }
            _ => None,
        })
        .expect("Bool source value used after the Map lifecycle");
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
    wrong_type_mir.replace_function_for_test_only(function);
    let receipt = wrong_type_mir
        .route_receipt(crate::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE);
    let results = crate::verifier::mir::verify_program_with_route_receipt(
        &wrong_type_mir,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
        &receipt,
    )
    .expect("wrong payload TypeDesc becomes a coverage rejection");
    assert_observer_v1_nits_without_artifact(&results, "observer");
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
        func protected_alias(s: string) -> i64 {
            requires: s != ""
            let alias = s
            nonempty(alias)
        }
        func protected_reversed(s: string) -> i64 {
            requires: "" != s
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
    let results =
        verify_ffi_checked_with_source_hash(&checked, hash.clone()).expect("typed FFI proofs");
    assert_eq!(
        results.len(),
        4,
        "each actual call has one obligation: {results:?}"
    );
    for (caller, status) in [
        ("protected calls nonempty", VerifStatus::Proven),
        ("protected_alias calls nonempty", VerifStatus::Proven),
        ("protected_reversed calls nonempty", VerifStatus::Proven),
        ("violated calls nonempty", VerifStatus::Disproven),
    ] {
        let result = results
            .iter()
            .find(|result| result.func_name == caller)
            .expect("call result");
        assert_eq!(result.status, status, "{caller}: {result:?}");
        let artifact = result.artifact.as_ref().expect("typed proof identity");
        assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.source_hash, hash);
        assert!(artifact.resolved_ir_hash.is_empty());
        assert!(artifact.mir_route_receipt.is_some());
    }
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
fn typed_ffi_direct_string_length_zero_test_profile_matches_public_and_mir_routes() {
    require_z3!();
    let source = r#"
        extern "C" {
            func require_nonempty(s: string) -> i64 requires: len(s) > 0;
        }
        func caller_nonempty(s: string) -> i64 {
            requires: len(s) > 0
            require_nonempty(s)
        }
        func caller_empty(s: string) -> i64 {
            requires: len(s) == 0
            require_nonempty(s)
        }
        func caller_long(s: string) -> i64 {
            requires: len(s) > 1
            require_nonempty(s)
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-string-length-guard").expect("parse");
    let checked = crate::core::check_program(&file).expect("check direct String length guards");
    let (declaration_id, declaration) = checked
        .ffi_declarations()
        .iter()
        .next()
        .expect("one checker-owned extern declaration");
    let signature = declaration
        .signature
        .as_ref()
        .expect("canonical String FFI signature");
    assert_eq!(signature.parameters.len(), 1);
    assert_eq!(
        signature.parameters[0].ty,
        crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String)
    );

    let call_sites = checked
        .call_sites_sorted()
        .into_iter()
        .filter(|site| site.kind == crate::core::ResolvedCallKind::Extern)
        .collect::<Vec<_>>();
    assert_eq!(call_sites.len(), 3, "all call obligations remain present");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let public_results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
        .expect("public checked FFI route");
    assert_eq!(public_results.len(), 3, "{public_results:?}");

    for site in &call_sites {
        if site.owner == "function:caller_long" {
            let boundary = public_results
                .iter()
                .find(|result| result.func_name.contains("caller_long"))
                .expect("out-of-profile String length fact remains visible");
            assert_eq!(boundary.status, VerifStatus::NotInTrustedSubset);
            assert!(boundary.artifact.is_none());
            assert_eq!(
                boundary.diagnostic.as_ref().unwrap().span,
                site.origin.user_span()
            );
            assert!(
                crate::core::mir::reference::MirProgram::from_checked_program_ffi_caller(
                    &checked,
                    &crate::core::NodeId(site.owner.clone()),
                )
                .is_err(),
                "the verifier-only MIR slice stays closed outside its exact zero-test profile"
            );
            continue;
        }
        let target = match site.resolved_callee.as_ref() {
            Some(crate::core::ir::ResolvedCallee::Extern(target)) => target,
            other => panic!("call site is not bound to its checked extern: {other:?}"),
        };
        assert_eq!(
            target, declaration_id,
            "call/declaration identity must agree"
        );
        let owner = crate::core::NodeId(site.owner.clone());
        let caller_mir = crate::core::mir::reference::MirProgram::from_checked_program_ffi_caller(
            &checked, &owner,
        )
        .expect("Checker-bound verifier-only caller MIR");
        assert_eq!(caller_mir.ffi_calls().len(), 1);
        let call_receipt = caller_mir
            .ffi_calls()
            .values()
            .next()
            .expect("one exact FFI call receipt");
        assert_eq!(call_receipt.caller, owner);
        assert_eq!(call_receipt.call_node, site.node_id);
        assert_eq!(call_receipt.callee, *target);
        assert_eq!(call_receipt.declaration, declaration.declaration);
        assert_eq!(
            call_receipt.parameter_types,
            vec![signature.parameters[0].ty.clone()]
        );

        let expected = if site.owner == "function:caller_nonempty" {
            VerifStatus::Proven
        } else {
            assert_eq!(site.owner, "function:caller_empty");
            VerifStatus::Disproven
        };
        let public = public_results
            .iter()
            .find(|result| {
                result
                    .func_name
                    .contains(site.owner.trim_start_matches("function:"))
            })
            .expect("public result for exact caller owner");
        assert_eq!(public.status, expected, "{}: {public:?}", site.owner);
        assert_eq!(
            public.artifact.as_ref().unwrap().engine,
            ProofArtifact::ENGINE_MIR
        );
        if expected == VerifStatus::Disproven {
            assert_eq!(
                public.diagnostic.as_ref().unwrap().span,
                site.origin.user_span(),
                "the counterexample belongs to this Checker call site"
            );
        }

        let route_receipt = caller_mir.route_receipt("verifier-ffi-owner-slice-v1");
        let explicit_mir_results = crate::verifier::verify_ffi_mir_requires_with_route_receipt(
            &caller_mir,
            &route_receipt,
            source_hash.clone(),
        )
        .expect("explicit verifier-only MIR route");
        assert_eq!(explicit_mir_results.len(), 1, "{explicit_mir_results:?}");
        assert_eq!(
            explicit_mir_results[0].status, expected,
            "public/MIR agreement for {}: {explicit_mir_results:?}",
            site.owner
        );
        assert_eq!(
            explicit_mir_results[0].artifact.as_ref().unwrap().engine,
            ProofArtifact::ENGINE_MIR
        );
    }
}

#[test]
fn mir_ffi_callsite_combines_requires_and_ensures_into_one_result() {
    require_z3!();
    let source = r#"
        extern "C" {
            func host_i32(value: i32) -> i32
                requires: value == value
                ensures: result == result;
        }
        func caller(value: i32) -> i32 {
            host_i32(value)
        }
    "#;
    let file = parse_memory_source(source, "mir-ffi-callsite-phases").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let callsite = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:caller" && site.callee == "host_i32")
        .expect("one checker-owned FFI callsite");
    let results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
        .expect("canonical MIR FFI phase proofs");
    assert_eq!(
        results.len(),
        1,
        "one result per FFI call site: {results:?}"
    );
    let result = results
        .iter()
        .find(|result| result.func_name == "function:caller")
        .expect("single qualified call-site result");
    assert_eq!(result.status, VerifStatus::Proven, "{result:?}");
    assert!(result.message.contains("extern requires contract proven"));
    assert!(result.message.contains("extern ensures contract proven"));
    assert!(
        result.message.contains(&callsite.node_id.0),
        "the combined proof remains bound to the exact source callsite: {result:?}"
    );
    let artifact = result.artifact.as_ref().expect("MIR proof artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.source_hash, source_hash);
    assert!(artifact.mir_route_receipt.is_some());
}

#[test]
fn mir_ffi_requires_failure_precedes_ensures_in_the_single_callsite_result() {
    require_z3!();
    let source = r#"
        extern "C" {
            func host_i32(value: i32) -> i32
                requires: value > 0
                ensures: result == result;
        }
        func caller(value: i32) -> i32 {
            host_i32(value)
        }
    "#;
    let file = parse_memory_source(source, "mir-ffi-callsite-requires-precedence").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let results = verify_ffi_checked_with_source_hash(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("canonical MIR FFI phase proofs");
    assert_eq!(results.len(), 1, "one result per call site: {results:?}");
    let result = &results[0];
    assert_eq!(result.func_name, "function:caller");
    assert_eq!(result.status, VerifStatus::Disproven, "{result:?}");
    assert!(result
        .message
        .contains("extern requires contract disproven"));
    assert!(result
        .message
        .contains("Ensures was not evaluated because Requires did not discharge the call"));
    assert!(!result.message.contains("extern ensures contract proven"));
    assert!(
        result.artifact.is_some(),
        "counterexample is definitive: {result:?}"
    );
}

#[test]
fn mir_ffi_root_requires_flows_through_one_checked_local_helper() {
    require_z3!();
    let source = r#"
        extern "C" {
            func host_i32(value: i32) -> i32
                requires: value > 0
                ensures: result == result;
        }
        func caller(value: i32) -> i32 {
            requires: value > 0
            func local(value: i32) -> i32 { host_i32(value) }
            local(value)
        }
    "#;
    let file = parse_memory_source(source, "mir-ffi-nested-helper-root-requires").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let callsite = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.kind == crate::core::ResolvedCallKind::Extern)
        .expect("Checker-owned extern callsite in the local helper");
    assert!(
        callsite
            .owner
            .starts_with("function:caller/function:local:"),
        "the extern source owner must remain the nested helper: {callsite:?}"
    );

    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("canonical scalar FFI route with nested helper");
    let canonical = &route.program;
    canonical
        .validate_nested_callable_scope_receipts()
        .expect("exact Checker nested-call receipt");
    canonical
        .validate_ffi_source_census()
        .expect("exact Checker extern source/call join");
    let receipt = canonical.route_receipt("verify-ffi-v1");
    let (instruction_id, ffi_call) = canonical
        .ffi_calls()
        .iter()
        .next()
        .expect("one canonical FFI call receipt");
    assert_eq!(ffi_call.call_node, callsite.node_id);
    assert_eq!(ffi_call.caller.0, callsite.owner);
    let helper = canonical
        .functions()
        .get(&ffi_call.caller)
        .expect("nested helper MIR owner");
    let matching_instructions = helper
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| instruction.id == *instruction_id)
        .collect::<Vec<_>>();
    let [ffi_instruction] = matching_instructions.as_slice() else {
        panic!("the Checker FFI instruction identity must occur exactly once");
    };
    match &ffi_instruction.kind {
        crate::core::mir::MirInstructionKind::Call {
            callee: crate::core::ir::ResolvedCallee::Extern(_),
            result,
            ..
        } => assert_eq!(
            result, &ffi_call.result,
            "Ensures result stays call-result exact"
        ),
        other => panic!("expected the exact canonical extern Call, found {other:?}"),
    }

    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
        .expect("FFI proof starts at the top-level caller and inlines its local helper");
    assert_eq!(
        results.len(),
        1,
        "one result for the exact FFI call: {results:?}"
    );
    let result = &results[0];
    assert_eq!(result.func_name, callsite.owner);
    assert_eq!(result.status, VerifStatus::Proven, "{result:?}");
    assert!(result.message.contains("extern requires contract proven"));
    assert!(result.message.contains("extern ensures contract proven"));
    assert!(result.message.contains(&callsite.node_id.0));
    let artifact = result
        .artifact
        .as_ref()
        .expect("canonical MIR proof artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.source_hash, source_hash);
    assert_eq!(artifact.mir_route_receipt.as_ref(), Some(&receipt));
}

#[test]
fn mir_ffi_nested_helper_without_root_requires_disproves_before_ensures() {
    require_z3!();
    let source = r#"
        extern "C" {
            func host_i32(value: i32) -> i32
                requires: value > 0
                ensures: result == result;
        }
        func caller(value: i32) -> i32 {
            func local(value: i32) -> i32 { host_i32(value) }
            local(value)
        }
    "#;
    let file =
        parse_memory_source(source, "mir-ffi-nested-helper-requires-failure").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let callsite = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.kind == crate::core::ResolvedCallKind::Extern)
        .expect("Checker-owned extern callsite in the local helper");
    let results = verify_ffi_checked_with_source_hash(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("the local helper's call is checked under the unconstrained root input");
    assert_eq!(
        results.len(),
        1,
        "one result for the exact FFI call: {results:?}"
    );
    let result = &results[0];
    assert_eq!(result.func_name, callsite.owner);
    assert_eq!(result.status, VerifStatus::Disproven, "{result:?}");
    assert!(result
        .message
        .contains("extern requires contract disproven"));
    assert!(result.message.contains(&callsite.node_id.0));
    assert!(result
        .message
        .contains("Ensures was not evaluated because Requires did not discharge the call"));
    assert!(!result.message.contains("extern ensures contract proven"));
    assert!(
        result.artifact.is_some(),
        "counterexample is definitive: {result:?}"
    );
}

#[test]
fn mir_ffi_nested_helper_with_multiple_parent_calls_is_rejected_explicitly() {
    require_z3!();
    let source = r#"
        extern "C" {
            func host_i32(value: i32) -> i32
                requires: value > 0
                ensures: result == result;
        }
        func caller(value: i32) -> i32 {
            requires: value > 0
            func local(value: i32) -> i32 { host_i32(value) }
            let first = local(value)
            local(first)
        }
    "#;
    let file =
        parse_memory_source(source, "mir-ffi-nested-helper-multiple-parent-calls").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = verify_ffi_checked_with_source_hash(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect_err("multiple helper call edges remain outside the admitted cross-function slice");
    assert!(
        error.contains("requires exactly one checker-receipted direct parent call"),
        "unsupported shape has an explicit verifier rejection: {error}"
    );
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
                return Open { n: 0 }
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
        "Checker keeps declaration-root calls separate from executable body calls"
    );
    let inactive_default_call = checked
        .checked_ffi_source_roots()
        .iter()
        .find(|root| {
            root.kind()
                == crate::core::ir::lower::ResolvedDeclarationExpressionKind::CallableParameterDefault
        })
        .and_then(|root| root.calls().first())
        .expect("the unused callable default still has a Checker declaration-root call")
        .call_node()
        .clone();
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
    let results = verify_ffi_checked(&checked).expect("typed complete call directory");
    assert_eq!(
        results.len(),
        expected - 1,
        "an unactivated default is not an executed FFI call: {results:?}"
    );
    assert!(
        results
            .iter()
            .all(|result| !result.message.contains(&inactive_default_call.0)),
        "the declaration-only default call must not manufacture a verifier result: {results:?}"
    );
    for site in checked.call_sites_sorted().into_iter().filter(|site| {
        site.kind == crate::core::ResolvedCallKind::Extern
            && site.callee == "nonempty"
            && site.node_id != inactive_default_call
    }) {
        let matching = results
            .iter()
            .filter(|result| result.message.contains(&site.node_id.0))
            .collect::<Vec<_>>();
        assert_eq!(
            matching.len(),
            1,
            "each checker call-site has one exact result: {site:?}, {results:?}"
        );
        if site.owner == "function:ordinary" || site.owner == "transition:ItemFlow::step::Open" {
            assert_eq!(
                matching[0].status,
                VerifStatus::Disproven,
                "site={site:?}; result={:#?}",
                matching[0]
            );
            assert!(matching[0]
                .artifact
                .as_ref()
                .is_some_and(|artifact| artifact.engine == ProofArtifact::ENGINE_MIR));
            assert!(matching[0].message.contains(&site.node_id.0));
        } else {
            assert_eq!(
                matching[0].status,
                VerifStatus::NotInTrustedSubset,
                "Actor/declaration compatibility boundaries must remain explicit until their exact roots are materialized: {site:?}"
            );
            assert!(matching[0].artifact.is_none(), "{site:?}");
        }
    }
}

#[test]
fn ffi_constant_initializer_root_is_consumed_by_mir_census_and_left_fail_closed() {
    let source = r#"
        extern "C" {
            func positive(value: i32) -> i32 requires: value > 0;
        }
        const SEED: i32 = positive(0);
        func main() -> i32 { 0 }
    "#;
    let file = parse_memory_source(source, "ffi-constant-source-root").expect("parse");
    let checked = crate::core::check_program(&file).expect("check constant FFI root");
    let checker_root = checked
        .checked_ffi_source_roots()
        .iter()
        .find(|root| {
            root.kind()
                == crate::core::ir::lower::ResolvedDeclarationExpressionKind::ConstantInitializer
        })
        .expect("Checker-issued constant root");
    let [checker_call] = checker_root.calls() else {
        panic!("one Checker-issued FFI call handle belongs to the constant");
    };
    let checker_context = checked
        .checked_ffi_source_call_context(checker_root, checker_call)
        .expect("Checker root and call handles resolve together");
    let call_site = checked
        .call_sites()
        .get(checker_call.call_node())
        .expect("exact call-site entry");
    assert_eq!(call_site.owner, "constant:SEED");
    assert_eq!(call_site.callee, "positive");
    assert_eq!(call_site.origin, checker_context.call.origin);
    assert_eq!(
        call_site.resolved_callee.as_ref(),
        Some(&crate::core::ir::ResolvedCallee::Extern(
            checker_call.external_declaration().clone(),
        )),
        "the call-site cache must carry the same Checker-issued extern identity"
    );

    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR carries the checker source-root inventory");
    canonical
        .validate_ffi_source_census()
        .expect("MIR verifier census validates exact root/call/declaration binding");
    let matching_mir_roots = canonical
        .proof_source_census()
        .ffi_source_roots()
        .iter()
        .filter(|root| root.root_node() == checker_root.root_node())
        .collect::<Vec<_>>();
    let [mir_root] = matching_mir_roots.as_slice() else {
        panic!("exactly one MIR census entry carries the Checker constant root");
    };
    assert_eq!(
        mir_root.execution(),
        crate::core::mir::MirProofSourceFfiRootExecution::ConstantInitializer
    );
    let [mir_call] = mir_root.calls() else {
        panic!("the MIR census preserves the exact Checker root/call identity");
    };
    assert_eq!(mir_call.call_node(), checker_call.call_node());
    assert_eq!(mir_call.declaration(), checker_call.external_declaration());
    assert_eq!(mir_call.symbol(), "positive");
    assert_eq!(mir_call.call_origin(), &call_site.origin);

    let results = super::verify_ffi_checked_with_source_hash_and_z3_override(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
        Some(true),
    )
    .expect("public verifier consumes the canonical root census");
    assert_eq!(results.len(), 1, "{results:?}");
    let result = &results[0];
    assert_eq!(result.status, VerifStatus::NotInTrustedSubset);
    assert!(result.artifact.is_none());
    assert_eq!(result.constraint_count, 0);
    assert!(result.message.contains(checker_root.root_node().0.as_str()));
    assert!(result.message.contains(checker_call.call_node().0.as_str()));
    assert!(result.message.contains("constant initializer root"));
    assert!(result
        .message
        .contains(&checker_root.origin().unwrap().user_span().to_string()));
    assert!(result.message.contains(&checker_call.call_node().0));
    assert!(result
        .message
        .contains(&checker_context.call.origin.user_span().to_string()));
    assert!(result.message.contains("no executable MIR proof owner"));
    assert_eq!(
        result.diagnostic.as_ref().unwrap().span,
        checker_context.call.origin.user_span()
    );
}

#[test]
fn ffi_root_execution_kinds_keep_actor_and_active_default_boundaries_explicit() {
    let source = r#"
        extern "C" {
            func positive(value: i32) -> i32 requires: value > 0;
        }
        const SEED: i32 = positive(0);
        actor Holder {
            initial: i32 = positive(1);
        }
        trait Defaults {
            func choose(seed: i32, value: i32 = positive(seed)) -> i32;
        }
        func dormant(value: i32 = positive(3)) -> i32 { value }
        func activated(value: i32 = positive(4)) -> i32 { value }
        func caller() -> i32 { activated() }
        func main() -> i32 { caller() }
    "#;
    let file = parse_memory_source(source, "ffi-root-execution-kinds").expect("parse");
    let checked = crate::core::check_program(&file).expect("check declaration roots");
    let positive_declaration = checked
        .ffi_declarations()
        .iter()
        .find(|(_, declaration)| declaration.symbol == "positive")
        .map(|(identity, _)| identity.clone())
        .expect("canonical Checker declaration identity for positive");
    let checked_roots = checked.checked_ffi_source_roots();
    let root_for_owner = |owner: &str| {
        checked_roots
            .iter()
            .find(|root| root.owner().0 == owner)
            .expect("Checker source root")
    };
    let constant = checked_roots
        .iter()
        .find(|root| {
            root.kind()
                == crate::core::ir::lower::ResolvedDeclarationExpressionKind::ConstantInitializer
        })
        .expect("constant initializer root");
    let actor = checked_roots
        .iter()
        .find(|root| {
            root.kind()
                == crate::core::ir::lower::ResolvedDeclarationExpressionKind::ActorFieldInitializer
        })
        .expect("Actor field root");
    let trait_default = checked_roots
        .iter()
        .find(|root| {
            root.kind()
                == crate::core::ir::lower::ResolvedDeclarationExpressionKind::TraitParameterDefault
        })
        .expect("trait default declaration root");
    let trait_default_record = checked
        .declaration_expression_bodies()
        .get(trait_default.root_node())
        .expect("typed trait-default declaration body record");
    assert_eq!(trait_default_record.node_id, *trait_default.root_node());
    assert_eq!(trait_default_record.owner, *trait_default.owner());
    let trait_default_body = trait_default_record
        .body
        .as_ref()
        .unwrap_or_else(|errors| panic!("trait-default body lowering failed: {errors:#?}"));
    let trait_default_calls = trait_default_body
        .call_expressions(checked.resolved_types(), checked.resolved_type_targets())
        .unwrap_or_else(|errors| {
            panic!("trait-default call census validation failed: {errors:#?}")
        });
    assert_eq!(trait_default_body.parameters.len(), 1);
    assert_eq!(trait_default_calls.len(), 1, "{trait_default_calls:#?}");
    let trait_default_call = trait_default_calls
        .values()
        .next()
        .expect("one typed trait-default FFI call");
    let crate::core::ResolvedExprKind::Call(trait_default_call) = &trait_default_call.kind else {
        panic!("expected typed direct call in trait default: {trait_default_call:#?}");
    };
    assert_eq!(trait_default_call.arguments.len(), 1);
    assert!(matches!(
        &trait_default_call.arguments[0].value.kind,
        crate::core::ResolvedExprKind::Load(place)
            if place.base == trait_default_body.parameters[0]
    ));
    let dormant = root_for_owner("function:dormant");
    let activated = root_for_owner("function:activated");
    assert!(dormant.calls().len() == 1 && activated.calls().len() == 1);

    for root in [constant, actor, trait_default, dormant, activated] {
        for call in root.calls() {
            let site = checked
                .call_sites()
                .get(call.call_node())
                .expect("each source-root call is bound to its exact syntax call site");
            assert_eq!(&site.node_id, call.call_node());
            assert_eq!(&site.owner, &root.owner().0);
            assert_eq!(
                site.resolved_callee.as_ref(),
                Some(&crate::core::ir::ResolvedCallee::Extern(
                    call.external_declaration().clone(),
                )),
                "constant, Actor, and dormant/active default roots preserve the exact extern identity"
            );
        }
    }
    let census = crate::core::mir::MirProofSourceCensus::checked_ffi_declaration_roots(&checked);
    census
        .validate_ffi_source_roots()
        .expect("source-only census preserves root kind and activation distinctions");
    let mir_roots = census.ffi_source_roots();
    let mir_root = |root_node: &crate::core::NodeId| {
        mir_roots
            .iter()
            .find(|root| root.root_node() == root_node)
            .expect("MIR source root")
    };
    assert_eq!(
        mir_root(constant.root_node()).execution(),
        crate::core::mir::MirProofSourceFfiRootExecution::ConstantInitializer
    );
    assert_eq!(
        mir_root(actor.root_node()).execution(),
        crate::core::mir::MirProofSourceFfiRootExecution::ActorConstructionPending
    );
    assert_eq!(
        mir_root(trait_default.root_node()).execution(),
        crate::core::mir::MirProofSourceFfiRootExecution::TraitDefaultDeclarationOnly
    );
    assert_eq!(
        mir_root(dormant.root_node()).execution(),
        crate::core::mir::MirProofSourceFfiRootExecution::CallableDefaultDeclarationOnly
    );
    let activated_mir_root = mir_root(activated.root_node());
    assert_eq!(
        activated_mir_root.execution(),
        crate::core::mir::MirProofSourceFfiRootExecution::CallableDefaultActivatedUnsupported
    );
    assert_eq!(activated_mir_root.activations().len(), 1);
    let activation = &activated_mir_root.activations()[0];
    assert_eq!(activation.caller_owner().0, "function:caller");
    let activation_site = checked
        .call_sites()
        .get(activation.call_node())
        .expect("activation is bound to one exact checked call site");
    assert_eq!(&activation_site.node_id, activation.call_node());
    assert_eq!(&activation_site.owner, &activation.caller_owner().0);
    assert_eq!(&activation_site.origin, activation.origin());
    assert_eq!(
        activation_site.target_function.as_ref(),
        Some(activated.owner())
    );
    assert_eq!(
        activation_site.resolved_callee.as_ref(),
        Some(&crate::core::ir::ResolvedCallee::Function(
            activated.owner().clone(),
        ))
    );
    assert_eq!(
        activation.disposition(),
        &crate::core::ir::TypedCallExecutionDisposition::Unconditional
    );

    let results = super::verify_ffi_checked_with_source_hash_and_z3_override(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
        Some(false),
    )
    .expect("source-only declaration roots report boundaries without Z3");
    assert_eq!(results.len(), 3, "{results:?}");
    for root in [constant, actor, activated] {
        let [call] = root.calls() else {
            panic!("one direct FFI call belongs to each reported root");
        };
        assert_eq!(call.external_declaration(), &positive_declaration);
        let context = checked
            .checked_ffi_source_call_context(root, call)
            .expect("exact Checker root/call context");
        let matching = results
            .iter()
            .filter(|result| result.message.contains(&root.root_node().0))
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{root:?}: {results:?}");
        assert_eq!(matching[0].status, VerifStatus::NotInTrustedSubset);
        assert_eq!(matching[0].constraint_count, 0);
        assert!(matching[0].artifact.is_none());
        assert!(matching[0].message.contains(&call.call_node().0));
        assert!(matching[0]
            .message
            .contains(&root.origin().unwrap().user_span().to_string()));
        assert!(matching[0]
            .message
            .contains(&context.call.origin.user_span().to_string()));
    }
    let activated_result = results
        .iter()
        .find(|result| result.message.contains(&activated.root_node().0))
        .expect("activated default boundary");
    assert!(activated_result.message.contains(&activation.call_node().0));
    assert!(activated_result
        .message
        .contains("activated default expansion"));
    assert!(results.iter().all(|result| !result
        .message
        .contains(&trait_default.calls()[0].call_node().0)));
    assert!(results
        .iter()
        .all(|result| !result.message.contains(&dormant.calls()[0].call_node().0)));

    // Source-only census and diagnostics run before any executable MIR
    // adapter attempts to expand this unsupported default. Executable MIR
    // still rejects the expansion when requested independently.
    let executable_error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("an FFI-bearing activated default remains outside executable MIR");
    assert!(
        format!("{executable_error:?}").contains(
            "default expression is outside the receipt-bound scalar local-only MIR profile"
        ),
        "execution MIR stays fail-closed after source-only census has retained the activation: {executable_error:?}"
    );
}

#[test]
fn ffi_variadic_source_roots_use_exact_variadic_arity_and_remain_non_executable() {
    let source = r#"
extern "C" {
    func foreign(head: i32 ...) -> i32 requires: head > 0;
}
const ZERO_TAIL: i32 = foreign(1);
const ONE_TAIL: i32 = foreign(1, 2);
const MANY_TAILS: i32 = foreign(1, 2, 3, 4);
trait Defaults {
    func choose(value: i32 = foreign(1, 5)) -> i32;
}
func dormant(value: i32 = foreign(1, 6)) -> i32 { value }
func activated(value: i32 = foreign(1, 7, 8)) -> i32 { value }
func caller() -> i32 { activated() }
func main() -> i32 { caller() }
"#;
    let file = parse_memory_source(source, "ffi-variadic-source-roots").expect("parse");
    let checked = crate::core::check_program(&file).expect("check variadic declaration roots");
    let roots = checked
        .checked_ffi_source_roots()
        .iter()
        .filter(|root| !root.calls().is_empty())
        .collect::<Vec<_>>();
    let trait_default = roots
        .iter()
        .find(|root| {
            root.kind()
                == crate::core::ir::lower::ResolvedDeclarationExpressionKind::TraitParameterDefault
        })
        .expect("trait default declaration root");
    let trait_default_owner = trait_default.owner().0.clone();
    let mut source_arities = std::collections::BTreeMap::new();
    for root in &roots {
        let [call] = root.calls() else {
            panic!("each fixture root has one variadic FFI source call: {root:?}");
        };
        let context = checked
            .checked_ffi_source_call_context(root, call)
            .expect("exact Checker root/call/declaration binding");
        let crate::core::ir::ResolvedExprKind::Call(resolved_call) = &context.call.kind else {
            panic!("Checker source root resolves to a typed Call");
        };
        let site = checked
            .call_sites()
            .get(call.call_node())
            .expect("exact Checker call-site identity");
        assert_eq!(
            context.declaration.declaration,
            *call.external_declaration()
        );
        assert!(context.declaration.invocation.variadic);
        assert_eq!(site.expected_argc, Some(1));
        assert_eq!(site.argc, resolved_call.arguments.len());
        assert!(site
            .checked_ffi_source_arity_matches(resolved_call.arguments.len(), context.declaration));
        source_arities.insert(site.owner.clone(), site.argc);
    }
    assert_eq!(
        source_arities,
        std::collections::BTreeMap::from([
            ("constant:MANY_TAILS".to_string(), 4),
            ("constant:ONE_TAIL".to_string(), 2),
            ("constant:ZERO_TAIL".to_string(), 1),
            ("function:activated".to_string(), 3),
            ("function:dormant".to_string(), 2),
            (trait_default_owner, 2),
        ]),
        "root receipts preserve the fixed argument and every variadic actual"
    );

    let source_census =
        crate::core::mir::MirProofSourceCensus::checked_ffi_declaration_roots(&checked);
    source_census
        .validate_ffi_source_roots()
        .expect("source-only census retains valid variadic root identity");
    let find_root = |kind: crate::core::ir::lower::ResolvedDeclarationExpressionKind,
                     owner: &str| {
        roots
            .iter()
            .find(|root| root.kind() == kind && root.owner().0 == owner)
            .expect("root by kind and owner")
    };
    let dormant = find_root(
        crate::core::ir::lower::ResolvedDeclarationExpressionKind::CallableParameterDefault,
        "function:dormant",
    );
    let activated = find_root(
        crate::core::ir::lower::ResolvedDeclarationExpressionKind::CallableParameterDefault,
        "function:activated",
    );
    let source_root = |root: &crate::core::ir::lower::CheckedFfiSourceRoot| {
        source_census
            .ffi_source_roots()
            .iter()
            .find(|source_root| source_root.root_node() == root.root_node())
            .expect("source census root")
    };
    assert_eq!(
        source_root(dormant).execution(),
        crate::core::mir::MirProofSourceFfiRootExecution::CallableDefaultDeclarationOnly
    );
    assert_eq!(
        source_root(trait_default).execution(),
        crate::core::mir::MirProofSourceFfiRootExecution::TraitDefaultDeclarationOnly
    );
    assert_eq!(
        source_root(activated).execution(),
        crate::core::mir::MirProofSourceFfiRootExecution::CallableDefaultActivatedUnsupported
    );
    assert_eq!(source_root(activated).activations().len(), 1);

    let results = super::verify_ffi_checked_with_source_hash_and_z3_override(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
        Some(false),
    )
    .expect("source-root verifier reports boundaries without executable admission");
    assert_eq!(
        results.len(),
        4,
        "three constant roots and the active default: {results:?}"
    );
    for root in roots.iter().filter(|root| {
        root.kind()
            == crate::core::ir::lower::ResolvedDeclarationExpressionKind::ConstantInitializer
            || root.owner() == activated.owner()
    }) {
        let [call] = root.calls() else {
            panic!("one source call belongs to the reported root");
        };
        let context = checked
            .checked_ffi_source_call_context(root, call)
            .expect("exact report root/call context");
        let matching_results = results
            .iter()
            .filter(|result| result.message.contains(&root.root_node().0))
            .collect::<Vec<_>>();
        let [result] = matching_results.as_slice() else {
            panic!("one exact NITS result must bind the root: {root:?} / {results:#?}");
        };
        assert_eq!(result.status, VerifStatus::NotInTrustedSubset);
        assert_eq!(result.constraint_count, 0);
        assert!(result.artifact.is_none());
        assert!(result.message.contains(&call.call_node().0));
        assert!(result
            .message
            .contains(&root.origin().unwrap().user_span().to_string()));
        assert!(result
            .message
            .contains(&context.call.origin.user_span().to_string()));
        assert_eq!(
            result.diagnostic.as_ref().unwrap().span,
            context.call.origin.user_span()
        );
    }
    for root in [dormant, trait_default] {
        assert!(results
            .iter()
            .all(|result| !result.message.contains(&root.root_node().0)));
    }

    let executable_source = r#"
extern "C" { func foreign(head: i32 ...) -> i32 }
func main() -> i32 { foreign(1, 2) }
"#;
    let executable_file =
        parse_memory_source(executable_source, "ffi-variadic-executable-reject").expect("parse");
    let executable_checked =
        crate::core::check_program(&executable_file).expect("check executable variadic call");
    let executable_site = executable_checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:main" && site.callee == "foreign")
        .expect("ordinary body call-site");
    assert_eq!(executable_site.argc, 2);
    assert_eq!(executable_site.expected_argc, Some(1));
    assert!(!executable_site.arity_matches());
    let route_error = crate::core::mir::materialize_canonical_mir_route(&executable_checked, None)
        .expect_err("variadic declaration remains outside executable ScalarFfi admission");
    assert!(
        format!("{route_error:?}").contains("uses variadic ABI outside canonical scalar FFI"),
        "executable route keeps the existing variadic rejection: {route_error:?}"
    );
}

#[test]
fn ffi_flow_transition_literal_requires_isolated_verifier_slice() {
    require_z3!();
    let source = r#"
        extern "C" {
            func nonempty(s: string) -> i32 requires: s != "";
        }
        flow EmptyInput {
            state Open { n: i32 }
            transition step(Open) -> Open {
                let checked = nonempty("")
                return Open { n: 0 }
            }
        }
        flow ReadyInput {
            state Open { n: i32 }
            transition step(Open) -> Open {
                let checked = nonempty("ready")
                return Open { n: 0 }
            }
        }
        flow NonLiteralInput {
            state Open { n: i32 }
            transition step(Open) -> Open {
                let input = "ready"
                let checked = nonempty(input)
                return Open { n: 0 }
            }
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-flow-literal-slice").expect("parse");
    let checked = crate::core::check_program(&file).expect("check Flow callers");
    let sites = checked
        .call_sites_sorted()
        .into_iter()
        .filter(|site| {
            site.kind == crate::core::ResolvedCallKind::Extern && site.callee == "nonempty"
        })
        .collect::<Vec<_>>();
    assert_eq!(sites.len(), 3, "one exact source call per transition");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
        .expect("the supported Flow callsites verify through isolated MIR slices");
    assert_eq!(results.len(), 3, "one result for each exact Requires call");

    for site in &sites {
        let matching = results
            .iter()
            .filter(|result| result.message.contains(&site.node_id.0))
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "exact callsite result for {site:?}");
        let result = matching[0];
        assert!(result.message.contains(site.owner.as_str()));
        match site.owner.as_str() {
            "transition:EmptyInput::step::Open" => {
                assert_eq!(result.status, VerifStatus::Disproven, "{result:?}");
                let artifact = result
                    .artifact
                    .as_ref()
                    .expect("MIR counterexample artifact");
                assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
                assert_eq!(
                    artifact.proof_profile,
                    ProofArtifact::PROFILE_MIR_FFI_EXPERIMENTAL_V1
                );
                assert_eq!(
                    artifact.obligation_scope,
                    ProofArtifact::SCOPE_FFI_REQUIRES_ONLY_V1
                );
                assert_eq!(artifact.source_hash, source_hash);
                assert_eq!(
                    artifact
                        .mir_route_receipt
                        .as_ref()
                        .map(|receipt| receipt.profile.as_str()),
                    Some("verifier-ffi-owner-slice-v1")
                );
                let mir = crate::core::mir::reference::MirProgram::from_checked_program_ffi_flow_transition(
                    &checked,
                    &crate::core::NodeId(site.owner.clone()),
                    &site.node_id,
                )
                .expect("exact transition gets verifier-only MIR");
                assert_eq!(
                    mir.purpose(),
                    crate::core::mir::reference::MirProgramPurpose::VerifierOnly
                );
            }
            "transition:ReadyInput::step::Open" => {
                assert_eq!(result.status, VerifStatus::Proven, "{result:?}");
                let artifact = result.artifact.as_ref().expect("MIR proof artifact");
                assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
                assert_eq!(
                    artifact.proof_profile,
                    ProofArtifact::PROFILE_MIR_FFI_EXPERIMENTAL_V1
                );
                assert_eq!(
                    artifact.obligation_scope,
                    ProofArtifact::SCOPE_FFI_REQUIRES_ONLY_V1
                );
                assert_eq!(artifact.source_hash, source_hash);
                assert_eq!(
                    artifact
                        .mir_route_receipt
                        .as_ref()
                        .map(|receipt| receipt.profile.as_str()),
                    Some("verifier-ffi-owner-slice-v1")
                );
            }
            "transition:NonLiteralInput::step::Open" => {
                assert_eq!(result.status, VerifStatus::NotInTrustedSubset, "{result:?}");
                assert!(
                    result.artifact.is_none(),
                    "unsupported shape has no proof artifact"
                );
            }
            owner => panic!("unexpected Flow owner {owner:?}"),
        }
    }
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
    let (default_parameter, default_expression) = caller
        .body
        .default_values
        .iter()
        .next()
        .expect("caller has one declaration default");
    let expected_default_result = format!(
        "default-proof:{}:{}:{}",
        caller.owner.0, default_parameter.0 .0, default_expression.node_id.0
    );
    assert!(
        caller
            .proof_point_worklist()
            .points
            .iter()
            .all(|point| point.id.statement != math_root),
        "the fixture exercises a real root that has no callable-body proof point"
    );

    let results = verify_ffi_checked(&checked).expect("typed call plus deferred Math root");
    let call = results
        .iter()
        .find(|result| result.func_name == "function:caller")
        .expect("the FFI call obligation remains visible");
    assert_eq!(call.status, VerifStatus::Proven, "{results:?}");
    assert!(call.artifact.is_some());

    let deferred = results
        .iter()
        .find(|result| result.func_name == expected_default_result)
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
    assert_eq!(good.body.math_proof_roots().len(), 1);
    assert_eq!(bad.body.math_proof_roots().len(), 1);
    let default_result_identity = |callable: &crate::core::ResolvedCallable| {
        let (parameter, expression) = callable
            .body
            .default_values
            .iter()
            .next()
            .expect("fixture callable has one declaration default");
        format!(
            "default-proof:{}:{}:{}",
            callable.owner.0, parameter.0 .0, expression.node_id.0
        )
    };
    let good_default_result = default_result_identity(good);
    let bad_default_result = default_result_identity(bad);

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

    let results = verify_ffi_checked(&checked).expect("typed calls plus scoped default Math");
    for (name, expected, default_result) in [
        ("good", VerifStatus::Proven, &good_default_result),
        ("bad", VerifStatus::Disproven, &bad_default_result),
    ] {
        let call = results
            .iter()
            .find(|result| result.func_name == format!("function:{name}"))
            .expect("the body FFI call obligation remains independently visible");
        assert_eq!(call.status, VerifStatus::Proven, "{results:?}");
        let default = results
            .iter()
            .find(|result| &result.func_name == default_result)
            .expect("the exact default Math root has its own result");
        assert_eq!(default.status, expected, "{results:?}");
        assert!(
            default
                .artifact
                .as_ref()
                .is_some_and(|artifact| artifact.engine == ProofArtifact::ENGINE_MIR),
            "an executable default Math root must carry its own proof artifact: {results:?}"
        );
    }
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
    assert_eq!(good.body.math_proof_roots().len(), 1);
    assert_eq!(bad.body.math_proof_roots().len(), 1);
    let default_result_identity = |callable: &crate::core::ResolvedCallable| {
        let (parameter, expression) = callable
            .body
            .default_values
            .iter()
            .next()
            .expect("fixture callable has one declaration default");
        format!(
            "default-proof:{}:{}:{}",
            callable.owner.0, parameter.0 .0, expression.node_id.0
        )
    };
    let good_default_result = default_result_identity(good);
    let bad_default_result = default_result_identity(bad);

    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("canonical ScalarFfi route for the formerly supported call context");
    let scalar_ffi = crate::core::mir::CanonicalMirRouteProfile::ScalarFfi;
    assert!(scalar_ffi.is_admitted(route.admission));
    assert!(scalar_ffi.is_materialized(&route));
    let canonical = &route.program;
    let expected_receipt = canonical.route_receipt("verify-ffi-v1");
    let results = super::verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
        .expect("public production FFI verifier entrypoint");
    assert_eq!(
        results.len(),
        4,
        "one result per call site and default Math root: {results:?}"
    );
    let result = |name: &str| {
        results
            .iter()
            .find(|result| result.func_name == name)
            .unwrap_or_else(|| panic!("missing {name}: {results:?}"))
    };
    assert_eq!(
        result("function:good").status,
        VerifStatus::Proven,
        "the caller Requires is available while proving the pre-call Requires"
    );
    assert_eq!(
        result("function:bad").status,
        VerifStatus::Disproven,
        "the foreign Ensures cannot constrain its fresh result"
    );
    assert!(result("function:good")
        .message
        .contains("extern requires contract proven"));
    assert!(result("function:good")
        .message
        .contains("extern ensures contract proven"));
    assert!(result("function:bad")
        .message
        .contains("extern requires contract proven"));
    assert!(result("function:bad")
        .message
        .contains("extern ensures contract disproven"));
    assert!(
        results
            .iter()
            .all(|result| result.func_name != "bad Ensures"),
        "the unproved foreign Ensures must not appear as a caller postcondition proof"
    );
    assert_eq!(
        results
            .iter()
            .find(|result| result.func_name == good_default_result)
            .expect("good default Math result")
            .status,
        VerifStatus::Proven
    );
    assert_eq!(
        results
            .iter()
            .find(|result| result.func_name == bad_default_result)
            .expect("bad default Math result")
            .status,
        VerifStatus::Disproven,
        "default Math is replayed in its own scope, without borrowing the foreign Ensures"
    );
    assert!(result("function:bad").artifact.is_some());
    for proof in &results {
        let artifact = proof
            .artifact
            .as_ref()
            .unwrap_or_else(|| panic!("definitive MIR proof has no artifact: {proof:?}"));
        assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR, "{proof:?}");
        assert_eq!(artifact.source_hash, source_hash, "{proof:?}");
        assert!(artifact.resolved_ir_hash.is_empty(), "{proof:?}");
        assert_eq!(artifact.mir_hash, expected_receipt.mir_digest, "{proof:?}");
        assert_eq!(
            artifact.mir_route_receipt.as_ref(),
            Some(&expected_receipt),
            "{proof:?}"
        );
    }
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
    let default_result_identity = |callable: &crate::core::ResolvedCallable| {
        let (parameter, expression) = callable
            .body
            .default_values
            .iter()
            .next()
            .expect("fixture callable has one declaration default");
        format!(
            "default-proof:{}:{}:{}",
            callable.owner.0, parameter.0 .0, expression.node_id.0
        )
    };
    let good_default_result = default_result_identity(good);
    let bad_default_result = default_result_identity(bad);

    let results = verify_ffi_checked(&checked).expect("canonical Ensures and default roots");
    assert_eq!(
        results.len(),
        4,
        "one call-site result and one result per Math root: {results:?}"
    );
    let result = |name: &str| {
        results
            .iter()
            .find(|result| result.func_name == name)
            .unwrap_or_else(|| panic!("missing {name}: {results:?}"))
    };
    for owner in ["good", "bad"] {
        assert_eq!(
            result(&format!("function:{owner}")).status,
            VerifStatus::Proven,
            "the tautological foreign Ensures is checked independently"
        );
    }
    for (root, result_identity, expected) in [
        (&good_root, &good_default_result, VerifStatus::Proven),
        (&bad_root, &bad_default_result, VerifStatus::Disproven),
    ] {
        let default = results
            .iter()
            .find(|result| &result.func_name == result_identity)
            .unwrap_or_else(|| panic!("missing independent Math root {}: {results:?}", root.0));
        assert_eq!(default.status, expected, "{results:?}");
        assert!(
            default
                .artifact
                .as_ref()
                .is_some_and(|artifact| artifact.engine == ProofArtifact::ENGINE_MIR),
            "each supported Math root has a separate proof artifact: {results:?}"
        );
    }
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
    let results = verify_ffi_checked(&checked)
        .expect("public checked FFI API keeps Deferred Ensures visible");
    let ensures = results
        .iter()
        .find(|result| result.func_name == "caller -> extern foreign Ensures")
        .expect("called Deferred Ensures retains its named obligation");
    let call_site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:caller" && site.callee == "foreign")
        .expect("checker-owned extern call site");
    assert_eq!(ensures.status, VerifStatus::NotInTrustedSubset);
    assert!(ensures.artifact.is_none());
    assert_eq!(
        ensures.trusted_subset_domain,
        Some(TrustedSubsetDomain::Contract)
    );
    assert_eq!(
        ensures
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    );
    assert_eq!(
        ensures.diagnostic.as_ref().unwrap().span,
        call_site.origin.user_span()
    );
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
    let results = verify_ffi_checked(&checked).expect("explicit compatibility context");
    assert_eq!(results.len(), 1, "conditional actual argument: {results:?}");
    assert_eq!(results[0].status, VerifStatus::Proven, "{results:?}");
    assert_eq!(
        results[0]
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR),
        "expression-only conditional actuals use the checker-owned typed receipt"
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
        Some(ProofArtifact::ENGINE_MIR),
        "the conditional counterexample must retain checker-owned provenance"
    );
}

#[test]
fn typed_ffi_short_circuit_and_actual_integer_width_control_definedness() {
    require_z3!();
    for (predicate, argument, status) in [
        ("false && 1 / n > 0", "0", VerifStatus::Disproven),
        ("true || 1 / n > 0", "0", VerifStatus::Proven),
        ("n + 1 > n", "2147483647", VerifStatus::Proven),
        ("n + 1 > n", "2147483646", VerifStatus::Proven),
    ] {
        // A string parameter keeps this outside the closed scalar executable
        // island, exercising the real typed FFI compatibility migration.
        let source = format!(
            "extern \"C\" {{ func foreign(s: string, n: i32) -> i32 requires: {predicate}; }}\nfunc caller() -> i32 {{ foreign(\"ok\", {argument}) }}"
        );
        let file = parse_memory_source(&source, "typed-ffi-definedness").expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let results = verify_ffi_checked(&checked).expect("typed predicate definedness");
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].status, status,
            "{predicate} with {argument}: {results:?}"
        );
    }
}

#[test]
fn typed_ffi_div_rem_requires_use_canonical_operand_width_and_short_circuit() {
    require_z3!();
    for (declaration, ty, defined_domain_status) in [
        ("", "i32", VerifStatus::Proven),
        ("", "i64", VerifStatus::Disproven),
        ("type Count = i32", "Count", VerifStatus::Proven),
    ] {
        for (predicate, status) in [
            ("n % -1 == 0", defined_domain_status.clone()),
            // For every defined signed integer, division by -1 equals -n.
            // On an i32 ABI slot the predicate is defined over the complete
            // domain after sign-extension; on i64, MIN is a real counterexample.
            ("n / -1 == -n", defined_domain_status),
            ("true || n % -1 == 0", VerifStatus::Proven),
            ("false && n % -1 == 0", VerifStatus::Disproven),
        ] {
            let source = format!(
                "{declaration}\nextern \"C\" {{ func foreign(s: string, n: {ty}) -> i32 requires: {predicate}; }}\nfunc caller(n: {ty}) -> i32 {{ foreign(\"ok\", n) }}"
            );
            let file = parse_memory_source(&source, "typed-ffi-remainder-width").expect("parse");
            let checked = crate::core::check_program(&file).expect("check");
            let results = verify_ffi_checked(&checked).expect("typed div/rem Requires");
            assert_eq!(results.len(), 1, "{source}: {results:?}");
            assert_eq!(results[0].status, status, "{source}: {results:?}");
            assert_eq!(
                results[0].artifact.as_ref().unwrap().engine,
                ProofArtifact::ENGINE_MIR
            );
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
            let results = verify_ffi_checked(&checked).expect("typed actual argument definedness");
            assert_eq!(results.len(), 1, "{source}: {results:?}");
            assert_eq!(results[0].status, status, "{source}: {results:?}");
            assert_eq!(
                results[0].artifact.as_ref().unwrap().engine,
                ProofArtifact::ENGINE_MIR
            );
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
        let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("checker-owned typed String FFI MIR");
        let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(
            !admission.scalar_ffi,
            "StringHandle must stay outside execution admission"
        );
        assert!(
            !crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_admitted(admission),
            "the proof-only String slice must not widen ScalarFfi execution admission"
        );
        let execution_route_error =
            crate::core::mir::materialize_canonical_mir_route(&checked, None)
                .expect_err("ScalarFfi production routing remains closed to StringHandle");
        assert!(
            execution_route_error
                .to_string()
                .contains("scalar FFI declaration boundary"),
            "execution route must expose its existing boundary: {execution_route_error}"
        );
        let capability_errors = crate::verifier::validate_mir_capabilities(&canonical)
            .expect_err("ordinary MIR execution capability remains scalar-only");
        assert!(
            capability_errors
                .iter()
                .any(|error| error.contains("StringHandle") || error.contains("scalar endpoint")),
            "execution capability must retain the StringHandle rejection: {capability_errors:?}"
        );
        let results = verify_ffi_checked(&checked).expect("exact typed String proof");
        assert_eq!(results.len(), 1, "{source}: {results:?}");
        assert_eq!(results[0].status, status, "{source}: {results:?}");
        assert_eq!(
            results[0].artifact.as_ref().unwrap().engine,
            ProofArtifact::ENGINE_MIR
        );
        let artifact = results[0]
            .artifact
            .as_ref()
            .expect("verifier-only MIR proof");
        assert_eq!(
            artifact
                .mir_route_receipt
                .as_ref()
                .map(|receipt| receipt.profile.as_str()),
            Some("verifier-ffi-owner-slice-v1"),
            "the proof uses its isolated verifier-only caller-slice receipt"
        );
    }
}

#[test]
fn typed_ffi_integer_parameter_domains_preserve_safe_inputs_and_unsafe_requires() {
    require_z3!();
    for (declaration, ty, maximum) in [
        ("", "i32", "2147483647"),
        ("type Count = i32", "Count", "2147483647"),
        ("", "i64", "9223372036854775807"),
    ] {
        for (condition, status) in [
            (format!("x < {maximum}"), VerifStatus::Proven),
            (format!("x == {maximum}"), VerifStatus::Disproven),
        ] {
            let source = format!(
                "{declaration}\nextern \"C\" {{ func foreign(s: string, n: {ty}) -> i32 requires: n < {maximum}; }}\nfunc caller(x: {ty}) -> i32 {{ requires: {condition}\nforeign(\"ok\", x) }}"
            );
            let file =
                parse_memory_source(&source, "typed-ffi-entry-integer-domain").expect("parse");
            let checked = crate::core::check_program(&file).expect("check bounded entry");
            let results = verify_ffi_checked(&checked).expect("exact machine parameter domain");
            assert_eq!(results.len(), 1, "{source}: {results:?}");
            assert_eq!(results[0].status, status, "{source}: {results:?}");
            assert_eq!(
                results[0].artifact.as_ref().unwrap().engine,
                ProofArtifact::ENGINE_MIR
            );
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
        assert_eq!(
            results[0]
                .diagnostic
                .as_ref()
                .and_then(|diagnostic| diagnostic.code.as_deref()),
            Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
        assert!(results[0].artifact.is_none());
    }
}

#[test]
fn typed_ffi_string_proof_rejects_forged_checker_source_receipt() {
    require_z3!();
    let source = r#"
        extern "C" { func foreign(s: string) -> i32 requires: s == "ok"; }
        func caller(s: string) -> i32 { foreign(s) }
    "#;
    let file = parse_memory_source(source, "typed-ffi-string-forged-source").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let mut canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("typed String FFI MIR");
    let instruction = canonical
        .ffi_calls()
        .keys()
        .next()
        .expect("one checker-sealed FFI call")
        .clone();
    let mut receipts = canonical.ffi_calls().clone();
    let contract = receipts.get_mut(&instruction).expect("FFI receipt");
    let Some(crate::core::mir::MirContractExpr::Binary { right, .. }) = contract.requires.as_mut()
    else {
        panic!("expected direct String equality in Checker receipt");
    };
    **right = crate::core::mir::MirContractExpr::String("forged".into());
    canonical.replace_ffi_calls_for_test_only(receipts);
    let forged_route = canonical.route_receipt("verifier-ffi-mir-v1");
    forged_route
        .validate_against_program(&canonical)
        .expect("fresh route digest alone does not authenticate Checker source");

    let error = crate::verifier::verify_ffi_mir_with_route_receipt(
        &canonical,
        &forged_route,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect_err("a newly hashed graph must still fail its sealed Checker source receipt");
    assert!(error.contains("checked-source witness"), "{error}");
}

#[test]
fn typed_ffi_symbolic_string_contexts_defer_without_legacy_access() {
    require_z3!();
    // Even a NUL-free literal does not make `s == "a"` exact for arbitrary
    // runtime `s`: native strcmp also accepts bytes `a\0tail`. The MIR proof
    // caller therefore keeps these rich symbolic String equality/char_at
    // contexts outside the isolated top-level FFI caller slice. They remain
    // owner-visible deferrals rather than assumptions or legacy body reads.
    for (literal, first, length) in [
        (r#""🧪值""#, r#""🧪""#, 2),
        (r#""\u{1F9EA}值""#, r#""\u{1F9EA}""#, 2),
        (r#""\\u{41}""#, r#""\\""#, 6),
        (r#""a""#, r#""a""#, 1),
        (r#""a\0🧪值""#, r#""a""#, 4),
        (r#""\0a""#, r#""\0""#, 2),
    ] {
        for claimed_length in [length, length + 1] {
            let source = format!(
                "extern \"C\" {{ func foreign(s: string) -> i32 requires: len(s) == {claimed_length}; }}\nfunc caller(s: string) -> i32 {{ requires: s == {literal} && char_at(s, 0) == {first}\nforeign(s) }}"
            );
            let file =
                parse_memory_source(&source, "typed-ffi-exact-deferred-string").expect("parse");
            let checked = crate::core::check_program(&file).expect("legal rich String context");
            let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            let results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
                .expect("checker-owned explicit String boundary");
            assert_eq!(results.len(), 1, "{source}: {results:?}");
            assert_eq!(
                results[0].status,
                VerifStatus::NotInTrustedSubset,
                "String-valued caller predicates cannot prove the callee's length condition: {source}: {results:?}"
            );
            assert!(
                results[0].artifact.is_none(),
                "no proof artifact on deferral"
            );
            assert!(
                results[0].message.contains(
                    "String-valued FFI caller predicates remain outside the trusted subset"
                ),
                "the owner-scoped deferral must name the String proof boundary: {results:?}"
            );
            assert!(
                results[0]
                    .message
                    .contains("foreign String ingress validates UTF-8"),
                "the deferral must preserve the ingress constraint: {results:?}"
            );
        }
    }
}

#[test]
fn typed_ffi_char_at0_empty_input_context_is_explicitly_deferred() {
    require_z3!();
    let source = r#"
        extern "C" { func foreign(s: string) -> i32 requires: len(s) == 0; }
        func caller(s: string) -> i32 {
            requires: s == "" && char_at(s, 0) == "a"
            foreign(s)
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-char-at0-empty-input").expect("parse");
    let checked = crate::core::check_program(&file).expect("legal exact String context");
    let results = verify_ffi_checked(&checked).expect("unsupported String caller fact is visible");
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].status, VerifStatus::NotInTrustedSubset);
    assert!(results[0].artifact.is_none());
    assert!(results[0]
        .message
        .contains("String-valued FFI caller predicates"));
}

#[test]
fn typed_ffi_nonzero_char_at_index_remains_outside_trusted_subset() {
    require_z3!();
    let source = r#"
        extern "C" { func foreign(s: string) -> i32 requires: len(s) == 2; }
        func caller(s: string, index: i64) -> i32 {
            requires: s == "ab" && char_at(s, index) == "b"
            foreign(s)
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-nonzero-char-at-index").expect("parse");
    let checked = crate::core::check_program(&file).expect("legal source contract");
    let results = verify_ffi_checked(&checked).expect("unsupported index stays a typed boundary");
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(
        results[0].status,
        VerifStatus::NotInTrustedSubset,
        "{results:?}"
    );
    assert!(results[0].artifact.is_none());
    assert_eq!(
        results[0]
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    );
}

#[test]
fn typed_ffi_guarded_char_at0_requires_match_checked_and_explicit_mir_routes() {
    require_z3!();
    let source = r#"
        extern "C" {
            func foreign_ascii(s: string) -> i32
                requires: len(s) > 0 && char_at(s, 0) == "A";
            func foreign_supplementary(s: string) -> i32
                requires: len(s) > 0 && char_at(s, 0) == "🧪";
        }
        func ascii_positive() -> i32 { foreign_ascii("A tail") }
        func ascii_negative() -> i32 { foreign_ascii("B") }
        func supplementary_positive() -> i32 { foreign_supplementary("🧪tail") }
        func supplementary_negative() -> i32 { foreign_supplementary("🐈") }
        func embedded_nul_positive() -> i32 { foreign_ascii("A\0tail") }
        func embedded_nul_negative() -> i32 { foreign_ascii("B\0tail") }
        func leading_nul_negative() -> i32 { foreign_ascii("\0A") }
        func empty_guarded_negative() -> i32 { foreign_ascii("") }
        func context_positive(s: string) -> i32 {
            requires: len(s) > 0 && char_at(s, 0) == "A"
            foreign_ascii(s)
        }
        func context_negative(s: string) -> i32 {
            requires: len(s) > 0 && char_at(s, 0) == "B"
            foreign_ascii(s)
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-guarded-char-at0").expect("parse");
    let checked = crate::core::check_program(&file).expect("check exact guarded char_at0 roots");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let checked_results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
        .expect("source-bound CheckedProgram FFI proofs");
    assert_eq!(
        checked_results.len(),
        10,
        "all exact FFI call sites: {checked_results:?}"
    );

    let expected = [
        ("ascii_positive", VerifStatus::Proven),
        ("ascii_negative", VerifStatus::Disproven),
        ("supplementary_positive", VerifStatus::Proven),
        ("supplementary_negative", VerifStatus::Disproven),
        ("embedded_nul_positive", VerifStatus::Proven),
        ("embedded_nul_negative", VerifStatus::Disproven),
        ("leading_nul_negative", VerifStatus::Disproven),
        ("empty_guarded_negative", VerifStatus::Disproven),
        ("context_positive", VerifStatus::Proven),
        ("context_negative", VerifStatus::Disproven),
    ];
    for (caller, status) in expected {
        let checked_result = checked_results
            .iter()
            .find(|result| result.func_name.contains(caller))
            .unwrap_or_else(|| {
                panic!("missing CheckedProgram result for {caller}: {checked_results:#?}")
            });
        assert_eq!(
            checked_result.status, status,
            "{caller}: {checked_result:?}"
        );
        let checked_artifact = checked_result.artifact.as_ref().unwrap_or_else(|| {
            panic!("{caller} must have a real proof artifact: {checked_result:?}")
        });
        assert_eq!(
            checked_artifact.engine,
            ProofArtifact::ENGINE_MIR,
            "{caller}"
        );
        assert_eq!(checked_artifact.source_hash, source_hash, "{caller}");
        assert!(checked_artifact.resolved_ir_hash.is_empty(), "{caller}");
        let checked_receipt = checked_artifact
            .mir_route_receipt
            .as_ref()
            .expect("canonical MIR route receipt");
        assert_eq!(
            checked_artifact.mir_hash, checked_receipt.mir_digest,
            "{caller}"
        );
        assert_eq!(
            checked_receipt.profile, "verifier-ffi-owner-slice-v1",
            "{caller}"
        );

        let site = checked
            .call_sites_sorted()
            .into_iter()
            .find(|site| {
                site.owner == format!("function:{caller}")
                    && site.kind == crate::core::ResolvedCallKind::Extern
            })
            .unwrap_or_else(|| panic!("missing exact Checker call-site for {caller}"));
        if status == VerifStatus::Disproven {
            assert_eq!(
                checked_result.diagnostic.as_ref().unwrap().span,
                site.origin.user_span(),
                "{caller} counterexample must bind to its exact source call"
            );
        }

        let owner = crate::core::NodeId(site.owner.clone());
        let canonical = crate::core::mir::reference::MirProgram::from_checked_program_ffi_caller(
            &checked, &owner,
        )
        .unwrap_or_else(|error| panic!("{caller} Checker-bound MIR: {error:?}"));
        let receipt = canonical.route_receipt("verifier-ffi-owner-slice-v1");
        let mir_results = crate::verifier::verify_ffi_mir_requires_with_route_receipt(
            &canonical,
            &receipt,
            source_hash.clone(),
        )
        .unwrap_or_else(|error| panic!("{caller} explicit MIR verifier: {error}"));
        assert_eq!(mir_results.len(), 1, "{caller}: {mir_results:?}");
        assert_eq!(mir_results[0].status, status, "{caller}: {mir_results:?}");
        let mir_artifact = mir_results[0]
            .artifact
            .as_ref()
            .unwrap_or_else(|| panic!("{caller} explicit MIR proof artifact: {mir_results:?}"));
        assert_eq!(mir_artifact.engine, ProofArtifact::ENGINE_MIR, "{caller}");
        assert_eq!(mir_artifact.source_hash, source_hash, "{caller}");
        assert_eq!(mir_artifact.mir_hash, receipt.mir_digest, "{caller}");
        assert_eq!(
            mir_artifact.mir_route_receipt.as_ref(),
            Some(&receipt),
            "{caller}"
        );
        if status == VerifStatus::Disproven {
            assert_eq!(
                mir_results[0].diagnostic.as_ref().unwrap().span,
                site.origin.user_span(),
                "{caller} explicit-MIR counterexample must bind to the call site"
            );
        }
    }
}

#[test]
fn typed_ffi_char_at0_rejects_unprofiled_declaration_and_context_shapes() {
    require_z3!();
    let declaration_cases = [
        (
            "unguarded",
            r#"
                extern "C" { func foreign(s: string) -> i32 requires: char_at(s, 0) == "A"; }
                func caller() -> i32 { foreign("A") }
            "#,
        ),
        (
            "not_equal",
            r#"
                extern "C" { func foreign(s: string) -> i32 requires: len(s) > 0 && char_at(s, 0) != "A"; }
                func caller() -> i32 { foreign("B") }
            "#,
        ),
        (
            "not_operator",
            r#"
                extern "C" { func foreign(s: string) -> i32 requires: len(s) > 0 && not (char_at(s, 0) == "A"); }
                func caller() -> i32 { foreign("B") }
            "#,
        ),
        (
            "or_operator",
            r#"
                extern "C" { func foreign(s: string) -> i32 requires: len(s) == 0 || char_at(s, 0) == "A"; }
                func caller() -> i32 { foreign("A") }
            "#,
        ),
        (
            "nested_len",
            r#"
                extern "C" { func foreign(s: string) -> i32 requires: len(s) > 0 && len(char_at(s, 0)) == 1; }
                func caller() -> i32 { foreign("A") }
            "#,
        ),
        (
            "nonzero_index",
            r#"
                extern "C" { func foreign(s: string) -> i32 requires: len(s) > 1 && char_at(s, 1) == "A"; }
                func caller() -> i32 { foreign("xA") }
            "#,
        ),
        (
            "dynamic_index",
            r#"
                extern "C" { func foreign(s: string, index: i64) -> i32 requires: len(s) > 0 && char_at(s, index) == "A"; }
                func caller() -> i32 { foreign("A", 0) }
            "#,
        ),
        (
            "empty_literal",
            r#"
                extern "C" { func foreign(s: string) -> i32 requires: len(s) > 0 && char_at(s, 0) == ""; }
                func caller() -> i32 { foreign("A") }
            "#,
        ),
        (
            "nul_literal",
            r#"
                extern "C" { func foreign(s: string) -> i32 requires: len(s) > 0 && char_at(s, 0) == "\0"; }
                func caller() -> i32 { foreign("A") }
            "#,
        ),
        (
            "above_scalar_limit",
            r#"
                extern "C" { func foreign(s: string) -> i32 requires: len(s) > 0 && char_at(s, 0) == "𰀀"; }
                func caller() -> i32 { foreign("𰀀") }
            "#,
        ),
        (
            "open_rhs",
            r#"
                extern "C" { func foreign(s: string, expected: string) -> i32 requires: len(s) > 0 && char_at(s, 0) == expected; }
                func caller() -> i32 { foreign("A", "A") }
            "#,
        ),
        (
            "field_projection",
            r#"
                type Holder { text: string }
                extern "C" { func foreign(s: string) -> i32 requires: len(s) > 0 && char_at(s, 0) == "A"; }
                func caller(value: Holder) -> i32 {
                    requires: len(value.text) > 0 && char_at(value.text, 0) == "A"
                    foreign(value.text)
                }
            "#,
        ),
    ];

    for (case, source) in declaration_cases {
        let filename = format!("typed-ffi-char-at0-{case}");
        let file = parse_memory_source(source, &filename)
            .unwrap_or_else(|error| panic!("{case}: parse: {error:?}"));
        let checked = crate::core::check_program(&file).unwrap_or_else(|diagnostics| {
            panic!("{case}: checker should preserve a typed boundary: {diagnostics:?}")
        });
        let results =
            verify_ffi_checked(&checked).unwrap_or_else(|error| panic!("{case}: verify: {error}"));
        assert_eq!(results.len(), 1, "{case}: {results:?}");
        assert_eq!(
            results[0].status,
            VerifStatus::NotInTrustedSubset,
            "{case}: {results:?}"
        );
        assert_eq!(results[0].constraint_count, 0, "{case}: {results:?}");
        assert!(results[0].artifact.is_none(), "{case}: {results:?}");
        assert_eq!(
            results[0]
                .diagnostic
                .as_ref()
                .and_then(|diagnostic| diagnostic.code.as_deref()),
            Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
            "{case}: unsupported source form has an explicit coverage diagnostic"
        );
    }

    let caller_context_cases = [
        (
            "not_equal",
            r#"
                extern "C" { func foreign(s: string) -> i32 requires: len(s) > 0; }
                func caller(s: string) -> i32 {
                    requires: len(s) > 0 && char_at(s, 0) != "B"
                    foreign(s)
                }
            "#,
        ),
        (
            "nonzero_index",
            r#"
                extern "C" { func foreign(s: string) -> i32 requires: len(s) > 0; }
                func caller(s: string) -> i32 {
                    requires: len(s) > 1 && char_at(s, 1) == "A"
                    foreign(s)
                }
            "#,
        ),
        (
            "dynamic_index",
            r#"
                extern "C" { func foreign(s: string) -> i32 requires: len(s) > 0; }
                func caller(s: string, index: i64) -> i32 {
                    requires: len(s) > 0 && char_at(s, index) == "A"
                    foreign(s)
                }
            "#,
        ),
    ];
    for (case, context_source) in caller_context_cases {
        let filename = format!("typed-ffi-char-at0-unsupported-context-{case}");
        let file = parse_memory_source(context_source, &filename)
            .unwrap_or_else(|error| panic!("{case}: parse unsupported caller context: {error:?}"));
        let checked = crate::core::check_program(&file).unwrap_or_else(|diagnostics| {
            panic!("{case}: check unsupported caller context: {diagnostics:?}")
        });
        let results = verify_ffi_checked(&checked)
            .unwrap_or_else(|error| panic!("{case}: caller-context verification: {error}"));
        assert_eq!(results.len(), 1, "{case}: {results:?}");
        assert_eq!(
            results[0].status,
            VerifStatus::NotInTrustedSubset,
            "{case}: {results:?}"
        );
        assert_eq!(results[0].constraint_count, 0, "{case}: {results:?}");
        assert!(results[0].artifact.is_none(), "{case}: {results:?}");
        assert_eq!(
            results[0]
                .diagnostic
                .as_ref()
                .and_then(|diagnostic| diagnostic.code.as_deref()),
            Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
            "{case}: unsupported caller source form has an explicit coverage diagnostic"
        );
    }

    let ensures_source = r#"
        extern "C" {
            func foreign(s: string) -> string
                ensures: char_at(old(s), 0) == "A" && char_at(result, 0) == "A";
        }
        func caller() -> string { foreign("A") }
    "#;
    let file = parse_memory_source(ensures_source, "typed-ffi-char-at0-ensures-old-result")
        .expect("parse Ensures old/result boundary");
    let checked = crate::core::check_program(&file).expect("check Ensures old/result boundary");
    let results = verify_ffi_checked(&checked).expect("Ensures stays a typed boundary");
    let caller = results
        .iter()
        .find(|result| result.func_name == "caller calls foreign")
        .unwrap_or_else(|| panic!("missing unsupported Ensures caller result: {results:#?}"));
    assert_eq!(caller.status, VerifStatus::NotInTrustedSubset, "{caller:?}");
    assert_eq!(caller.constraint_count, 0, "{caller:?}");
    assert!(caller.artifact.is_none(), "{caller:?}");
    assert_eq!(
        caller
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    );
    assert!(
        caller
            .diagnostic
            .as_ref()
            .is_some_and(|diagnostic| diagnostic.message.contains("ensures contract")),
        "unsupported Ensures remains explicit in coverage diagnostic: {caller:?}"
    );
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
        assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.source_hash, source_hash);
        assert!(artifact.resolved_ir_hash.is_empty());
        assert_eq!(artifact.mir_hash.len(), 64);
        assert!(artifact.mir_route_receipt.is_some());
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
}

#[test]
fn typed_ffi_aggregate_nested_fields_keep_scalar_leaf_domains() {
    require_z3!();
    for (claimed_ratio, status) in [(2.5, VerifStatus::Proven), (3.5, VerifStatus::Disproven)] {
        let source = format!(
            r#"
            #[repr(C)]
            type Inner {{ value: i32, ratio: f64 }}
            #[repr(C)]
            type Envelope {{ inner: Inner, enabled: bool }}
            extern "C" {{
                func foreign(value: Envelope) -> Envelope
                    requires: value.enabled && value.inner.value > 0 && value.inner.ratio == {claimed_ratio};
            }}
            func caller(value: Envelope) -> Envelope {{
                requires: value.enabled && value.inner.value == 3 && value.inner.ratio == 2.5
                foreign(value)
            }}
        "#
        );
        let file = parse_memory_source(&source, "typed-ffi-nested-record-fields").expect("parse");
        let checked = crate::core::check_program(&file).expect("check nested canonical fields");
        let route = crate::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(
            !route.scalar_ffi,
            "repr(C) record verifier admission must not widen ScalarFfi execution"
        );
        let catalog = crate::core::mir::types::MirTypeCatalog::from_checked_program(&checked)
            .expect("materialize nested record TypeDescs");
        for expected in ["Inner", "Envelope"] {
            let record = catalog
                .iter()
                .find_map(|(id, descriptor)| match &descriptor.layout {
                    crate::core::mir::types::MirLayout::Record { nominal, .. }
                        if nominal.as_str().ends_with(expected) =>
                    {
                        Some(id)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing {expected} TypeDesc"));
            assert!(
                catalog.is_canonical_ffi_verifier_record(record),
                "{expected} must satisfy the recursive scalar-leaf verifier profile"
            );
        }
        let results = verify_ffi_checked(&checked).expect("typed nested scalar-leaf contract");
        assert_eq!(results.len(), 1, "{source}: {results:?}");
        assert_eq!(results[0].status, status, "{source}: {results:?}");
        assert_eq!(
            results[0].artifact.as_ref().unwrap().engine,
            ProofArtifact::ENGINE_MIR
        );
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
        let results = verify_ffi_checked(&checked).expect("bounded record field inputs");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, status, "{source}: {results:?}");
        assert_eq!(
            results[0].artifact.as_ref().unwrap().engine,
            ProofArtifact::ENGINE_MIR
        );
    }
}

#[test]
fn typed_ffi_aggregate_string_field_joins_the_owned_string_proof_profile() {
    require_z3!();
    let source = r#"
        #[repr(C)] type Pair { value: i64, label: string }
        extern "C" {
            func foreign(value: Pair) -> Pair
                requires: value.value >= 0 && len(value.label) > 0;
        }
        func caller(value: Pair) -> Pair {
            requires: value.value >= 0 && len(value.label) > 0
            foreign(value)
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-record-string-leaf").expect("parse");
    let checked = crate::core::check_program(&file).expect("check source-level FFI declaration");
    let catalog = crate::core::mir::types::MirTypeCatalog::from_checked_program(&checked)
        .expect("materialize owned String record TypeDesc");
    let pair = catalog
        .iter()
        .find_map(|(id, descriptor)| match &descriptor.layout {
            crate::core::mir::types::MirLayout::Record { nominal, .. }
                if nominal.as_str().ends_with("Pair") =>
            {
                Some(id)
            }
            _ => None,
        })
        .expect("Pair TypeDesc");
    assert!(catalog.is_canonical_ffi_verifier_string_record(pair));
    let mut malformed = catalog.clone();
    let mut forged_pair = catalog.get(pair).unwrap().clone();
    forged_pair.drop_plan = None;
    malformed.replace_for_test_only(pair.clone(), forged_pair);
    assert!(
        !malformed.is_canonical_ffi_verifier_string_record(pair),
        "String-bearing records need their exact aggregate ownership/drop receipt"
    );
    let results = verify_ffi_checked(&checked).expect("typed aggregate String Requires proof");
    assert_eq!(
        results.len(),
        1,
        "single record FFI obligation: {results:?}"
    );
    assert_eq!(results[0].status, VerifStatus::Proven, "{results:?}");
    assert_eq!(
        results[0].artifact.as_ref().unwrap().engine,
        ProofArtifact::ENGINE_MIR
    );
}

#[test]
fn typed_ffi_aggregate_nested_string_fields_preserve_nul_unicode_and_length() {
    require_z3!();
    for (claimed_length, expected) in [(4, VerifStatus::Proven), (5, VerifStatus::Disproven)] {
        let source = format!(
            r#"
            #[repr(C)] type Inner {{ value: i64, label: string }}
            #[repr(C)] type Envelope {{ inner: Inner, enabled: bool }}
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
        let file = parse_memory_source(&source, "typed-ffi-nested-string-field").expect("parse");
        let checked = crate::core::check_program(&file).expect("check nested String FFI roots");
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
            .expect("nested owned String field proof uses the MIR owner receipt");
        assert_eq!(results.len(), 1, "one call-site obligation: {results:?}");
        let result = &results[0];
        assert_eq!(result.status, expected, "{source}: {results:?}");
        let artifact = result
            .artifact
            .as_ref()
            .expect("supported nested String field proof artifact");
        assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.source_hash, source_hash);
        assert!(artifact.resolved_ir_hash.is_empty());
        assert_eq!(
            artifact
                .mir_route_receipt
                .as_ref()
                .map(|receipt| receipt.profile.as_str()),
            Some("verifier-ffi-owner-slice-v1")
        );
        if expected == VerifStatus::Disproven {
            let call_site = checked
                .call_sites_sorted()
                .into_iter()
                .find(|site| site.owner == "function:caller" && site.callee == "foreign")
                .expect("exact nested String FFI call site");
            assert_eq!(
                result.diagnostic.as_ref().unwrap().span,
                call_site.origin.user_span()
            );
        }
    }
}

#[test]
fn typed_ffi_string_record_ensures_use_checker_receipt_and_fresh_result() {
    require_z3!();
    for (ensures, expected) in [
        ("len(result.label) >= 0", VerifStatus::Proven),
        ("len(result.label) > 0", VerifStatus::Disproven),
    ] {
        let source = format!(
            r#"
            #[repr(C)] type Pair {{ value: i64, label: string }}
            extern "C" {{
                func foreign(value: Pair) -> Pair
                    requires: value.value >= 0 && len(value.label) > 0
                    ensures: {ensures};
            }}
            func caller(value: Pair) -> Pair {{
                requires: value.value >= 0 && len(value.label) > 0
                foreign(value)
            }}
            func main() -> i64 {{ 0 }}
            "#
        );
        let file = parse_memory_source(&source, "typed-ffi-string-record-ensures").expect("parse");
        let checked = crate::core::check_program(&file).expect("check typed FFI roots");
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let checked_results = verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
            .expect("owner-slice verifier proves Ready String-bearing Ensures");
        assert_eq!(checked_results.len(), 1, "{checked_results:?}");
        let checked_result = &checked_results[0];
        assert_eq!(checked_result.func_name, "caller calls foreign");
        assert_eq!(
            checked_result.status, expected,
            "{source}: {checked_result:?}"
        );
        let checked_artifact = checked_result
            .artifact
            .as_ref()
            .expect("Checker owner-slice proof artifact");
        assert_eq!(checked_artifact.engine, ProofArtifact::ENGINE_MIR);
        assert_eq!(
            checked_artifact.obligation_scope,
            ProofArtifact::SCOPE_FFI_REQUIRES_ENSURES_V1
        );
        assert_eq!(checked_artifact.source_hash, source_hash);
        assert_eq!(
            checked_artifact
                .mir_route_receipt
                .as_ref()
                .map(|receipt| receipt.profile.as_str()),
            Some("verifier-ffi-owner-slice-v1")
        );
        let dual_results = verify_checked_dual(&checked, source_hash.clone())
            .expect("default dual entry admits the exact String-record FFI owner slice");
        assert_eq!(dual_results.len(), 1, "{dual_results:?}");
        let dual_result = &dual_results[0];
        assert_eq!(dual_result.func_name, "caller calls foreign");
        assert_eq!(dual_result.status, expected, "{source}: {dual_result:?}");
        let dual_artifact = dual_result
            .artifact
            .as_ref()
            .expect("dual owner-slice proof artifact");
        assert_eq!(dual_artifact.engine, ProofArtifact::ENGINE_MIR);
        assert_eq!(dual_artifact.source_hash, source_hash);
        assert_eq!(
            dual_artifact.obligation_scope,
            ProofArtifact::SCOPE_FFI_REQUIRES_ENSURES_V1
        );
        assert_eq!(
            dual_artifact
                .mir_route_receipt
                .as_ref()
                .map(|receipt| receipt.profile.as_str()),
            Some("verifier-ffi-owner-slice-v1")
        );
        if expected == VerifStatus::Disproven {
            let site = checked
                .call_sites_sorted()
                .into_iter()
                .find(|site| site.owner == "function:caller" && site.callee == "foreign")
                .expect("exact callsite for the foreign postcondition");
            assert_eq!(
                checked_result.diagnostic.as_ref().unwrap().span,
                site.origin.user_span()
            );
            assert_eq!(
                dual_result.diagnostic.as_ref().unwrap().span,
                site.origin.user_span()
            );
        }

        let declaration = checked
            .ffi_declarations()
            .values()
            .find(|declaration| declaration.symbol == "foreign")
            .expect("Checker FFI declaration");
        assert!(matches!(
            &declaration.ensures,
            crate::core::ir::ResolvedFfiContractRoot::Ready { .. }
        ));
        let owner = crate::core::NodeId("function:caller".into());
        let canonical = crate::core::mir::reference::MirProgram::from_checked_program_ffi_caller(
            &checked, &owner,
        )
        .expect("materialize checked caller FFI slice");
        assert_eq!(
            canonical.purpose(),
            crate::core::mir::reference::MirProgramPurpose::VerifierOnly,
            "the String-record proof graph must remain verifier-only"
        );
        let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(
            !crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_admitted(admission),
            "the verifier-only record route must not authorize the ScalarFfi execution profile"
        );
        assert!(
            crate::core::mir::materialize_canonical_mir_route(&checked, None).is_err(),
            "the verifier-only record route must not materialize as an execution route"
        );
        assert!(
            crate::codegen::mir::validate_mir_native(&canonical).is_err(),
            "native codegen must reject the verifier-only record graph"
        );
        assert!(
            crate::interp::bytecode::compile_mir_program(&canonical).is_err(),
            "bytecode must reject the verifier-only record graph"
        );
        let call = canonical
            .ffi_call_entries_in_source_order()
            .into_iter()
            .find(|(_, contract)| contract.caller == owner)
            .expect("Checker/MIR call receipt")
            .1;
        let checked_site = checked
            .call_sites_sorted()
            .into_iter()
            .find(|site| site.owner == owner.0 && site.callee == "foreign")
            .expect("exact Checker callsite identity");
        assert_eq!(call.call_node, checked_site.node_id);
        assert_eq!(call.caller, owner);
        assert_eq!(call.symbol, checked_site.callee);
        assert_eq!(call.source_body_owner, owner);
        assert!(call.ensures.is_some(), "Ready Ensures root is materialized");
        assert!(!call.ensures_deferred, "Ready Ensures root is not Deferred");
        assert!(
            !call.ensures_types.is_empty(),
            "root-local TypeDesc receipt"
        );
        let receipt = canonical.route_receipt("verifier-ffi-string-record-ensures-v1");
        let results = crate::verifier::verify_ffi_mir_with_route_receipt(
            &canonical,
            &receipt,
            blake3::hash(source.as_bytes()).to_hex().to_string(),
        )
        .expect("full Requires and Ensures FFI verifier route");
        assert_eq!(results.len(), 1, "one call-site result: {results:?}");
        assert_eq!(results[0].status, expected, "{source}: {results:?}");
        assert!(results[0]
            .message
            .contains("extern requires contract proven"));
        assert!(
            results[0]
                .message
                .contains(if expected == VerifStatus::Proven {
                    "extern ensures contract proven"
                } else {
                    "extern ensures contract disproven"
                }),
            "foreign postcondition is checked against a fresh aggregate result: {results:?}"
        );
        assert_eq!(
            results[0].artifact.as_ref().unwrap().engine,
            ProofArtifact::ENGINE_MIR
        );
    }
}

#[test]
fn typed_ffi_aggregate_string_requires_near_misses_stay_owner_scoped_nits() {
    require_z3!();
    let cases = [
        (
            "extra-length-call",
            r#"
                #[repr(C)] type Pair { value: i64, label: string }
                extern "C" {
                    func foreign(value: Pair) -> Pair
                        ensures: len(result.label) >= 0;
                }
                func caller(value: Pair) -> Pair {
                    requires: value.value >= 0 && len(value.label) > 0 && len("extra") > 0
                    foreign(value)
                }
                func main() -> i64 { 0 }
            "#,
        ),
        (
            "nested-field-projection",
            r#"
                #[repr(C)] type Inner { label: string }
                #[repr(C)] type Pair { value: i64, inner: Inner }
                extern "C" {
                    func foreign(value: Pair) -> Pair
                        ensures: len(result.inner.label) >= 0;
                }
                func caller(value: Pair) -> Pair {
                    requires: value.value >= 0 && len(value.inner.label) > 0
                    foreign(value)
                }
                func main() -> i64 { 0 }
            "#,
        ),
        (
            "aggregate-field-string-equality-outside-length-guard",
            r#"
                #[repr(C)] type Pair { value: i64, label: string }
                extern "C" {
                    func foreign(value: Pair) -> Pair
                        ensures: len(result.label) >= 0;
                }
                func caller(value: Pair) -> Pair {
                    requires: value.value >= 0 && len(value.label) > 0 && value.label != ""
                    foreign(value)
                }
                func main() -> i64 { 0 }
            "#,
        ),
        (
            "aggregate-length-without-conjunction",
            r#"
                #[repr(C)] type Pair { value: i64, label: string }
                extern "C" {
                    func foreign(value: Pair) -> Pair
                        ensures: len(result.label) >= 0;
                }
                func caller(value: Pair) -> Pair {
                    requires: len(value.label) > 0
                    foreign(value)
                }
                func main() -> i64 { 0 }
            "#,
        ),
        (
            "aggregate-length-under-or",
            r#"
                #[repr(C)] type Pair { value: i64, label: string }
                extern "C" {
                    func foreign(value: Pair) -> Pair
                        ensures: len(result.label) >= 0;
                }
                func caller(value: Pair) -> Pair {
                    requires: value.value >= 0 && (len(value.label) > 0 || value.value == 0)
                    foreign(value)
                }
                func main() -> i64 { 0 }
            "#,
        ),
        (
            "aggregate-duplicate-length-guards",
            r#"
                #[repr(C)] type Pair { value: i64, label: string }
                extern "C" {
                    func foreign(value: Pair) -> Pair
                        ensures: len(result.label) >= 0;
                }
                func caller(value: Pair) -> Pair {
                    requires: value.value >= 0 && len(value.label) > 0 && len(value.label) == 0
                    foreign(value)
                }
                func main() -> i64 { 0 }
            "#,
        ),
    ];

    for (case_name, source) in cases {
        let file = parse_memory_source(source, case_name).expect("parse FFI near-miss source");
        let checked = crate::core::check_program(&file).expect("check FFI near-miss source");
        let results = verify_ffi_checked_with_source_hash(
            &checked,
            blake3::hash(source.as_bytes()).to_hex().to_string(),
        )
        .expect("unsupported owner stays at the FFI MIR NITS boundary");
        assert_eq!(results.len(), 1, "{case_name}: {results:?}");
        let result = &results[0];
        assert_eq!(
            result.status,
            VerifStatus::NotInTrustedSubset,
            "{case_name}: {result:?}"
        );
        assert!(
            result.artifact.is_none(),
            "{case_name}: unsupported owner has no artifact"
        );
        assert_eq!(
            result
                .diagnostic
                .as_ref()
                .and_then(|diagnostic| diagnostic.code.as_deref()),
            Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
            "{case_name}: unsupported owner keeps its owner-scoped route diagnostic"
        );
        let call_site = checked
            .call_sites_sorted()
            .into_iter()
            .find(|site| site.owner == "function:caller" && site.callee == "foreign")
            .expect("exact foreign call site");
        assert_eq!(
            result.diagnostic.as_ref().unwrap().span,
            call_site.origin.user_span(),
            "{case_name}: NITS is bound to the exact foreign call"
        );
    }
}

#[test]
fn typed_ffi_foreign_requires_is_not_assumed_while_proving_ensures() {
    require_z3!();
    let source = r#"
        #[repr(C)] type Pair { value: i64, label: string }
        extern "C" {
            func foreign(value: Pair) -> Pair
                requires: value.value > 0
                ensures: value.value > 0;
        }
        func caller(value: Pair) -> Pair { foreign(value) }
    "#;
    let file =
        parse_memory_source(source, "typed-ffi-foreign-requires-not-assumed").expect("parse");
    let checked = crate::core::check_program(&file).expect("check FFI declaration");
    let results = verify_ffi_checked_with_source_hash(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("owner-slice route checks Requires before any foreign Ensures");
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].status, VerifStatus::Disproven, "{results:?}");
    assert!(results[0]
        .message
        .contains("extern requires contract disproven"));
    assert!(results[0]
        .message
        .contains("Ensures was not evaluated because Requires did not discharge the call"));
    assert!(!results[0]
        .message
        .contains("extern ensures contract proven"));
    assert_eq!(
        results[0].artifact.as_ref().unwrap().engine,
        ProofArtifact::ENGINE_MIR
    );
    let call_site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:caller" && site.callee == "foreign")
        .expect("exact FFI call site");
    assert_eq!(
        results[0].diagnostic.as_ref().unwrap().span,
        call_site.origin.user_span()
    );
}

#[test]
fn typed_ffi_deferred_ensures_stays_nits_beside_ready_owner_slice_ensures() {
    require_z3!();
    let source = r#"
        #[repr(C)] type Pair { value: i64, label: string }
        extern "C" {
            func supported(value: Pair) -> Pair
                ensures: len(result.label) >= 0;
            func deferred(value: Pair) -> Pair
                ensures: unsupported_contract(result);
        }
        func caller(value: Pair) -> Pair {
            supported(value);
            deferred(value)
        }
    "#;
    let file =
        parse_memory_source(source, "typed-ffi-mixed-ready-deferred-ensures").expect("parse");
    let checked = crate::core::check_program(&file).expect("check mixed FFI declarations");
    assert!(matches!(
        &checked
            .ffi_declarations()
            .values()
            .find(|declaration| declaration.symbol == "supported")
            .unwrap()
            .ensures,
        crate::core::ir::ResolvedFfiContractRoot::Ready { .. }
    ));
    assert!(matches!(
        &checked
            .ffi_declarations()
            .values()
            .find(|declaration| declaration.symbol == "deferred")
            .unwrap()
            .ensures,
        crate::core::ir::ResolvedFfiContractRoot::Deferred { .. }
    ));
    let results = verify_ffi_checked_with_source_hash(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("one Ready proof and one explicit Deferred boundary");
    assert_eq!(results.len(), 2, "{results:?}");
    let supported = results
        .iter()
        .find(|result| result.func_name == "caller calls supported")
        .expect("supported callsite Ensures proof");
    assert_eq!(supported.status, VerifStatus::Proven, "{results:?}");
    let artifact = supported.artifact.as_ref().expect("MIR proof artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(
        artifact.obligation_scope,
        ProofArtifact::SCOPE_FFI_REQUIRES_ENSURES_V1
    );
    assert_eq!(
        artifact
            .mir_route_receipt
            .as_ref()
            .map(|receipt| receipt.profile.as_str()),
        Some("verifier-ffi-owner-slice-v1")
    );
    let deferred = results
        .iter()
        .find(|result| result.func_name == "caller -> extern deferred Ensures")
        .expect("deferred sibling keeps its declared NITS row");
    assert_eq!(deferred.status, VerifStatus::NotInTrustedSubset);
    assert!(deferred.artifact.is_none());
    assert_eq!(
        deferred
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    );
    let deferred_site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:caller" && site.callee == "deferred")
        .expect("Deferred exact call site");
    assert_eq!(
        deferred.diagnostic.as_ref().unwrap().span,
        deferred_site.origin.user_span()
    );
}

#[test]
fn typed_ffi_mixed_caller_owner_keeps_ready_ensures_at_nits_boundary() {
    require_z3!();
    let source = r#"
        #[repr(C)] type Pair { value: i64, label: string }
        extern "C" {
            func foreign(value: Pair) -> Pair
                ensures: len(result.label) >= 0;
        }
        func helper(value: i64) -> i64 { value }
        func caller(value: Pair) -> Pair {
            let unused = helper(value.value)
            foreign(value)
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-mixed-owner-helper-call").expect("parse");
    let checked = crate::core::check_program(&file).expect("check mixed caller owner");
    let results = verify_ffi_checked_with_source_hash(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("mixed owner remains explicit NITS");
    assert_eq!(results.len(), 1, "{results:?}");
    let ensures = &results[0];
    assert_eq!(
        ensures.func_name, "caller -> extern foreign Ensures",
        "owner outside the isolated top-level slice does not enter MIR proof routing"
    );
    assert_eq!(ensures.status, VerifStatus::NotInTrustedSubset);
    assert!(ensures.artifact.is_none());
    assert_eq!(
        ensures
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    );
    let call_site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:caller" && site.callee == "foreign")
        .expect("exact extern call site");
    assert_eq!(
        ensures.diagnostic.as_ref().unwrap().span,
        call_site.origin.user_span()
    );
}

#[test]
fn typed_ffi_ready_unsupported_record_ensures_preserves_requires_proof() {
    require_z3!();
    let source = r#"
        #[repr(C)] type Pair { value: i64, label: string }
        extern "C" {
            func foreign(value: Pair) -> Pair
                requires: value.value >= 0
                ensures: char_at(result.label, 0) == "x";
        }
        func caller(value: Pair) -> Pair {
            requires: value.value >= 0
            foreign(value)
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-unsupported-ready-record-ensures")
        .expect("parse typed but unsupported aggregate Ensures");
    let checked = crate::core::check_program(&file).expect("Checker materializes Ready roots");
    let declaration = checked
        .ffi_declarations()
        .values()
        .find(|declaration| declaration.symbol == "foreign")
        .expect("Checker FFI declaration");
    assert!(matches!(
        &declaration.ensures,
        crate::core::ir::ResolvedFfiContractRoot::Ready { .. }
    ));

    let results = verify_ffi_checked_with_source_hash(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("unsupported Ready Ensures stays a call-local NITS outcome");
    assert_eq!(results.len(), 1, "one result per call site: {results:?}");
    let result = &results[0];
    assert_eq!(result.func_name, "caller calls foreign");
    assert_eq!(result.status, VerifStatus::NotInTrustedSubset, "{result:?}");
    assert!(
        result.artifact.is_none(),
        "unsupported Ensures has no proof artifact"
    );
    assert!(
        result.message.contains("extern requires contract proven"),
        "the independent Requires proof must survive: {result:?}"
    );
    assert!(
        result
            .message
            .contains("extern ensures contract is outside the trusted subset"),
        "the selected but unsupported Ensures remains explicit: {result:?}"
    );
    assert_eq!(
        result
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    );
    let call_site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:caller" && site.callee == "foreign")
        .expect("exact foreign call site");
    assert_eq!(
        result.diagnostic.as_ref().unwrap().span,
        call_site.origin.user_span()
    );
    let dual_results = verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("default dual entry keeps unsupported Ready Ensures as owner-scoped NITS");
    assert_eq!(dual_results.len(), 1, "{dual_results:?}");
    let dual_result = &dual_results[0];
    assert_eq!(dual_result.func_name, "caller calls foreign");
    assert_eq!(dual_result.status, VerifStatus::NotInTrustedSubset);
    assert!(dual_result.artifact.is_none());
    assert!(dual_result
        .message
        .contains("extern requires contract proven"));
    assert!(dual_result
        .message
        .contains("extern ensures contract is outside the trusted subset"));
    assert_eq!(
        dual_result.diagnostic.as_ref().unwrap().span,
        call_site.origin.user_span()
    );
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
        let results =
            verify_ffi_checked(&checked).expect("all record field evaluation obligations");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, status, "{source}: {results:?}");
        assert_eq!(
            results[0].artifact.as_ref().unwrap().engine,
            ProofArtifact::ENGINE_MIR
        );
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
        let results =
            verify_ffi_checked(&checked).expect("rvalue field must keep constructor traps");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, status, "{source}: {results:?}");
        assert_eq!(
            results[0].artifact.as_ref().unwrap().engine,
            ProofArtifact::ENGINE_MIR
        );
    }
}

#[test]
fn typed_ffi_aggregate_argument_attribution_keeps_unowned_traps_caller_wide() {
    require_z3!();
    let source = r#"
        #[repr(C)] type Pair { value: i32, noise: i32 }
        extern "C" { func foreign(value: Pair) -> Pair requires: value.value >= 0; }
        func caller(n: i32) -> Pair {
            let unrelated = n % -1
            foreign(Pair { value: 1, noise: unrelated - unrelated })
        }
    "#;
    let file = parse_memory_source(source, "typed-ffi-record-unowned-trap").expect("parse");
    let checked = crate::core::check_program(&file).expect("check unowned trap source");
    let error = verify_ffi_checked(&checked)
        .expect_err("an unrelated caller trap must remain outside the argument receipt");
    assert!(error.contains("canonical MIR FFI verifier encountered reachable trap 'E0802'"));
    assert!(error.contains("function:caller"));
}

#[test]
fn typed_ffi_caller_if_and_short_circuit_paths_constrain_the_call() {
    require_z3!();
    let source = r#"
        extern "C" { func foreign(value: i32) -> i32 requires: value >= 0; }
        func if_positive(value: i32) -> i32 {
            if value >= 0 { foreign(value) } else { 0 }
        }
        func if_negative(value: i32) -> i32 {
            if value < 0 { foreign(value) } else { 0 }
        }
        func short_circuit_positive(value: i32) -> i32 {
            if value == 0 || (value > 0 && foreign(value) >= 0) { 1 } else { 0 }
        }
        func while_positive(value: i32) -> i32 {
            while value >= 0 { foreign(value); break; }
            0
        }
        func while_negative(value: i32) -> i32 {
            while value < 0 { foreign(value); break; }
            0
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-caller-paths").expect("parse");
    let checked = crate::core::check_program(&file).expect("check path-sensitive calls");
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
            ProofArtifact::ENGINE_MIR,
            "supported caller branch facts must be consumed by the typed verifier"
        );
    }
}

#[test]
fn typed_ffi_caller_literal_match_patterns_constrain_the_call() {
    require_z3!();
    let source = r#"
        extern "C" { func positive(value: i32) -> i32 requires: value > 0; }
        func match_positive(value: i32) -> i32 {
            match value {
                1 => positive(value),
                _ => 0
            }
        }
        func match_zero(value: i32) -> i32 {
            match value {
                0 => positive(value),
                _ => 0
            }
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-match-literal-paths").expect("parse");
    let checked = crate::core::check_program(&file).expect("check literal match paths");
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
        assert_eq!(
            result.artifact.as_ref().unwrap().engine,
            ProofArtifact::ENGINE_MIR
        );
    }
}

#[test]
fn typed_ffi_caller_singleton_iteration_binds_the_checked_element() {
    require_z3!();
    let source = r#"
        extern "C" { func positive(value: i32) -> i32 requires: value > 0; }
        func one_positive(value: i32) -> i32 {
            requires: value == 0
            for item in [value + 1] { positive(item); }
            0
        }
        func one_zero(value: i32) -> i32 {
            requires: value == 0
            for item in [value] { positive(item); }
            0
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-singleton-iteration").expect("parse");
    let checked = crate::core::check_program(&file).expect("check singleton iteration");
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
        assert_eq!(
            result.artifact.as_ref().unwrap().engine,
            ProofArtifact::ENGINE_MIR
        );
    }
}

#[test]
fn verifier_only_main_singleton_for_slice_cannot_enter_execution_consumers() {
    require_z3!();
    let source = r#"
        extern "C" { func positive(value: i32) -> i32 requires: value > 0; }
        func main() -> i64 {
            for item in [1] { positive(item); }
            0
        }
    "#;
    let file = parse_memory_source(source, "verifier-only-main-singleton-for").expect("parse");
    let checked = crate::core::check_program(&file).expect("check singleton For main");

    let checked_results = verify_ffi_checked(&checked).expect("checked FFI verifier route");
    assert_eq!(checked_results.len(), 1, "main's FFI call is retained");
    assert_eq!(
        checked_results[0].status,
        VerifStatus::Proven,
        "{checked_results:?}"
    );
    assert_eq!(
        checked_results[0].artifact.as_ref().unwrap().engine,
        ProofArtifact::ENGINE_MIR
    );

    let owner = crate::core::NodeId("function:main".into());
    let program =
        crate::core::mir::reference::MirProgram::from_checked_program_ffi_caller(&checked, &owner)
            .expect("isolated verifier-only main slice");
    assert_eq!(
        program.purpose(),
        crate::core::mir::reference::MirProgramPurpose::VerifierOnly
    );
    let receipt = program.route_receipt("verifier-ffi-owner-slice-v1");
    let direct_results =
        verify_ffi_mir_requires_with_route_receipt(&program, &receipt, String::new())
            .expect("symbolic FFI verifier consumes verifier-only singleton-For MIR");
    assert_eq!(direct_results.len(), 1);
    assert_eq!(direct_results[0].status, VerifStatus::Proven);
    assert_eq!(
        direct_results[0].artifact.as_ref().unwrap().engine,
        ProofArtifact::ENGINE_MIR
    );

    let reference_error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&owner, &[])
        .expect_err("reference execution must reject verifier-only MIR");
    assert!(reference_error.message.contains("verifier-only MIR"));

    let bytecode_errors = crate::interp::bytecode::compile_mir_program(&program)
        .expect_err("bytecode compilation must reject verifier-only MIR");
    assert!(bytecode_errors
        .iter()
        .any(|error| error.message.contains("verifier-only MIR")));

    let native_validation_errors = crate::codegen::mir::validate_mir_native(&program)
        .expect_err("native validation must reject verifier-only MIR");
    assert!(native_validation_errors
        .iter()
        .any(|error| error.message.contains("verifier-only MIR")));

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "verifier_only_mir");
    let native_compile_errors = generator
        .compile_mir_native(&program)
        .expect_err("native compilation must reject verifier-only MIR");
    assert!(native_compile_errors
        .iter()
        .any(|error| error.message.contains("verifier-only MIR")));
}

#[test]
fn typed_ffi_while_exit_paths_are_exact() {
    require_z3!();
    let source = r#"
        extern "C" {
            func positive(value: i32) -> i32 requires: value > 0;
            func greater_than_one(value: i32) -> i32 requires: value > 1;
        }
        func while_continue_positive(value: i32) -> i32 {
            while value >= 0 {
                if value == 0 { continue; }
                positive(value);
                break;
            }
            0
        }
        func while_break_positive(value: i32) -> i32 {
            while value >= 0 {
                if value == 0 { break; }
                positive(value);
                break;
            }
            0
        }
        func while_break_negative(value: i32) -> i32 {
            while value >= 0 {
                if value <= 0 { break; }
                greater_than_one(value);
                break;
            }
            0
        }
        func main() -> i64 { 0 }
    "#;
    let file = parse_memory_source(source, "typed-ffi-loop-exit-paths").expect("parse");
    let checked = crate::core::check_program(&file).expect("check loop exit paths");
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
        assert_eq!(
            result.artifact.as_ref().unwrap().engine,
            ProofArtifact::ENGINE_MIR
        );
    }
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
            ProofArtifact::ENGINE_MIR,
            "{caller}: binary64 leaf must be verified from its typed descriptor"
        );
    }
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
    let results = verify_ffi_checked(&checked).expect("typed IEEE NaN comparison");
    assert_eq!(results.len(), 1, "single FFI call site: {results:?}");
    assert_eq!(
        results[0].status,
        VerifStatus::Disproven,
        "the caller Requires admits NaN, for which ordered >= is false: {results:?}"
    );
    assert_eq!(
        results[0].artifact.as_ref().unwrap().engine,
        ProofArtifact::ENGINE_MIR
    );
}

#[test]
fn typed_ffi_nested_contract_context_is_rejected_by_the_checker() {
    require_z3!();
    let source = r#"
        extern "C" { func foreign(s: string, value: i64) -> i64 requires: value >= 0; }
        func caller(value: i64) -> i64 {
            requires: value >= 0
            value + {
                ensures: false
                0
            }
        }
        func main() -> i32 { foreign("ok", 0) }
    "#;
    let file = parse_memory_source(source, "typed-ffi-nested-context-obligation").expect("parse");
    let diagnostics = crate::core::check_program(&file)
        .expect_err("nested Ensures has no scoped language semantics");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0503)
                && diagnostic.message.contains("ensures")
                && diagnostic
                    .message
                    .contains("top-level callable-body contract")
        }),
        "the FFI verifier must never accept a source root the language checker rejects: {diagnostics:?}"
    );
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

#[test]
fn actor_no_z3_results_keep_infrastructure_and_owner_scoped_coverage_separate() {
    let actor_prefix = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/real_world/actor_nested_func_capture.mimi"
    ))
    .split("\nfunc main() -> i32 {")
    .next()
    .expect("original Actor fixture has its main entry");
    let source = format!(
        r#"{actor_prefix}
func accepted_owner(value: i64) -> i64 {{
    requires: value >= 0
    ensures: result == value
    value
}}
func deferred_owner(value: i64) -> i64 {{
    requires: value >= 0
    ensures: result == value
    eprintln("diagnostic")
    value
}}
extern "C" {{
    func actor_ffi_probe(value: i64) -> i64
        requires: value > 0
        ensures: result == value;
}}
func actor_ffi_caller(value: i64) -> i64 {{ actor_ffi_probe(value) }}
func main() -> i32 {{
    let counter = Counter.spawn()
    counter.bump(5)
    0
}}
"#
    );
    let file = parse_memory_source(&source, "actor-no-z3-proof-owners").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    assert!(super::is_actor_proof_compatibility(&checked));

    let results = super::actor_compatibility_no_z3_results(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    );
    let accepted = results
        .iter()
        .find(|result| result.func_name == "accepted_owner")
        .expect("admitted scalar owner remains visible");
    assert_eq!(accepted.status, VerifStatus::InfrastructureError);
    assert_eq!(accepted.message, "Z3 solver not available");
    assert_eq!(accepted.constraint_count, 0);
    assert!(accepted.artifact.is_none());

    let deferred = results
        .iter()
        .find(|result| result.func_name == "deferred_owner")
        .expect("unsupported proof owner remains visible");
    assert_eq!(deferred.status, VerifStatus::NotInTrustedSubset);
    assert_eq!(deferred.constraint_count, 0);
    assert!(deferred.artifact.is_none());
    assert_eq!(
        deferred
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    );
    assert!(deferred
        .diagnostic
        .as_ref()
        .is_some_and(|diagnostic| diagnostic.span != crate::span::Span::UNKNOWN));

    let ffi_requires = results
        .iter()
        .find(|result| {
            result.func_name.contains("actor_ffi_probe")
                && result.status == VerifStatus::InfrastructureError
        })
        .unwrap_or_else(|| {
            panic!("typed foreign Requires remains an infrastructure result: {results:#?}")
        });
    assert_eq!(ffi_requires.message, "Z3 solver not available");
    assert_eq!(ffi_requires.constraint_count, 0);
    assert!(ffi_requires.artifact.is_none());
    assert!(ffi_requires.diagnostic.is_none());
    let ffi_ensures = results
        .iter()
        .find(|result| result.func_name == "actor_ffi_caller -> extern actor_ffi_probe Ensures")
        .expect("foreign Ensures remains a separate result");
    assert_eq!(ffi_ensures.status, VerifStatus::NotInTrustedSubset);
    assert!(ffi_ensures.artifact.is_none());
    assert_eq!(ffi_ensures.constraint_count, 0);
    assert_eq!(
        ffi_ensures
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    );
    let ffi_call = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == "function:actor_ffi_caller" && site.callee == "actor_ffi_probe")
        .expect("typed foreign call site remains checker-owned");
    assert_eq!(
        ffi_ensures.diagnostic.as_ref().unwrap().span,
        ffi_call.origin.user_span()
    );
    assert!(!results
        .iter()
        .any(|result| result.status == VerifStatus::Proven));
}

#[test]
fn actor_compatibility_ffi_proves_requires_and_defers_ready_ensures() {
    if !super::is_z3_available() {
        eprintln!("ACTOR_FFI_Z3_POLICY z3_available=false outcome=skipped");
        return;
    }
    eprintln!("ACTOR_FFI_Z3_POLICY z3_available=true outcome=executed");

    let actor_source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/real_world/actor_nested_func_capture.mimi"
    ));
    let source = format!(
        r#"{actor_source}
extern "C" {{
    func actor_ffi_probe(value: i64) -> i64
        requires: value > 0
        ensures: result == value;
    func actor_ffi_unused(value: i64) -> i64
        ensures: result == value;
}}
func actor_ffi_positive(value: i64) -> i64 {{
    requires: value > 0
    actor_ffi_probe(value)
}}
func actor_ffi_negative() -> i64 {{
    actor_ffi_probe(0 as i64)
}}
"#
    );
    let file = parse_memory_source(&source, "actor-ffi-z3-policy").expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    assert!(super::is_actor_proof_compatibility(&checked));
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let results = super::verify_ffi_checked_with_source_hash(&checked, source_hash)
        .expect("Actor compatibility FFI caller proofs");

    let positive_requires = results
        .iter()
        .find(|result| result.func_name == "actor_ffi_positive calls actor_ffi_probe")
        .expect("positive call-site Requires result");
    assert_eq!(
        positive_requires.status,
        VerifStatus::Proven,
        "{positive_requires:?}"
    );
    assert!(
        positive_requires.artifact.is_some(),
        "{positive_requires:?}"
    );

    let negative_requires = results
        .iter()
        .find(|result| result.func_name == "actor_ffi_negative calls actor_ffi_probe")
        .expect("negative call-site Requires result");
    assert_eq!(
        negative_requires.status,
        VerifStatus::Disproven,
        "{negative_requires:?}"
    );

    for caller in ["actor_ffi_positive", "actor_ffi_negative"] {
        let ensures = results
            .iter()
            .find(|result| {
                result.func_name == format!("{caller} -> extern actor_ffi_probe Ensures")
            })
            .unwrap_or_else(|| panic!("{caller}: foreign Ensures result missing: {results:#?}"));
        assert_eq!(
            ensures.status,
            VerifStatus::NotInTrustedSubset,
            "{ensures:?}"
        );
        assert_eq!(ensures.constraint_count, 0, "{ensures:?}");
        assert!(ensures.artifact.is_none(), "{ensures:?}");
        assert_eq!(
            ensures
                .diagnostic
                .as_ref()
                .and_then(|diagnostic| diagnostic.code.as_deref()),
            Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
            "{ensures:?}"
        );
    }
    assert!(
        results
            .iter()
            .all(|result| !result.func_name.contains("actor_ffi_unused")),
        "uncalled extern declaration must not manufacture obligations: {results:#?}"
    );
}
