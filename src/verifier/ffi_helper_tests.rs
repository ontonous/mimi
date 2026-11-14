use super::*;

fn check_source(source: &str) -> crate::core::CheckedProgram {
    let file = super::parse_memory_source(source, "verifier-ffi-pure-helper").expect("parse");
    crate::core::check_program(&file).expect("check")
}

fn skip_without_z3() -> bool {
    if super::is_z3_available() {
        false
    } else {
        eprintln!("MIMI_FFI_REQUIRES_PURE_HELPER z3_available=false outcome=skipped");
        true
    }
}

#[test]
fn ffi_requires_accepts_one_private_pure_helper_and_keeps_verifier_only_receipt_closed() {
    if skip_without_z3() {
        return;
    }
    let source = r#"
        extern "C" {
            func host_i32(value: i32) -> i32 requires: value > 0;
        }
        func positive(value: i32) -> bool { value > 0 }
        func caller(value: i32) -> i32 {
            requires: positive(value)
            host_i32(value)
        }
    "#;
    let checked = check_source(source);
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let results = super::verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
        .expect("verifier-only pure helper FFI proof");
    let result = results
        .iter()
        .find(|result| result.func_name == "function:caller")
        .expect("one FFI caller result");
    assert_eq!(result.status, VerifStatus::Proven, "{result:?}");
    let artifact = result.artifact.as_ref().expect("proof artifact");
    assert_eq!(artifact.engine, ProofArtifact::ENGINE_MIR);
    assert_eq!(artifact.source_hash, source_hash);

    let caller_owner = crate::core::NodeId("function:caller".into());
    let helper_owner = crate::core::NodeId("function:positive".into());
    let helper_site = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| {
            site.owner == caller_owner.0
                && site.kind == crate::core::ResolvedCallKind::Function
                && site.resolved_callee
                    == Some(crate::core::ir::ResolvedCallee::Function(
                        helper_owner.clone(),
                    ))
        })
        .expect("one typed Requires helper callsite");
    let mir = crate::core::mir::reference::MirProgram::from_checked_program_ffi_caller_with_helper(
        &checked,
        &caller_owner,
        &helper_owner,
        &helper_site.node_id,
    )
    .expect("verifier-only caller and complete helper MIR graph");
    assert_eq!(
        mir.purpose(),
        crate::core::mir::reference::MirProgramPurpose::VerifierOnly
    );
    assert_eq!(
        mir.functions().len(),
        2,
        "caller and helper are both materialized"
    );
    assert!(mir.functions().contains_key(&caller_owner));
    assert!(mir.functions().contains_key(&helper_owner));
    mir.validate_proof_source_census()
        .expect("complete checked callable graph census");
    mir.validate_ffi_source_census()
        .expect("FFI and helper callsite source census");

    let caller = mir.functions().get(&caller_owner).expect("caller MIR");
    let contract = caller
        .contracts
        .iter()
        .find(|contract| contract.kind == crate::core::mir::MirContractKind::Requires)
        .expect("caller Requires MIR");
    let helper_receipt = contract
        .ffi_helper_call
        .as_ref()
        .expect("helper call receipt is part of the canonical contract");
    assert_eq!(helper_receipt.call_node, helper_site.node_id);
    assert_eq!(helper_receipt.helper, helper_owner);
    assert!(contract.canonical_text().contains(&helper_site.node_id.0));
    assert!(contract.canonical_text().contains(&helper_owner.0));

    let owner_census = mir
        .proof_source_census()
        .owner(&caller_owner)
        .expect("caller source owner census");
    let source_call = owner_census
        .calls()
        .iter()
        .find(|call| call.call_node() == &helper_site.node_id)
        .expect("exact checked Requires call identity");
    assert_eq!(
        source_call.origin(),
        &crate::core::mir::MirProofSourceCallOrigin::ProofRoot(contract.id.clone())
    );
    assert_eq!(
        source_call.callee(),
        &crate::core::ir::ResolvedCallee::Function(helper_owner.clone())
    );
    let helper_census = mir
        .proof_source_census()
        .checked_callable(&helper_owner)
        .expect("helper in complete checked source inventory");
    assert!(helper_census.materialized_owners().contains(&helper_owner));

    let receipt = mir.route_receipt("verifier-ffi-owner-slice-v1");
    receipt
        .validate_against_program(&mir)
        .expect("receipt binds the caller, helper, census, and canonical contract");
    let bytecode_errors =
        crate::interp::bytecode::compile_mir_program_with_route_receipt(&mir, &receipt)
            .expect_err("verifier-only helper graph cannot enter bytecode");
    assert!(bytecode_errors
        .iter()
        .any(|error| error.message.contains("verifier-only MIR")));
    let native_errors = crate::codegen::mir::validate_mir_native(&mir)
        .expect_err("verifier-only helper graph cannot enter native validation");
    assert!(native_errors
        .iter()
        .any(|error| error.message.contains("verifier-only MIR")));
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "ffi_helper_verifier_only");
    let native_compile_errors = generator
        .compile_mir_native_with_route_receipt(&mir, &receipt)
        .expect_err("verifier-only helper graph cannot enter native compilation");
    assert!(native_compile_errors
        .iter()
        .any(|error| error.message.contains("verifier-only MIR")));
}

#[test]
fn ffi_requires_pure_helper_can_disprove_a_weaker_caller_assumption() {
    if skip_without_z3() {
        return;
    }
    let source = r#"
        extern "C" {
            func host_i32(value: i32) -> i32 requires: value > 1;
        }
        func positive(value: i32) -> bool { value > 0 }
        func caller(value: i32) -> i32 {
            requires: positive(value)
            host_i32(value)
        }
    "#;
    let checked = check_source(source);
    let results = super::verify_ffi_checked_with_source_hash(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("pure helper counterexample proof");
    let result = results
        .iter()
        .find(|result| result.func_name == "function:caller")
        .expect("one FFI caller result");
    assert_eq!(result.status, VerifStatus::Disproven, "{result:?}");
    assert!(result.artifact.is_some(), "counterexample is definitive");
}

#[test]
fn ffi_requires_helper_admission_fails_closed_for_recursion_effects_generics_and_body_calls() {
    let cases = [
        (
            "recursive helper",
            r#"
                extern "C" { func host_i32(value: i32) -> i32 requires: value > 0; }
                func predicate(value: i32) -> bool { predicate(value) }
                func caller(value: i32) -> i32 {
                    requires: predicate(value)
                    host_i32(value)
                }
            "#,
        ),
        (
            "effectful helper",
            r#"
                extern "C" { func host_i32(value: i32) -> i32 requires: value > 0; }
                func predicate(value: i32) -> bool { println(value); value > 0 }
                func caller(value: i32) -> i32 {
                    requires: predicate(value)
                    host_i32(value)
                }
            "#,
        ),
        (
            "generic helper",
            r#"
                extern "C" { func host_i32(value: i32) -> i32 requires: value > 0; }
                func predicate<T>(value: T) -> bool { true }
                func caller(value: i32) -> i32 {
                    requires: predicate(value)
                    host_i32(value)
                }
            "#,
        ),
        (
            "body-context helper call",
            r#"
                extern "C" { func host_i32(value: i32) -> i32 requires: value > 0; }
                func predicate(value: i32) -> bool { value > 0 }
                func caller(value: i32) -> i32 {
                    predicate(value);
                    host_i32(value)
                }
            "#,
        ),
    ];

    for (label, source) in cases {
        let checked = check_source(source);
        let results = super::verify_ffi_checked_with_source_hash(
            &checked,
            blake3::hash(source.as_bytes()).to_hex().to_string(),
        )
        .unwrap_or_else(|error| panic!("{label}: checked FFI adapter failed: {error}"));
        let result = results
            .iter()
            .find(|result| result.func_name == "function:caller")
            .unwrap_or_else(|| panic!("{label}: no owner-scoped result: {results:?}"));
        assert_eq!(
            result.status,
            VerifStatus::NotInTrustedSubset,
            "{label}: {result:?}"
        );
        assert!(result.artifact.is_none(), "{label}: {result:?}");
    }
}
