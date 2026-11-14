use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use mimi::core::mir::{
    validate_ordinary_map_owned_any_v2_checked_get_route,
    validate_ordinary_map_owned_any_v2_island, CanonicalMirRouteProfile,
    OrdinaryMapOwnedAnyV2Admission, ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
    ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
};
use mimi::core::resolved::NodeId;

fn assert_checked_get_source_unpack_executes(
    source: &str,
    expected: bool,
    target: mimi::core::PrimitiveType,
) {
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("CheckedGet source fixture tokenizes");
    let file = mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("CheckedGet source fixture parses");
    let checked = mimi::core::check_program(&file).expect("CheckedGet source fixture typechecks");
    let route = mimi::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("the exact source unpack materializes as CheckedGet");
    assert_eq!(
        route.admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::CheckedGet
    );
    let receipt = route
        .ordinary_map_owned_any_v2_checked_get_route_receipt
        .as_ref()
        .expect("CheckedGet source unpack has an exact route receipt");
    let unpacks = route
        .program
        .ordinary_map_owned_any_v2_unpack_result_receipts()
        .expect("CheckedGet source unpack receipt is source-backed");
    assert_eq!(unpacks.len(), 1);
    assert!(route
        .program
        .type_catalog()
        .get(&unpacks[0].conversion_to)
        .is_some_and(|descriptor| {
            descriptor.kind == mimi::core::mir::types::MirTypeKind::Primitive(target)
        }));
    validate_ordinary_map_owned_any_v2_checked_get_route(&route.program)
        .expect("MIR route validates the Checker unpack and ownership receipts");

    let reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(receipt);
    let observed = reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference MIR executes the CheckedGet source unpack");
    assert_eq!(observed.value, MirRuntimeValue::Bool(expected));
    assert_eq!(observed.output, "");

    let bytecode =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&route.program, receipt)
            .expect("the exact CheckedGet receipt seals AST-free Bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = mimi::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value()
            .expect("Bytecode executes the CheckedGet source unpack"),
        mimi::interp::Value::Bool(expected)
    );
    assert_eq!(vm.take_stdout(), "");
}

#[test]
fn checked_get_i32_string_and_canonical_miss_unpack_execute_in_reference_and_bytecode() {
    assert_checked_get_source_unpack_executes(
        include_str!("fixtures/mir_ordinary_map_any_owner_v2_checked_get_hit_i32.mimi"),
        true,
        mimi::core::PrimitiveType::I32,
    );
    assert_checked_get_source_unpack_executes(
        include_str!("fixtures/mir_ordinary_map_any_owner_v2_checked_get_hit_string.mimi"),
        true,
        mimi::core::PrimitiveType::String,
    );
    assert_checked_get_source_unpack_executes(
        include_str!("fixtures/mir_ordinary_map_any_owner_v2_checked_get_miss_i32.mimi"),
        false,
        mimi::core::PrimitiveType::I32,
    );
}

#[test]
fn checked_get_settled_branch_tail_has_a_receipted_route_and_executes_in_reference_mir() {
    let source =
        include_str!("fixtures/mir_ordinary_map_any_owner_v2_checked_get_settled_branch_tail.mimi");
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("settled CheckedGet branch-tail fixture tokenizes");
    let file = mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("settled CheckedGet branch-tail fixture parses");
    let checked = mimi::core::check_program(&file)
        .expect("settled CheckedGet branch-tail fixture passes the Checker");

    let route = mimi::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("the Checker-receipted settled tail materializes as CheckedGet");
    assert_eq!(
        route.admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::CheckedGet
    );
    assert!(CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CheckedGet.is_admitted(route.admission));
    assert!(route.materialized_ordinary_map_owned_any_v2_checked_get_candidate);
    assert!(!route.materialized_ordinary_map_owned_any_v2_candidate);
    const {
        assert!(
            !ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the narrow branch-tail profile must leave global V2 readiness closed"
        );
    };

    let receipt = route
        .ordinary_map_owned_any_v2_checked_get_route_receipt
        .as_ref()
        .expect("CheckedGet route receipt binds this canonical MIR");
    assert_eq!(
        receipt.profile,
        ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE
    );
    assert_eq!(
        receipt,
        &route
            .program
            .route_receipt(ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE),
        "the route receipt is the canonical identity of the materialized program"
    );
    for function in route.program.functions().values() {
        function
            .validate()
            .expect("the receipt-backed MIR function validates its Checker flow proof");
    }
    validate_ordinary_map_owned_any_v2_checked_get_route(&route.program)
        .expect("the CheckedGet MIR receipt joins the exact Map result and unpack");
    validate_ordinary_map_owned_any_v2_island(&route.program)
        .expect("the whole program remains inside the closed Map/Any V2 island");

    let reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(receipt);
    let observation = reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference MIR executes the settled branch tail");
    assert_eq!(observation.value, MirRuntimeValue::Bool(true));
    assert_eq!(observation.output, "");

    let bytecode =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&route.program, receipt)
            .expect("the exact CheckedGet route receipt seals AST-free Bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = mimi::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value()
            .expect("the narrow CheckedGet route is admitted by the production VM"),
        mimi::interp::Value::Bool(true)
    );
    assert_eq!(vm.take_stdout(), "");

    let generic_receipt = route.program.route_receipt("run-canonical-v1");
    let generic_bytecode = mimi::interp::bytecode::compile_mir_program_with_route_receipt(
        &route.program,
        &generic_receipt,
    )
    .expect("a generic receipt remains available for diagnostic bytecode compilation");
    let mut generic_vm = mimi::interp::bytecode::BytecodeVM::new(generic_bytecode);
    let error = generic_vm
        .run_value()
        .expect_err("a generic route receipt cannot borrow CheckedGet production authority");
    assert!(
        error
            .message()
            .contains("diagnostic-only while V2 production readiness is disabled"),
        "{error}"
    );
    assert_eq!(generic_vm.take_stdout(), "");

    let mut forged_receipt = receipt.clone();
    forged_receipt.mir_digest.push('0');
    let error = mimi::interp::bytecode::compile_mir_program_with_route_receipt(
        &route.program,
        &forged_receipt,
    )
    .expect_err("a digest-forged CheckedGet receipt cannot seal Bytecode");
    assert!(
        format!("{error:?}").contains("route receipt rejected"),
        "{error:?}"
    );
}

#[test]
fn checked_get_bool_has_a_receipted_route_and_copies_the_selected_payload() {
    let source = include_str!("fixtures/mir_ordinary_map_any_owner_v2_checked_get_hit_bool.mimi");
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("Bool CheckedGet fixture tokenizes");
    let file = mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("Bool CheckedGet fixture parses");
    let checked =
        mimi::core::check_program(&file).expect("Bool CheckedGet fixture passes the Checker");
    let route = mimi::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("the exact Bool Set/Get/unpack receipt materializes as CheckedGet");
    assert_eq!(
        route.admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::CheckedGet
    );
    assert!(CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CheckedGet.is_admitted(route.admission));
    const {
        assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
    let receipt = route
        .ordinary_map_owned_any_v2_checked_get_route_receipt
        .as_ref()
        .expect("Bool CheckedGet requires an exact route receipt");
    validate_ordinary_map_owned_any_v2_checked_get_route(&route.program)
        .expect("Bool Set/Get/unpack MIR agrees with Checker ownership evidence");

    let reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(receipt);
    let observed = reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("Reference MIR clones and observes the selected Bool payload");
    assert_eq!(observed.value, MirRuntimeValue::Bool(true));
    assert_eq!(observed.output, "");

    let bytecode =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&route.program, receipt)
            .expect("the exact Bool CheckedGet receipt seals AST-free Bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = mimi::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value()
            .expect("Bytecode preserves the selected Bool payload"),
        mimi::interp::Value::Bool(true)
    );
    assert_eq!(vm.take_stdout(), "");

    let generic_receipt = route.program.route_receipt("run-canonical-v1");
    let generic_error = mimi::interp::bytecode::compile_mir_program_with_route_receipt(
        &route.program,
        &generic_receipt,
    )
    .expect_err("a generic route receipt cannot authorize Bool Set payload emission");
    assert!(
        format!("{generic_error:?}").contains("Bool payload requires the exact CheckedGet route"),
        "generic route must leave the CheckedGet Bool Set closed: {generic_error:?}"
    );

    let mut forged_receipt = receipt.clone();
    forged_receipt.mir_digest.push('0');
    let error = mimi::interp::bytecode::compile_mir_program_with_route_receipt(
        &route.program,
        &forged_receipt,
    )
    .expect_err("a digest-forged Bool CheckedGet receipt cannot seal Bytecode");
    assert!(
        format!("{error:?}").contains("route receipt rejected"),
        "{error:?}"
    );
}
