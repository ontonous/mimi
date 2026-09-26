//! Same-graph execution evidence for the bounded scalar generic identity MIR
//! profile. The checker is run once and every consumer below receives the
//! same materialized `MirProgram` and its route receipt.

use super::*;
use crate::core::mir::reference::MirReferenceInterpreter;
use crate::core::NodeId;

#[test]
fn scalar_generic_identity_matches_reference_bytecode_native_and_verifier() {
    let source = "func pass<T>(value: T) -> T { value }\nfunc main() -> i32 { pass(41) }";
    let file = parse_prod(source);
    let checked = crate::core::check_program(&file).expect("checker");
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("checker-owned scalar identity route");
    assert_eq!(
        route.admission.scalar_generic_identity_i32,
        crate::core::mir::ScalarGenericIdentityAdmission::CompleteCoverage
    );
    assert!(route.materialized_scalar_generic_identity_i32_candidate);
    let mir = route.program;
    crate::core::mir::validate_scalar_generic_identity_island(&mir)
        .expect("whole-program profile gate");
    let receipt = mir.route_receipt(crate::core::mir::SCALAR_GENERIC_IDENTITY_I32_ISLAND);

    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(41)
    );

    let bytecode = crate::interp::bytecode::compile_mir_program_with_route_receipt(&mir, &receipt)
        .expect("AST-free bytecode consumer");
    let bytecode_value = crate::interp::bytecode::BytecodeVM::new(bytecode)
        .run_value()
        .expect("bytecode execution");
    assert!(matches!(bytecode_value, crate::interp::Value::Int(41)));

    crate::verifier::validate_mir_capabilities(&mir).expect("MIR verifier capability");
    let verified = crate::verifier::verify_mir_with_route_receipt(
        &mir,
        &receipt,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("no-contract MIR verifier pass");
    assert!(
        verified.is_empty(),
        "no source contracts means no proof results"
    );

    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "scalar_generic_identity");
    codegen
        .compile_mir_native_with_route_receipt(&mir, &receipt)
        .expect("native consumer using the same MIR and receipt");
    codegen
        .module
        .verify()
        .expect("native LLVM module verifies");
    let native = link_and_observe_canonical_mir(&codegen).expect("native process execution");
    assert_eq!(native.stdout, "");
    assert_eq!(native.stderr, "");
    assert_eq!(native.exit_code, Some(41));
}
