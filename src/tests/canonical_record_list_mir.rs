//! Same-MIR differential coverage for the accepted flat Record and scalar
//! List profiles. These tests join checker admission, reference execution,
//! AST-free bytecode, native codegen, MIR verification, and direct CheckedProgram
//! no-legacy tripwires without widening either profile.

use super::*;
use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
use crate::core::mir::{
    CanonicalMirRouteProfile, FlatCopyRecordAdmission, ScalarCollectionAdmission,
};
use crate::core::NodeId;
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};
use crate::interp::Value;
use crate::verifier::VerifStatus;

const RECORD_SOURCE: &str = include_str!("../../tests/fixtures/mir_native_record_copy.mimi");
const LIST_SOURCE: &str =
    include_str!("../../tests/fixtures/mir_native_generic_list_construct.mimi");

fn checked_and_materialized(
    source: &str,
    label: &str,
    profile: CanonicalMirRouteProfile,
) -> (crate::core::CheckedProgram, MirProgram) {
    let file = crate::tests::parse_prod(source);
    let checked = crate::core::check_program(&file)
        .unwrap_or_else(|errors| panic!("{label} checker: {errors:?}"));
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
    match profile {
        CanonicalMirRouteProfile::FlatCopyRecord => assert_eq!(
            admission.record,
            FlatCopyRecordAdmission::CompleteCoverage,
            "{label} must have complete flat Copy-record admission"
        ),
        CanonicalMirRouteProfile::ScalarCollection => assert_eq!(
            admission.collection,
            ScalarCollectionAdmission::CompleteCoverage,
            "{label} must have complete scalar-collection admission"
        ),
        other => panic!("{label} uses unexpected profile {other:?}"),
    }
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .unwrap_or_else(|error| panic!("{label} route: {error}"));
    assert!(
        profile.is_materialized(&route),
        "{label} route omitted its {profile:?} operation receipt"
    );
    (checked, route.program)
}

fn same_mir_three_consumer_and_verifier_case(
    source: &str,
    label: &str,
    profile: CanonicalMirRouteProfile,
    expected: i64,
    contract_expected: bool,
) {
    let (checked, mir) = checked_and_materialized(source, label, profile);
    let digest = mir.canonical_digest();

    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error}"));
    assert_eq!(
        reference.value,
        MirRuntimeValue::Int(expected),
        "{label} value"
    );
    assert_eq!(reference.output, "", "{label} reference stdout");

    let bytecode = compile_mir_program(&mir)
        .unwrap_or_else(|errors| panic!("{label} bytecode compile: {errors:?}"));
    assert!(bytecode.ast.is_none(), "{label} bytecode must be AST-free");
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value()
            .unwrap_or_else(|error| panic!("{label} bytecode: {error}")),
        Value::Int(expected),
        "{label} bytecode value"
    );
    assert_eq!(vm.stdout(), "", "{label} bytecode stdout");

    assert!(
        can_link(),
        "{label} requires the configured host linker for native parity"
    );
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, label);
    generator
        .compile_mir_native(&mir)
        .unwrap_or_else(|errors| panic!("{label} native compile: {errors:?}"));
    generator
        .module
        .verify()
        .unwrap_or_else(|error| panic!("{label} native module verification: {error}"));
    let native = link_and_observe_canonical_mir(&generator)
        .unwrap_or_else(|error| panic!("{label} native run: {error}"));
    assert_eq!(
        native.exit_code,
        Some(expected as i32),
        "{label} native value"
    );
    assert_eq!(native.stdout, "", "{label} native stdout");
    assert_eq!(native.stderr, "", "{label} native stderr");

    let receipt = mir.route_receipt("verifier-mir-v1");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let proofs =
        crate::verifier::verify_mir_with_route_receipt(&mir, &receipt, source_hash.clone())
            .unwrap_or_else(|error| panic!("{label} same-MIR verifier: {error}"));
    if contract_expected {
        let main = proofs
            .iter()
            .find(|proof| proof.func_name.contains("main"))
            .unwrap_or_else(|| panic!("{label} verifier omitted main contract: {proofs:?}"));
        assert_eq!(
            main.status,
            VerifStatus::Proven,
            "{label}: {}",
            main.message
        );
        assert_eq!(
            main.artifact
                .as_ref()
                .and_then(|artifact| artifact.mir_route_receipt.as_ref()),
            Some(&receipt),
            "{label} verifier artifact must retain the same MIR receipt"
        );
    } else {
        assert!(
            proofs.is_empty(),
            "{label} has no contract obligations: {proofs:?}"
        );
    }

    // These are the direct CheckedProgram entry points. The MIR consumers
    // above already used the same immutable graph; this separate tripwire
    // proves the convenience API does not re-enter the legacy AST owner.
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let checked_results = crate::verifier::verify_checked(&checked, source_hash.clone())
        .unwrap_or_else(|error| panic!("{label} verify_checked: {error}"));
    if contract_expected {
        assert!(
            checked_results.iter().any(|result| {
                result.func_name.contains("main") && result.status == VerifStatus::Proven
            }),
            "{label} checked verifier must prove main: {checked_results:?}"
        );
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    crate::verifier::verify_checked_dual(&checked, source_hash)
        .unwrap_or_else(|error| panic!("{label} verify_checked_dual: {error}"));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let checked_context = inkwell::context::Context::create();
    let mut checked_codegen = crate::codegen::CodeGenerator::new(&checked_context, label);
    checked_codegen
        .compile_checked(&checked)
        .unwrap_or_else(|errors| panic!("{label} direct compile_checked: {errors:?}"));
    checked_codegen
        .module
        .verify()
        .unwrap_or_else(|error| panic!("{label} direct checked LLVM module: {error}"));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    assert_eq!(mir.canonical_digest(), digest, "{label} consumer stability");
}

#[test]
fn flat_copy_record_uses_one_mir_across_reference_bytecode_native_and_verifier() {
    same_mir_three_consumer_and_verifier_case(
        RECORD_SOURCE,
        "canonical_record_copy_same_mir",
        CanonicalMirRouteProfile::FlatCopyRecord,
        42,
        false,
    );
}

#[test]
fn scalar_list_construct_uses_one_mir_across_reference_bytecode_native_and_verifier() {
    same_mir_three_consumer_and_verifier_case(
        LIST_SOURCE,
        "canonical_list_construct_same_mir",
        CanonicalMirRouteProfile::ScalarCollection,
        1,
        true,
    );
}
