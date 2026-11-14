use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use mimi::core::mir::{
    materialize_canonical_mir_route,
    validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route,
    OrdinaryMapOwnedAnyV2Admission, ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
    ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE,
};
use mimi::core::resolved::NodeId;

#[test]
fn values_crosscall_string_caller_index1_executes_same_checker_mir_receipt_in_reference_and_bytecode(
) {
    let source = include_str!(
        "fixtures/mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1.mimi"
    );
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("Values cross-call Index1 fixture tokenizes");
    let file = mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("Values cross-call Index1 fixture parses");
    let checked = mimi::core::check_program(&file)
        .expect("Values cross-call Index1 fixture passes the Checker");

    let route = materialize_canonical_mir_route(&checked, None)
        .expect("Checker admits the exact Values cross-call Index1 MIR route");
    assert_eq!(
        route.admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::ValuesCrosscallStringCallerIndex1
    );
    const {
        assert!(
            !ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the narrow Index1 admission must leave broad V2 readiness closed"
        );
    };

    let receipt = route
        .ordinary_map_owned_any_v2_values_crosscall_string_caller_route_receipt
        .as_ref()
        .expect("Index1 route materialization carries its dedicated receipt");
    assert_eq!(
        receipt.profile,
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE
    );
    assert_eq!(
        receipt,
        &route
            .program
            .route_receipt(ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE),
        "the receipt identifies this exact in-memory MIR program"
    );
    let checked_receipt =
        validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
            &checked,
            &route.program,
        )
        .expect("Checker and MIR independently validate the exact Index1 profile");
    assert_eq!(&checked_receipt, receipt);

    let reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(receipt);
    let reference_observation = reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("Reference executes the same receipted Index1 MIR program");
    let MirRuntimeValue::Int(reference_result) = &reference_observation.value else {
        panic!("Reference main must return the fixture's i32 zero")
    };
    assert_eq!(*reference_result, 0);
    assert_eq!(reference_observation.output, "true\n");

    let bytecode =
        mimi::interp::bytecode::mir::compile_mir_program_with_values_crosscall_string_caller_index1_route(
            &checked,
            &route.program,
        )
        .expect("the same Checker/MIR pair compiles through the exact Index1 adapter");
    assert!(
        bytecode.ast.is_none(),
        "canonical bytecode must be AST-free"
    );
    let mut vm = mimi::interp::bytecode::BytecodeVM::new(bytecode);
    let mimi::interp::Value::Int(bytecode_result) = vm
        .run_value()
        .expect("Bytecode executes the exact Index1 caller route")
    else {
        panic!("Bytecode main must return the fixture's i32 zero")
    };
    assert_eq!(bytecode_result, *reference_result);
    assert_eq!(vm.take_stdout(), reference_observation.output);

    let mut forged_receipt = receipt.clone();
    forged_receipt.ownership_digest = "0".repeat(64);
    let forged_reference =
        MirReferenceInterpreter::new(&route.program).with_route_receipt(&forged_receipt);
    let error = forged_reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect_err("Reference rejects a forged Index1 ownership digest");
    assert!(
        error.message.contains("canonical route receipt rejected"),
        "forged receipt rejection should identify the receipt gate: {error}"
    );
    assert!(
        forged_reference.captured_output().is_empty(),
        "a forged route receipt must be rejected before user output"
    );
}
