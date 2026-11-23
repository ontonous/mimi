use super::*;
use crate::core::ir::PrimitiveType;
use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
use crate::core::mir::types::MirOwnership;
use crate::core::mir::{
    classify_canonical_mir_route_admission, materialize_canonical_mir_route,
    ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND,
};
use crate::core::NodeId;
use crate::interp::bytecode::{compile_mir_program_with_route_receipt, BytecodeVM};
use crate::interp::Value;

const SOURCE: &str = include_str!(
    "../../../tests/fixtures/mir_ordinary_map_owned_get_string_detached_result_match.mimi"
);

fn checked(source: &str) -> crate::core::CheckedProgram {
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("fixture lexes");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("fixture parses");
    crate::core::check_program(&file).expect("fixture typechecks")
}

fn materialized_fixture() -> (
    crate::core::CheckedProgram,
    crate::core::mir::CanonicalMirRouteMaterialization,
) {
    let checked = checked(SOURCE);
    let admission = classify_canonical_mir_route_admission(&checked);
    assert!(admission.ordinary_map_owned_get_i32_string_v1_complete());
    const {
        assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
    let materialized = materialize_canonical_mir_route(&checked, None)
        .expect("exact detached String Result match materializes");
    assert!(materialized.materialized_ordinary_map_owned_get_i32_string_v1_candidate);
    (checked, materialized)
}

fn main_function(program: &MirProgram) -> &MirFunction {
    program
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("fixture main MIR")
}

#[test]
fn checker_cfg_mir_and_all_consumers_accept_the_detached_string_result_match() {
    let (checked, materialized) = materialized_fixture();
    let program = &materialized.program;
    let function = main_function(program);
    function
        .validate()
        .expect("MIR ownership and CFG receipts validate");

    let cfg = function
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1_cfg_receipt
        .as_ref()
        .expect("Checker-owned CFG receipt");
    assert_eq!(cfg.owner, function.owner);
    assert_eq!(
        cfg.profile_version,
        crate::core::OrdinaryMapOwnedGetI32StringV1CfgProfileVersion::ResultBranchV3
    );
    let [receipt] = cfg.string_match_receipts.as_slice() else {
        panic!("exactly one direct String-get Result match receipt is required");
    };
    assert_eq!(
        receipt.profile_version,
        crate::core::OrdinaryMapOwnedGetI32StringV1ProfileVersion::V1
    );
    assert!(cfg.attempts.iter().any(|attempt| {
        attempt.kind == crate::core::OrdinaryMapOwnedGetI32StringV1AttemptKind::GetString
            && attempt.point == receipt.operation
            && attempt.result_local.as_ref() == Some(&receipt.result_binding)
    }));
    assert!(cfg.attempts.iter().all(|attempt| {
        attempt.kind != crate::core::OrdinaryMapOwnedGetI32StringV1AttemptKind::ScopeDrop
    }));

    let actions = &function
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1;
    let created_roots = actions
        .iter()
        .filter(|action| {
            matches!(
                action.kind,
                crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::New
                    | crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::Clone
                    | crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::Set
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        created_roots.len(),
        3,
        "new, set, and one local Map clone are receipted"
    );
    for root in created_roots {
        assert!(
            actions
                .iter()
                .filter(|action| {
                    action.kind == crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::Drop
                        && action.root == root.root
                        && action.local == root.local
                })
                .count()
                >= 1,
            "every Checker-owned Map root has explicit path Drops"
        );
    }
    assert_eq!(
        actions
            .iter()
            .filter(
                |action| action.kind == crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::Drop
            )
            .count(),
        4
    );

    let string_ty = crate::core::ir::primitive_type_id(PrimitiveType::String);
    let i32_ty = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let result_ty = crate::core::ir::result_type_id(string_ty.clone(), i32_ty.clone());
    assert_eq!(
        function.values
            [&MirValueId::new(format!("local:{}", receipt.result_binding.0 .0)).unwrap()]
            .ty,
        result_ty,
        "the direct result local is Result<String, i32>"
    );
    let result_scrutinee = MirValueId::new(format!("expr:{}", receipt.scrutinee.0)).unwrap();
    let result_binding = MirValueId::new(format!("local:{}", receipt.result_binding.0 .0)).unwrap();
    let getter_result = function
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1
        .iter()
        .find(|action| {
            action.kind == crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::GetString
                && action.point == receipt.operation
        })
        .and_then(|action| action.result.as_ref())
        .expect("typed getter MIR result");
    assert_eq!(
        function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(&instruction.kind, MirInstructionKind::Move { result, source }
                    if result == &result_binding && source == getter_result)
            })
            .count(),
        1,
        "Checker result local is bound directly from map_get_string"
    );
    let switch = function
        .blocks
        .values()
        .find_map(|block| match &block.terminator {
            MirTerminator::SwitchMove { scrutinee, arms } if scrutinee == &result_scrutinee => {
                Some((scrutinee, arms))
            }
            _ => None,
        })
        .expect("the exact Result local is consumed by SwitchMove");
    assert_eq!(switch.0, &result_scrutinee);
    assert_eq!(switch.1.len(), 2);
    assert!(function.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(&instruction.kind, MirInstructionKind::Move { result, source }
                if result == &result_scrutinee && source == &result_binding)
        })
    }));
    let switch_tags = switch
        .1
        .iter()
        .filter_map(|arm| match &arm.case {
            MirSwitchCase::Variant(variant) => Some(variant.0.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        switch_tags,
        BTreeSet::from(["builtin:variant:Result::Err", "builtin:variant:Result::Ok",])
    );
    assert!(switch.1.iter().all(|arm| {
        arm.bindings.len() == 1
            && arm.arguments.is_empty()
            && match &arm.case {
                MirSwitchCase::Variant(variant)
                    if variant.0.as_str() == "builtin:variant:Result::Ok" =>
                {
                    arm.bindings[0].projection.ownership == MirOwnership::Move
                }
                MirSwitchCase::Variant(variant)
                    if variant.0.as_str() == "builtin:variant:Result::Err" =>
                {
                    arm.bindings[0].projection.ownership == MirOwnership::Copy
                }
                _ => false,
            }
    }));
    assert!(function.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            instruction.id.as_str() == format!("inst:drop.0:{}", receipt.ok_drop.0)
                && matches!(&instruction.kind, MirInstructionKind::Drop { value }
                    if value == &MirValueId::new(format!("local:{}", receipt.ok_binding.0.0)).unwrap())
        })
    }));

    let route_receipt =
        crate::core::mir::validate_ordinary_map_owned_get_result_branch_v3_mir_route(
            &checked, program,
        )
        .expect("exact CheckedProgram/MIR owned-get ResultBranchV3 route validates");
    assert_eq!(
        route_receipt,
        program.route_receipt(ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND)
    );
    route_receipt
        .validate_against_program(program)
        .expect("exact typed owned-get route receipt covers the MIR graph");

    let reference = MirReferenceInterpreter::new(program)
        .with_route_receipt(&route_receipt)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference consumer executes the exact Result ownership receipt");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "snapshot survives\n");

    crate::codegen::mir::validate_mir_native(program)
        .expect("native preflight validates the same Checker/MIR receipt");
    crate::verifier::validate_mir_capabilities_with_ordinary_map_owned_get_result_branch_v3_route(
        &checked,
        program,
        &route_receipt,
    )
    .expect("verifier capability gate accepts the exact ResultBranchV3 route");
    crate::verifier::verify_mir_with_ordinary_map_owned_get_result_branch_v3_route(
        &checked,
        program,
        &route_receipt,
        "ordinary_map_owned_get_detached_string_match".into(),
    )
    .expect("symbolic verifier accepts the exact ResultBranchV3 route");

    let bytecode =
        crate::interp::bytecode::compile_mir_program_with_ordinary_map_owned_get_result_branch_v3_route(
            &checked, program,
        )
        .expect("AST-free bytecode consumer compiles the exact ResultBranchV3 route");
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(vm.run_value().expect("bytecode execution"), Value::Int(0));
    assert_eq!(vm.take_stdout(), "snapshot survives\n");
}

#[test]
fn tampered_switch_tag_and_missing_string_drop_fail_every_consumer() {
    let (_checked, materialized) = materialized_fixture();
    let program = materialized.program;
    let original = main_function(&program).clone();
    let cfg_receipt = original
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1_cfg_receipt
        .as_ref()
        .expect("Checker CFG receipt");
    let [match_receipt] = cfg_receipt.string_match_receipts.as_slice() else {
        panic!("String match receipt");
    };
    let match_receipt = match_receipt.clone();

    let mut wrong_tag = original.clone();
    let switch = wrong_tag
        .blocks
        .values_mut()
        .find_map(|block| match &mut block.terminator {
            MirTerminator::SwitchMove { scrutinee, arms }
                if scrutinee.0 == format!("expr:{}", match_receipt.scrutinee.0) =>
            {
                Some(arms)
            }
            _ => None,
        })
        .expect("Result SwitchMove");
    switch[0].case = MirSwitchCase::Variant(NodeId("builtin:variant:Result::Unexpected".into()));
    let tag_errors = wrong_tag
        .validate()
        .expect_err("MIR validator rejects an unreceipted Result tag");
    assert!(tag_errors
        .iter()
        .any(|error| error.message.contains("String match receipt")));
    let mut forged_tag_program = program.clone();
    forged_tag_program.replace_function_for_test_only(wrong_tag);
    let route_receipt = program.route_receipt(ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND);
    assert!(route_receipt
        .validate_against_program(&forged_tag_program)
        .is_err());
    assert!(crate::codegen::mir::validate_mir_native(&forged_tag_program).is_err());
    assert!(crate::verifier::validate_mir_capabilities(&forged_tag_program).is_err());
    assert!(compile_mir_program_with_route_receipt(
        &forged_tag_program,
        &program.route_receipt(ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND)
    )
    .is_err());
    assert!(MirReferenceInterpreter::new(&forged_tag_program)
        .execute(&NodeId("function:main".into()), &[])
        .is_err());

    let mut missing_drop = original;
    let drop_id = format!("inst:drop.0:{}", match_receipt.ok_drop.0);
    let mut removed = false;
    for block in missing_drop.blocks.values_mut() {
        if let Some(index) = block.instructions.iter().position(|instruction| {
            instruction.id.as_str() == drop_id
                && matches!(&instruction.kind, MirInstructionKind::Drop { .. })
        }) {
            block.instructions.remove(index);
            removed = true;
            break;
        }
    }
    assert!(removed, "fixture has the receipted Ok String Drop");
    let drop_errors = missing_drop
        .validate()
        .expect_err("MIR validator rejects a missing Ok String Drop");
    assert!(drop_errors
        .iter()
        .any(|error| error.message.contains("String match receipt")));
    let mut missing_drop_program = program;
    missing_drop_program.replace_function_for_test_only(missing_drop);
    assert!(route_receipt
        .validate_against_program(&missing_drop_program)
        .is_err());
    assert!(crate::codegen::mir::validate_mir_native(&missing_drop_program).is_err());
    assert!(crate::verifier::validate_mir_capabilities(&missing_drop_program).is_err());
    assert!(compile_mir_program_with_route_receipt(&missing_drop_program, &route_receipt).is_err());
    assert!(MirReferenceInterpreter::new(&missing_drop_program)
        .execute(&NodeId("function:main".into()), &[])
        .is_err());
}

#[test]
fn erased_string_match_receipt_fails_closed_at_every_consumer() {
    let (_checked, materialized) = materialized_fixture();
    let program = materialized.program;
    let route_receipt = program.route_receipt(ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND);
    let mut function = main_function(&program).clone();
    function
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1_cfg_receipt
        .as_mut()
        .expect("Checker CFG receipt")
        .string_match_receipts
        .clear();
    let errors = function
        .validate()
        .expect_err("direct GetString-to-SwitchMove requires its Checker match receipt");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("missing the Checker receipt for a direct GetString-to-SwitchMove")
    }));

    let mut erased_receipt_program = program.clone();
    erased_receipt_program.replace_function_for_test_only(function);
    assert!(route_receipt
        .validate_against_program(&erased_receipt_program)
        .is_err());
    assert!(crate::codegen::mir::validate_mir_native(&erased_receipt_program).is_err());
    assert!(crate::verifier::validate_mir_capabilities(&erased_receipt_program).is_err());
    assert!(
        compile_mir_program_with_route_receipt(&erased_receipt_program, &route_receipt).is_err()
    );
    assert!(MirReferenceInterpreter::new(&erased_receipt_program)
        .execute(&NodeId("function:main".into()), &[])
        .is_err());
}

#[test]
fn source_near_misses_do_not_acquire_the_string_match_receipt() {
    let near_misses = [
        SOURCE.replace(
            "                        Err(status) => { status },",
            "                        Err(_) => { 1 },",
        ),
        SOURCE
            .replace(
                "let snapshot = map_get_string(source_copy, \"payload\")",
                "let snapshot = map_get_string(source_copy, \"payload\")\n    let alias = snapshot",
            )
            .replace("match snapshot {", "match alias {"),
        SOURCE.replace("                            drop(text)\n", ""),
    ];
    for (index, source) in near_misses.iter().enumerate() {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("near miss still lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("near miss still parses");
        let checked = match crate::core::check_program(&file) {
            Ok(checked) => checked,
            Err(diagnostics) if index == 2 => {
                assert!(
                    !diagnostics.is_empty(),
                    "Checker rejection of missing String Drop has a diagnostic"
                );
                continue;
            }
            Err(diagnostics) => panic!(
                "near miss {index} should reach route admission, but Checker rejected it: {diagnostics:?}"
            ),
        };
        let admission = classify_canonical_mir_route_admission(&checked);
        assert!(
            !admission.ordinary_map_owned_get_i32_string_v1_complete(),
            "near miss {index} cannot complete the ordinary typed owned-get route"
        );
        let materialization = materialize_canonical_mir_route(&checked, None);
        match materialization {
            Err(_) => {}
            Ok(route) => assert!(
                !route.materialized_ordinary_map_owned_get_i32_string_v1_candidate,
                "near miss {index} must fail closed instead of acquiring V1 route authority"
            ),
        }
    }
}
