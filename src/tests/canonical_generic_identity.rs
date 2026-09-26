//! Same-graph execution evidence for the bounded scalar generic identity MIR
//! profile. The checker is run once and every consumer below receives the
//! same materialized `MirProgram` and its route receipt.

use super::*;
use crate::core::mir::reference::MirReferenceInterpreter;
use crate::core::NodeId;

#[test]
fn scalar_generic_identity_matches_reference_bytecode_native_and_verifier() {
    assert_scalar_generic_identity_differential(
        "func pass<T>(value: T) -> T { value }\nfunc main() -> i32 { pass(41) }",
        crate::core::PrimitiveType::I32,
        crate::core::mir::SCALAR_GENERIC_IDENTITY_I32_ISLAND,
        41,
    );
}

#[test]
fn scalar_generic_identity_i64_matches_reference_bytecode_native_and_verifier() {
    for value in [i32::MAX as i64 + 1, 2_147_483_690, i64::MAX] {
        let source = format!(
            "func pass<T>(value: T) -> T {{ value }}\nfunc main() -> i64 {{ pass({value}) }}"
        );
        assert_scalar_generic_identity_differential(
            &source,
            crate::core::PrimitiveType::I64,
            crate::core::mir::SCALAR_GENERIC_IDENTITY_I64_ISLAND,
            value,
        );
    }
}

fn assert_scalar_generic_identity_differential(
    source: &str,
    primitive: crate::core::PrimitiveType,
    island: &str,
    expected: i64,
) {
    let file = parse_prod(source);
    let checked = crate::core::check_program(&file).expect("checker");
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("checker-owned scalar identity route");
    let (admission, materialized, validate) = match primitive {
        crate::core::PrimitiveType::I32 => (
            route.admission.scalar_generic_identity_i32,
            route.materialized_scalar_generic_identity_i32_candidate,
            crate::core::mir::validate_scalar_generic_identity_island
                as fn(&crate::core::mir::reference::MirProgram) -> Result<(), Vec<String>>,
        ),
        crate::core::PrimitiveType::I64 => (
            route.admission.scalar_generic_identity_i64,
            route.materialized_scalar_generic_identity_i64_candidate,
            crate::core::mir::validate_scalar_generic_identity_i64_island
                as fn(&crate::core::mir::reference::MirProgram) -> Result<(), Vec<String>>,
        ),
        unsupported => panic!("test helper only covers i32/i64 identity, got {unsupported:?}"),
    };
    assert_eq!(
        admission,
        crate::core::mir::ScalarGenericIdentityAdmission::CompleteCoverage
    );
    assert!(materialized);
    let mir = route.program;
    validate(&mir).expect("whole-program profile gate");
    let receipt = mir.route_receipt(island);

    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(expected)
    );

    let bytecode = crate::interp::bytecode::compile_mir_program_with_route_receipt(&mir, &receipt)
        .expect("AST-free bytecode consumer");
    let bytecode_value = crate::interp::bytecode::BytecodeVM::new(bytecode)
        .run_value()
        .expect("bytecode execution");
    assert!(matches!(bytecode_value, crate::interp::Value::Int(value) if value == expected));

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
    assert_eq!(native.exit_code, Some((expected as u8) as i32));
}
