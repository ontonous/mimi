use super::*;
use crate::core::ir::{PrimitiveType, ResolvedType, ResolvedTypeTable};
use crate::core::mir::types::{MirGlueKind, MirOwnership};

fn type_id(table: &mut ResolvedTypeTable, ty: ResolvedType) -> ResolvedTypeId {
    table.intern_resolved(ty).expect("test type must intern")
}

fn checked_program(source: &str) -> crate::core::CheckedProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    crate::core::check_program(&file).expect("check")
}

#[test]
fn materializes_generic_option_predicate_with_a_specialized_variant_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_predicate.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option predicate must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantPredicate {
                    contract: crate::core::mir::types::MirVariantPredicateContract {
                        predicate: crate::core::mir::MirVariantPredicate::IsSome,
                        ..
                    }
                }
            )
        })
        .expect("generic Option predicate instance");
    let MirGenericInstanceContract::ScalarVariantPredicate { contract } = &instance.contract else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.variant_name, "Some");
    assert_eq!(contract.alternate_variant_name, "None");
    assert_eq!(contract.discriminant, 1);

    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized generic predicate target");
    let receipts = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::VariantPredicate {
                contract: Some(receipt),
                predicate: crate::core::mir::MirVariantPredicate::IsSome,
                ..
            } => Some(receipt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0], contract);
    let Some(option_desc) = program.type_catalog().get(&receipts[0].variant_ty) else {
        panic!("specialized Option TypeDesc is absent");
    };
    let crate::core::mir::types::MirLayout::Option { inner, .. } = &option_desc.layout else {
        panic!("generic predicate receipt must point at an Option TypeDesc");
    };
    assert_eq!(inner, &instance.arguments[0]);

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option predicate execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn rejects_generic_option_predicate_for_non_copy_payload_before_legacy() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_predicate_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("non-Copy generic Option predicate must fail closed");
    let message = error.to_string();
    assert!(
        message.contains("MIR lowering failed"),
        "unexpected rejection: {message}"
    );
}

#[test]
fn materializes_generic_result_predicate_with_a_specialized_variant_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_predicate.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result predicate must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantPredicate {
                    contract: crate::core::mir::types::MirVariantPredicateContract {
                        predicate: crate::core::mir::MirVariantPredicate::IsOk,
                        ..
                    }
                }
            )
        })
        .expect("generic Result predicate instance");
    let MirGenericInstanceContract::ScalarVariantPredicate { contract } = &instance.contract else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.nominal.as_str(), "builtin:type:Result");
    assert_eq!(contract.variant_name, "Ok");
    assert_eq!(contract.alternate_variant_name, "Err");
    assert_eq!(contract.discriminant, 0);
    let Some(result_desc) = program.type_catalog().get(&contract.variant_ty) else {
        panic!("specialized Result TypeDesc is absent");
    };
    assert!(matches!(
        result_desc.layout,
        crate::core::mir::types::MirLayout::Result { .. }
    ));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Result predicate execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn rejects_generic_result_predicate_for_non_copy_payload_before_legacy() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_predicate_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("non-Copy generic Result predicate must fail closed");
    let message = error.to_string();
    assert!(
        message.contains("MIR lowering failed"),
        "unexpected rejection: {message}"
    );
}

#[test]
fn materializes_generic_result_error_slot_predicate_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_error_slot.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Result<i32, T> predicate must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantPredicate {
                    contract: crate::core::mir::types::MirVariantPredicateContract {
                        predicate: crate::core::mir::MirVariantPredicate::IsErr,
                        ..
                    }
                }
            )
        })
        .expect("generic Result error-slot predicate instance");
    let MirGenericInstanceContract::ScalarVariantPredicate { contract } = &instance.contract else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.nominal.as_str(), "builtin:type:Result");
    assert_eq!(contract.variant_name, "Err");
    assert_eq!(contract.alternate_variant_name, "Ok");
    assert_eq!(contract.discriminant, 1);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Result<i32, T> predicate execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn lowers_option_string_unwrap_to_consuming_variant_projection() {
    let source = include_str!("../../../tests/fixtures/mir_native_option_string_unwrap.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Option<string>.unwrap must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:unwrap_owned".into()))
        .expect("unwrap_owned MIR function");
    let receipt = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::VariantProjectMove {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("unwrap must carry a consuming variant projection receipt");
    assert_eq!(receipt.variant_name, "Some");
    assert_eq!(receipt.projection.field_index, 0);
    assert_eq!(receipt.projection.arity, 1);
    assert_eq!(receipt.projection.ownership, MirOwnership::Move);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Option<string>.unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn lowers_option_i32_unwrap_to_copy_variant_projection() {
    let source = include_str!("../../../tests/fixtures/mir_native_option_i32_unwrap.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Option<i32>.unwrap must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:unwrap_copy".into()))
        .expect("unwrap_copy MIR function");
    let receipt = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::VariantProject {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("Copy unwrap must carry a read-only variant projection receipt");
    assert_eq!(receipt.variant_name, "Some");
    assert_eq!(receipt.projection.field_index, 0);
    assert_eq!(receipt.projection.arity, 1);
    assert_eq!(receipt.projection.ownership, MirOwnership::Copy);
    assert_eq!(receipt.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Option<i32>.unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn copy_option_i32_typedesc_contract_rejects_other_payloads() {
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(
        include_str!("../../../tests/fixtures/mir_native_option_i32_unwrap.mimi"),
    ))
    .expect("Option<i32> MIR");
    let option_i32 = program
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == crate::core::mir::types::MirTypeKind::Option).then_some(id.clone())
        })
        .expect("Option<i32> TypeDesc");
    assert!(program
        .type_catalog()
        .validate_copy_option_i32_variant(&option_i32)
        .is_ok());

    let i64_program = crate::core::mir::reference::MirProgram::from_checked_program(
        &checked_program("func main() -> i64 { let value: Option<i64> = Some(7); drop(value); 7 }"),
    )
    .expect("Option<i64> construction MIR");
    let option_i64 = i64_program
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == crate::core::mir::types::MirTypeKind::Option).then_some(id.clone())
        })
        .expect("Option<i64> TypeDesc");
    let error = i64_program
        .type_catalog()
        .validate_copy_option_i32_variant(&option_i64)
        .expect_err("Option<i64> must stay outside the i32 island");
    assert!(error.contains("expected i32"), "{error}");
}

#[test]
fn option_string_unwrap_none_preserves_canonical_trap_class() {
    let source = r#"
        func main() -> i32 {
            let value: Option<string> = None
            let text = value.unwrap()
            drop(text)
            41
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Option<string>.unwrap None must still be canonical MIR");
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("unwrap on None must trap");
    assert!(
        error.to_string().contains("E0800"),
        "unexpected trap: {error}"
    );
}

#[test]
fn result_unwrap_remains_fail_closed_outside_option_projection_island() {
    let source = r#"
        func main() -> i32 {
            let value: Result<i32, i32> = Ok(41)
            value.unwrap()
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("Result::unwrap must remain outside the Option<string> projection island");
    let crate::core::mir::reference::MirProgramBuildError::Lowering(errors) = error else {
        panic!("Result::unwrap must fail during MIR lowering");
    };
    assert!(
        errors.iter().any(|error| error.message.contains(
            "Option/Result unwrap shape is outside the canonical variant projection contract"
        )),
        "unexpected fail-closed diagnostics: {errors:?}"
    );
}

#[test]
fn option_copy_unwrap_remains_fail_closed_outside_move_projection_island() {
    let source = r#"
        func main() -> i64 {
            let value: Option<i64> = Some(41)
            value.unwrap()
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("Copy Option::unwrap must remain outside the move projection island");
    let crate::core::mir::reference::MirProgramBuildError::Lowering(errors) = error else {
        panic!("Copy Option::unwrap must fail during MIR lowering");
    };
    assert!(
        errors.iter().any(|error| error.message.contains(
            "Option/Result unwrap shape is outside the canonical variant projection contract"
        )),
        "unexpected fail-closed diagnostics: {errors:?}"
    );
}

#[test]
fn materializes_generic_scalar_list_len_as_a_canonical_facade() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_list_len.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic List.len must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .next()
        .expect("generic List.len instance");
    assert!(matches!(
        instance.contract,
        MirGenericInstanceContract::ScalarListFacade {
            operation: crate::core::mir::MirListOperation::Len
        }
    ));
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized List.len target");
    let list_operations = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                operation: crate::core::mir::MirListOperation::Len,
                list_operation_contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(list_operations.len(), 1);
    assert!(matches!(
        program
            .type_catalog()
            .get(&list_operations[0].element_ty)
            .map(|descriptor| &descriptor.kind),
        Some(crate::core::mir::types::MirTypeKind::Primitive(
            PrimitiveType::I32
        ))
    ));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference List.len facade execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(3));
}

#[test]
fn materializes_generic_scalar_list_reverse_as_a_canonical_facade() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_list_reverse.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic List.reverse must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .next()
        .expect("generic List.reverse instance");
    assert!(matches!(
        instance.contract,
        MirGenericInstanceContract::ScalarListFacade {
            operation: crate::core::mir::MirListOperation::Reverse
        }
    ));
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized List.reverse target");
    let receipts = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                operation: crate::core::mir::MirListOperation::Reverse,
                list_operation_contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].list_ty, receipts[0].result_ty,
        "List.reverse must preserve one canonical List TypeDesc"
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference List.reverse facade execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(3));
}

#[test]
fn materializes_generic_scalar_list_concat_as_a_two_input_move_facade() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_list_concat.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic List.concat must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .next()
        .expect("generic List.concat instance");
    assert!(matches!(
        instance.contract,
        MirGenericInstanceContract::ScalarListFacade {
            operation: crate::core::mir::MirListOperation::Concat
        }
    ));
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized List.concat target");
    let mut move_results = Vec::new();
    let mut concat_receipt = None;
    for instruction in target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
    {
        match &instruction.kind {
            MirInstructionKind::Move { result, .. } => move_results.push(result.clone()),
            MirInstructionKind::ListOp {
                operation: crate::core::mir::MirListOperation::Concat,
                list,
                argument: Some(argument),
                list_operation_contract: Some(receipt),
                ..
            } => concat_receipt = Some((list.clone(), argument.clone(), receipt.clone())),
            _ => {}
        }
    }
    assert_eq!(
        move_results.len(),
        2,
        "Concat must move both callable inputs"
    );
    let (list, argument, receipt) = concat_receipt.expect("canonical concat receipt");
    assert!(move_results.contains(&list));
    assert!(move_results.contains(&argument));
    assert_eq!(receipt.argument_ty, Some(receipt.list_ty.clone()));
    assert_eq!(
        receipt.operation,
        crate::core::mir::MirListOperation::Concat
    );
    assert_eq!(receipt.result_ty, receipt.list_ty);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference List.concat facade execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(5));
}

#[test]
fn materializes_generic_scalar_list_construct_with_a_type_desc_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_list_construct.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic List construction must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .next()
        .expect("generic List construction instance");
    let MirGenericInstanceContract::ScalarListConstruct { contract } = &instance.contract else {
        panic!("expected a ScalarListConstruct instance contract");
    };
    assert_eq!(contract.element_count, 1);
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized List construction target");
    let receipts = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::ConstructList {
                list_construct_contract: Some(receipt),
                elements,
                ..
            } => Some((receipt, elements.len())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].0, contract);
    assert_eq!(receipts[0].1, 1);
    assert!(target.canonical_text().contains("list_construct_contract"));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference List construction execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(1));
}

#[test]
fn rejects_generic_list_construct_for_non_copy_elements_at_the_mir_gate() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_list_construct_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("List<string> generic construction must fail closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("outside the canonical Copy scalar contract")
            || message.contains("generic List facade candidate did not materialize"),
        "unexpected non-Copy generic List construction rejection: {message}"
    );
}

#[test]
fn materializes_generic_scalar_list_projection_with_a_constant_zero_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_list_projection.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic List projection must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .next()
        .expect("generic List projection instance");
    let MirGenericInstanceContract::ScalarListProjection {
        contract,
        index_value,
    } = &instance.contract
    else {
        panic!("expected a ScalarListProjection instance contract");
    };
    assert_eq!(*index_value, 0);
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized List projection target");
    let receipts = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::Project {
                projection: MirProjection::Index(_),
                list_index_contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0], contract);
    assert_eq!(contract.element_ty, contract.result_ty);
    assert!(target
        .canonical_text()
        .contains("list_index=MirListIndexProjectionContract"));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference List projection execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn materializes_generic_scalar_list_projection_with_a_constant_one_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_list_projection_index_one.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic List index-one projection must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .next()
        .expect("generic List index-one projection instance");
    assert!(matches!(
        instance.contract,
        MirGenericInstanceContract::ScalarListProjection { index_value: 1, .. }
    ));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference List index-one projection execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn rejects_generic_list_projection_for_managed_elements_at_the_mir_gate() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_list_projection_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("managed generic List projection must fail closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("outside scalar contract")
            || message.contains("generic List facade candidate did not materialize"),
        "unexpected managed generic List projection rejection: {message}"
    );
}

#[test]
fn rejects_generic_list_projection_for_nonzero_constant_index_at_the_mir_gate() {
    let source = r#"
        func first<T>(values: List<T>) -> T {
            values[2]
        }

        func main() -> i32 {
            let values = [41, 42]
            let picked = first(values)
            drop(values)
            picked
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("nonzero generic List projection must fail closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("constant index literal 0 or 1")
            || message.contains("literal-zero")
            || message.contains("constant zero"),
        "unexpected nonzero generic List projection rejection: {message}"
    );
}

#[test]
fn rejects_generic_list_concat_for_non_copy_elements_at_the_mir_gate() {
    let source = r#"
        func list_concat<T>(left: List<T>, right: List<T>) -> List<T> {
            left.concat(right)
        }

        func main() -> i32 {
            let left: List<string> = ["a"]
            let right: List<string> = ["b"]
            let joined = list_concat(left, right)
            let count = len(joined)
            drop(left)
            drop(right)
            drop(joined)
            count
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("List<string> generic concat must fail closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("outside the canonical Copy scalar contract"),
        "unexpected non-Copy generic List.concat rejection: {message}"
    );
}

#[test]
fn rejects_generic_list_concat_body_without_a_canonical_operation() {
    let source = r#"
        func bad<T>(left: List<T>, right: List<T>) -> List<T> { left }

        func main() -> i32 {
            let left: List<i32> = [1]
            let right: List<i32> = [2]
            let joined = bad(left, right)
            let count = len(joined)
            drop(left)
            drop(right)
            drop(joined)
            count
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic List.concat body without ListOp must fail closed");
    let message = format!("{error:?}");
    assert!(message.contains("generic List facade must lower to exactly one canonical ListOp"));
}

#[test]
fn rejects_generic_list_facade_with_multiple_operations_before_backends() {
    let source = r#"
        func bad<T>(values: List<T>) -> i32 {
            let first = len(values)
            let second = len(values)
            first + second
        }

        func main() -> i32 {
            let values: List<i32> = [1, 2]
            bad(values)
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("unsupported generic List body must fail closed");
    let message = format!("{error:?}");
    assert!(message.contains("generic List facade must lower to exactly one canonical ListOp"));
}

#[test]
fn rejects_generic_list_reverse_body_without_a_canonical_operation() {
    let source = r#"
        func bad<T>(values: List<T>) -> List<T> { values }

        func main() -> i32 {
            let values: List<i32> = [1, 2]
            let result = bad(values)
            drop(result)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic List body without reverse operation must fail closed");
    let message = format!("{error:?}");
    assert!(message.contains("generic List facade must lower to exactly one canonical ListOp"));
}

#[test]
fn rejects_generic_list_len_for_non_copy_elements_at_the_mir_gate() {
    let source = r#"
        func list_len<T>(values: List<T>) -> i32 { len(values) }

        func main() -> i32 {
            let values: List<string> = ["not-copy"]
            list_len(values)
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("non-Copy generic List.len must fail closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("outside scalar contract"),
        "unexpected non-Copy generic List.len rejection: {message}"
    );
}

fn fixture() -> MirFunction {
    let mut types = ResolvedTypeTable::new();
    let i64_ty = type_id(&mut types, ResolvedType::Primitive(PrimitiveType::I64));
    let entry = MirBlockId::new("bb.entry").unwrap();
    let exit = MirBlockId::new("bb.exit").unwrap();
    let arg = MirValueId::new("v.arg").unwrap();
    let one = MirValueId::new("v.one").unwrap();
    let sum = MirValueId::new("v.sum").unwrap();
    let result = MirValueId::new("v.result").unwrap();
    let mut values = BTreeMap::new();
    for id in [&arg, &one, &sum, &result] {
        values.insert(
            id.clone(),
            MirValue {
                id: id.clone(),
                ty: i64_ty.clone(),
            },
        );
    }
    MirFunction {
        owner: NodeId("func:test".into()),
        parameters: vec![arg.clone()],
        result: i64_ty.clone(),
        entry: entry.clone(),
        values,
        blocks: BTreeMap::from([
            (
                entry.clone(),
                MirBlock {
                    id: entry,
                    parameters: vec![],
                    instructions: vec![
                        MirInstruction {
                            id: MirInstructionId::new("i.const").unwrap(),
                            kind: MirInstructionKind::Const {
                                result: one.clone(),
                                literal: ResolvedLiteral::Int(1),
                            },
                        },
                        MirInstruction {
                            id: MirInstructionId::new("i.add").unwrap(),
                            kind: MirInstructionKind::Binary {
                                result: sum.clone(),
                                op: ResolvedBinaryOp::Add,
                                left: arg,
                                right: one,
                            },
                        },
                    ],
                    terminator: MirTerminator::Goto {
                        edge: MirEdgeId::new("e.return").unwrap(),
                        target: exit.clone(),
                        arguments: vec![sum],
                    },
                },
            ),
            (
                exit.clone(),
                MirBlock {
                    id: exit,
                    parameters: vec![MirBlockParameter {
                        value: result.clone(),
                    }],
                    instructions: vec![],
                    terminator: MirTerminator::Return {
                        value: Some(result),
                    },
                },
            ),
        ]),
        contracts: Vec::new(),
        ownership: MirOwnershipSummary::default(),
    }
}

#[test]
fn valid_function_passes_structural_validation() {
    let function = fixture();
    assert!(function.validate().is_ok(), "{:?}", function.validate());
}

#[test]
fn direct_variant_projection_reference_checks_active_tag_and_returns_payload() {
    let fixture = crate::core::mir::test_support::direct_variant_projection_fixture();
    let nominal =
        crate::core::ir::NominalTypeId::new("builtin:type:Option").expect("Option nominal");
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&fixture.program);
    let value = reference
        .execute(
            &fixture.function,
            &[crate::core::mir::reference::MirRuntimeValue::Variant {
                nominal: nominal.clone(),
                variant: fixture.some.clone(),
                payload: vec![crate::core::mir::reference::MirRuntimeValue::Int(41)],
            }],
        )
        .expect("direct Some projection");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));

    let error = reference
        .execute(
            &fixture.function,
            &[crate::core::mir::reference::MirRuntimeValue::Variant {
                nominal,
                variant: fixture.none.clone(),
                payload: Vec::new(),
            }],
        )
        .expect_err("wrong active variant must trap");
    assert!(error.message.contains("E0800"), "{error}");
}

#[test]
fn direct_variant_projection_receipt_is_checked_before_consumers() {
    let fixture = crate::core::mir::test_support::direct_variant_projection_fixture();
    assert_eq!(fixture.receipt.source_ty, fixture.source_ty);
    assert_eq!(fixture.receipt.result_ty, fixture.result_ty);
    assert_eq!(fixture.receipt.projection.variant, fixture.some);
    assert_eq!(fixture.receipt.projection.field, fixture.field);
    assert_eq!(fixture.receipt.discriminant, 1);
    assert_eq!(
        fixture.receipt.trap_code,
        crate::core::mir::types::MIR_VARIANT_PROJECTION_TRAP_CODE
    );
    assert!(fixture
        .program
        .functions()
        .get(&fixture.function)
        .expect("project function")
        .canonical_text()
        .contains("variant_project"));

    let mut function = fixture
        .program
        .functions()
        .get(&fixture.function)
        .expect("project function")
        .clone();
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::VariantProject { .. }))
        .expect("variant projection instruction");
    let MirInstructionKind::VariantProject {
        contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!();
    };
    receipt.discriminant = 0;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(fixture.function.clone(), function)]),
        fixture.program.type_catalog().clone(),
    )
    .expect_err("forged active-tag receipt must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant projection trap receipt disagrees with TypeDesc")
    }));
}

#[test]
fn consuming_variant_projection_moves_owned_payload_and_traps_on_wrong_tag() {
    let fixture = crate::core::mir::test_support::direct_variant_move_projection_fixture();
    assert_eq!(fixture.receipt.source_ty, fixture.source_ty);
    assert_eq!(fixture.receipt.result_ty, fixture.result_ty);
    let nominal =
        crate::core::ir::NominalTypeId::new("builtin:type:Option").expect("Option nominal");
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&fixture.program);
    let value = reference
        .execute(
            &fixture.function,
            &[crate::core::mir::reference::MirRuntimeValue::Variant {
                nominal: nominal.clone(),
                variant: fixture.some.clone(),
                payload: vec![crate::core::mir::reference::MirRuntimeValue::String(
                    "owned".into(),
                )],
            }],
        )
        .expect("consuming Some projection");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::String("owned".into())
    );

    let error = reference
        .execute(
            &fixture.function,
            &[crate::core::mir::reference::MirRuntimeValue::Variant {
                nominal,
                variant: fixture.none.clone(),
                payload: Vec::new(),
            }],
        )
        .expect_err("wrong active variant must trap before payload extraction");
    assert!(error.message.contains("E0800"), "{error}");
}

#[test]
fn consuming_variant_projection_receipt_is_move_owned_and_fail_closed() {
    let fixture = crate::core::mir::test_support::direct_variant_move_projection_fixture();
    assert_eq!(fixture.receipt.projection.ownership, MirOwnership::Move);
    assert_eq!(
        fixture.receipt.projection.move_out_glue,
        crate::core::mir::types::MirGlueKind::OwnedString
    );
    assert_eq!(fixture.receipt.projection.variant, fixture.some);
    assert_eq!(fixture.receipt.projection.field, fixture.field);
    assert_eq!(fixture.receipt.discriminant, 1);

    let mut function = fixture
        .program
        .functions()
        .get(&fixture.function)
        .expect("project function")
        .clone();
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::VariantProjectMove { .. }
            )
        })
        .expect("variant move projection instruction");
    let MirInstructionKind::VariantProjectMove {
        contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!();
    };
    receipt.projection.ownership = MirOwnership::Copy;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(fixture.function.clone(), function)]),
        fixture.program.type_catalog().clone(),
    )
    .expect_err("forged Copy move receipt must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant move projection trap receipt disagrees with TypeDesc")
    }));

    let mut double_use = fixture
        .program
        .functions()
        .get(&fixture.function)
        .expect("project function")
        .clone();
    double_use
        .blocks
        .values_mut()
        .next()
        .expect("entry block")
        .instructions
        .push(MirInstruction {
            id: MirInstructionId::new("i.after-variant-project-move").expect("instruction id"),
            kind: MirInstructionKind::Drop {
                value: MirValueId::new("v.input").expect("input value id"),
            },
        });
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(fixture.function.clone(), double_use)]),
        fixture.program.type_catalog().clone(),
    )
    .expect_err("consumed variant source must not be used again");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("use after consuming non-Copy value")));
}

#[test]
fn record_move_drop_projection_moves_selected_string_and_drops_residual() {
    let fixture = crate::core::mir::test_support::direct_record_move_drop_fixture();
    assert_eq!(fixture.receipt.source_ty, fixture.source_ty);
    assert_eq!(fixture.receipt.result_ty, fixture.result_ty);
    assert_eq!(fixture.receipt.projection.field, fixture.selected_field);
    assert_eq!(fixture.receipt.projection.field_index, 0);
    assert_eq!(fixture.receipt.residual.len(), 1);
    assert_eq!(fixture.receipt.residual[0].name, "right");
    assert_eq!(
        fixture.receipt.residual[0].glue,
        crate::core::mir::types::MirGlueKind::OwnedString
    );

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&fixture.program)
        .execute(
            &fixture.function,
            &[crate::core::mir::reference::MirRuntimeValue::Record {
                nominal: fixture.receipt.projection.nominal.clone(),
                fields: vec![
                    crate::core::mir::reference::MirRuntimeValue::String("left".into()),
                    crate::core::mir::reference::MirRuntimeValue::String("right".into()),
                ],
            }],
        )
        .expect("record move/drop projection");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::String("left".into())
    );
}

#[test]
fn record_move_drop_projection_receipt_and_source_use_fail_closed() {
    let fixture = crate::core::mir::test_support::direct_record_move_drop_fixture();
    let mut function = fixture
        .program
        .functions()
        .get(&fixture.function)
        .expect("project function")
        .clone();
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::MoveProjectDrop { .. }))
        .expect("record move/drop instruction");
    let MirInstructionKind::MoveProjectDrop {
        contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!();
    };
    receipt.residual.clear();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(fixture.function.clone(), function)]),
        fixture.program.type_catalog().clone(),
    )
    .expect_err("incomplete residual receipt must fail before consumers");
    assert!(errors.iter().any(|error| error
        .message
        .contains("record move/drop projection receipt disagrees")));

    let mut double_use = fixture
        .program
        .functions()
        .get(&fixture.function)
        .expect("project function")
        .clone();
    double_use
        .blocks
        .values_mut()
        .next()
        .expect("entry block")
        .instructions
        .push(MirInstruction {
            id: MirInstructionId::new("i.after-record-move-drop").expect("instruction id"),
            kind: MirInstructionKind::Drop {
                value: MirValueId::new("v.input").expect("input value id"),
            },
        });
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(fixture.function.clone(), double_use)]),
        fixture.program.type_catalog().clone(),
    )
    .expect_err("consumed record source must not be used again");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("use after consuming non-Copy value")));
}

#[test]
fn canonical_text_is_deterministic_and_contains_contract_shapes() {
    let function = fixture();
    let first = function.canonical_text();
    let second = function.canonical_text();
    assert_eq!(first, second);
    assert!(first.contains("mir.function func:test"));
    assert!(first.contains("binary v.sum = Add v.arg, v.one"));
    assert!(first.contains("goto e.return bb.exit(v.sum)"));
}

#[test]
fn missing_target_and_arity_are_rejected_before_backend() {
    let mut function = fixture();
    let entry = function
        .blocks
        .get_mut(&MirBlockId::new("bb.entry").unwrap())
        .unwrap();
    entry.terminator = MirTerminator::Goto {
        edge: MirEdgeId::new("e.bad").unwrap(),
        target: MirBlockId::new("bb.missing").unwrap(),
        arguments: vec![],
    };
    let errors = function
        .validate()
        .expect_err("invalid target must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("missing block")));
}

#[test]
fn malformed_trap_identity_is_rejected_before_backend() {
    let mut function = fixture();
    let entry = function
        .blocks
        .get_mut(&MirBlockId::new("bb.entry").unwrap())
        .unwrap();
    entry.terminator = MirTerminator::Trap { code: "\n".into() };
    let errors = function
        .validate()
        .expect_err("malformed trap must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("trap code is empty")));
}

#[test]
fn oversized_or_controlled_trap_identity_is_rejected_before_backend() {
    let mut function = fixture();
    let entry = function
        .blocks
        .get_mut(&MirBlockId::new("bb.entry").unwrap())
        .unwrap();
    entry.terminator = MirTerminator::Trap {
        code: format!(
            "bad{}",
            "x".repeat(crate::core::mir::types::MIR_TRAP_CODE_MAX_LEN)
        ),
    };
    let errors = function
        .validate()
        .expect_err("oversized trap must fail closed");
    assert!(errors.iter().any(|error| error.message.contains("exceeds")));

    let mut function = fixture();
    let entry = function
        .blocks
        .get_mut(&MirBlockId::new("bb.entry").unwrap())
        .unwrap();
    entry.terminator = MirTerminator::Trap {
        code: "trap\u{0007}".into(),
    };
    let errors = function
        .validate()
        .expect_err("control character in trap must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("control character")));
}

#[test]
fn duplicate_definition_is_rejected() {
    let mut function = fixture();
    let entry = function
        .blocks
        .get_mut(&MirBlockId::new("bb.entry").unwrap())
        .unwrap();
    entry.instructions.push(MirInstruction {
        id: MirInstructionId::new("i.duplicate").unwrap(),
        kind: MirInstructionKind::Const {
            result: MirValueId::new("v.one").unwrap(),
            literal: ResolvedLiteral::Int(2),
        },
    });
    let errors = function
        .validate()
        .expect_err("duplicate value definition must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("defined more than once")));
}

#[test]
fn value_catalog_identity_is_checked() {
    let mut function = fixture();
    let key = MirValueId::new("v.one").unwrap();
    let value = function.values.get_mut(&key).unwrap();
    value.id = MirValueId::new("v.other").unwrap();
    let errors = function
        .validate()
        .expect_err("catalog identity mismatch must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("catalog key disagrees")));
}

#[test]
fn use_before_definition_is_rejected_even_when_catalog_is_complete() {
    let mut function = fixture();
    let entry_id = MirBlockId::new("bb.entry").unwrap();
    let entry = function.blocks.get_mut(&entry_id).unwrap();
    entry.instructions.swap(0, 1);
    let errors = function
        .validate()
        .expect_err("an instruction cannot read a later definition");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("used before its definition")));
}

#[test]
fn ownership_events_are_part_of_the_canonical_function_contract() {
    let mut function = fixture();
    function.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::Move,
        resource: "resource:token".into(),
        value: None,
        source: Some("token".into()),
        target: Some("consumed".into()),
        point: NodeId("node:consume".into()),
    });
    assert!(
        function.validate().is_ok(),
        "ownership event should validate"
    );
    let text = function.canonical_text();
    assert!(text.contains("ownership[0] move resource=resource:token"));
    assert!(text.contains("source=token target=consumed point=node:consume"));
}

#[test]
fn ownership_event_without_source_is_rejected_for_consuming_kinds() {
    let mut function = fixture();
    function.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::Drop,
        resource: "resource:token".into(),
        value: None,
        source: None,
        target: None,
        point: NodeId("node:drop".into()),
    });
    let errors = function
        .validate()
        .expect_err("drop without a source must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("drop event has no source")));
}

#[test]
fn ownership_event_value_must_be_declared_by_the_function() {
    let mut function = fixture();
    function.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::Read,
        resource: "resource:token".into(),
        value: Some(MirValueId::new("local:missing").unwrap()),
        source: Some("token".into()),
        target: None,
        point: NodeId("node:read".into()),
    });
    let errors = function
        .validate()
        .expect_err("ownership values must be backed by the value catalog");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("event value 'local:missing' is absent")
    }));
}

#[test]
fn record_projection_contract_rejects_unknown_field_and_wrong_result_type() {
    let source = "type Point { x: i32, y: bool }\nfunc main() -> i32 { Point { x: 1, y: true }.x }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());

    let mut unknown = canonical.functions().get(&owner).cloned().expect("main");
    let projection = unknown
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::Project { projection, .. } => Some(projection),
            _ => None,
        })
        .expect("record projection");
    *projection = MirProjection::Field(crate::core::NodeId("field:missing".into()));
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner.clone(), unknown)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("unknown record field must fail before a backend");
    assert!(errors.iter().any(|error| error.message.contains("absent")));

    let bool_ty = canonical
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi == crate::core::mir::types::MirAbiClass::Bool).then(|| id.clone())
        })
        .expect("bool type");
    let mut wrong_type = canonical.functions().get(&owner).cloned().expect("main");
    let projection_result = wrong_type
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Project { result, .. } => Some(result.clone()),
            _ => None,
        })
        .expect("record projection result");
    wrong_type
        .values
        .get_mut(&projection_result)
        .expect("projection value")
        .ty = bool_ty.clone();
    wrong_type.result = bool_ty;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner, wrong_type)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("wrong record projection type must fail before a backend");
    assert!(errors.iter().any(|error| {
        error.message.contains("projection") && error.message.contains("disagrees")
    }));
}

#[test]
fn list_index_contract_rejects_non_integer_operand_before_backend() {
    let source = "func main() -> i32 { let values = [10, 20]; values[0] }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut function = canonical.functions().get(&owner).cloned().expect("main");
    let index = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Project {
                projection: MirProjection::Index(index),
                ..
            } => Some(index.clone()),
            _ => None,
        })
        .expect("List index projection");
    let bool_ty = canonical
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi == crate::core::mir::types::MirAbiClass::Bool).then(|| id.clone())
        })
        .expect("bool type");
    function.values.get_mut(&index).expect("index value").ty = bool_ty;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner, function)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("non-integer List index must fail before a backend");
    assert!(errors.iter().any(|error| {
        error.message.contains("List index operand") && error.message.contains("Copy scalar")
    }));
}

#[test]
fn list_index_projection_materializes_type_desc_receipt() {
    let source = "func main() -> i32 { let values = [10, 20, 30]; values[1] }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main");
    let receipt = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Project {
                projection: MirProjection::Index(index),
                list_index_contract: Some(receipt),
                ..
            } => Some((index.clone(), receipt.clone())),
            _ => None,
        })
        .expect("canonical List index receipt");
    let index_ty = &function.values.get(&receipt.0).expect("index value").ty;
    let result_ty = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Project {
                result,
                projection: MirProjection::Index(_),
                ..
            } => Some(&function.values.get(result).expect("result value").ty),
            _ => None,
        })
        .expect("List result type");
    assert_eq!(&receipt.1.index_ty, index_ty);
    assert_eq!(&receipt.1.result_ty, result_ty);
    assert_eq!(&receipt.1.element_ty, result_ty);
    assert!(canonical.functions().values().any(|function| function
        .canonical_text()
        .contains("list_index=MirListIndexProjectionContract")));
}

#[test]
fn canonical_program_gate_rejects_missing_or_stale_list_index_receipt() {
    let source = "func main() -> i32 { let values = [10, 20]; values[0] }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());

    let mut missing = canonical.functions().get(&owner).cloned().expect("main");
    let instruction = missing
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::Project {
                    projection: MirProjection::Index(_),
                    ..
                }
            )
        })
        .expect("List projection");
    let MirInstructionKind::Project {
        list_index_contract,
        ..
    } = &mut instruction.kind
    else {
        unreachable!()
    };
    list_index_contract.take();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner.clone(), missing)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("missing List index receipt must fail before backend");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("no canonical receipt")));

    let bool_ty = canonical
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi == crate::core::mir::types::MirAbiClass::Bool).then(|| id.clone())
        })
        .expect("bool TypeDesc");
    let mut stale = canonical.functions().get(&owner).cloned().expect("main");
    let instruction = stale
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::Project {
                    projection: MirProjection::Index(_),
                    ..
                }
            )
        })
        .expect("List projection");
    let MirInstructionKind::Project {
        list_index_contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!()
    };
    receipt.element_ty = bool_ty;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner, stale)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("stale List index receipt must fail before backend");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("List index projection receipt disagrees with TypeDesc")
    }));
}

#[test]
fn list_len_operation_materializes_type_desc_receipt() {
    let source =
        "func main() -> i32 { let values = [10, 20, 30]; let count = len(values); drop(values); count }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main");
    let (list_ty, result_ty, receipt) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                result,
                list,
                operation: MirListOperation::Len,
                list_operation_contract: Some(receipt),
                ..
            } => Some((
                function.values.get(list).expect("List value").ty.clone(),
                function.values.get(result).expect("count value").ty.clone(),
                receipt.clone(),
            )),
            _ => None,
        })
        .expect("canonical List.len receipt");
    assert_eq!(receipt.list_ty, list_ty);
    assert!(!receipt.element_ty.as_str().is_empty());
    assert_eq!(receipt.result_ty, result_ty);
    assert_eq!(receipt.operation, MirListOperation::Len);
    assert!(function
        .canonical_text()
        .contains("list_contract=MirListOperationContract"));
}

#[test]
fn list_reverse_operation_materializes_clone_based_type_desc_receipt() {
    let source = "func main() -> List<i32> { let values = [1, 2, 3]; let reversed = reverse(values); drop(values); reversed }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main");
    let (list_ty, result_ty, receipt) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                result,
                list,
                operation: MirListOperation::Reverse,
                list_operation_contract: Some(receipt),
                ..
            } => Some((
                function.values.get(list).expect("List value").ty.clone(),
                function
                    .values
                    .get(result)
                    .expect("reversed value")
                    .ty
                    .clone(),
                receipt.clone(),
            )),
            _ => None,
        })
        .expect("canonical List.reverse receipt");
    assert_eq!(receipt.list_ty, list_ty);
    assert_eq!(receipt.result_ty, result_ty);
    assert_eq!(receipt.result_ty, receipt.list_ty);
    assert!(!receipt.element_ty.as_str().is_empty());
    assert_eq!(receipt.operation, MirListOperation::Reverse);

    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&owner, &[])
        .expect("reference List.reverse execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::List(vec![
            crate::core::mir::reference::MirRuntimeValue::Int(3),
            crate::core::mir::reference::MirRuntimeValue::Int(2),
            crate::core::mir::reference::MirRuntimeValue::Int(1),
        ])
    );
}

#[test]
fn list_reverse_method_materializes_the_same_canonical_operation_receipt() {
    let source = "func main() -> List<i32> { let values = [1, 2, 3]; let reversed = values.reverse(); drop(values); reversed }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main");
    let receipt = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                operation: MirListOperation::Reverse,
                list_operation_contract: Some(receipt),
                ..
            } => Some(receipt.clone()),
            _ => None,
        })
        .expect("method call must lower to canonical List.reverse");
    assert_eq!(receipt.operation, MirListOperation::Reverse);
    assert_eq!(receipt.list_ty, receipt.result_ty);
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&owner, &[])
        .expect("reference List.reverse method execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::List(vec![
            crate::core::mir::reference::MirRuntimeValue::Int(3),
            crate::core::mir::reference::MirRuntimeValue::Int(2),
            crate::core::mir::reference::MirRuntimeValue::Int(1),
        ])
    );
}

#[test]
fn list_concat_method_materializes_two_input_move_receipt_and_consumes_both_sources() {
    let source = "func main() -> List<i32> { let left = [1, 2]; let right = [3, 4]; let joined = left.concat(right); joined }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main");
    let (left, right, receipt) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                list,
                argument: Some(argument),
                operation: MirListOperation::Concat,
                list_operation_contract: Some(receipt),
                ..
            } => Some((list.clone(), argument.clone(), receipt.clone())),
            _ => None,
        })
        .expect("canonical List.concat receipt");
    assert_eq!(receipt.operation, MirListOperation::Concat);
    assert_eq!(receipt.list_ty, receipt.result_ty);
    assert_eq!(receipt.argument_ty, Some(receipt.list_ty.clone()));
    // Scalar List containers are not checker-linear merely because their
    // element is Copy, so the resource ledger has no linear events to attach
    // here.  The operation's two explicit MoveOut inputs are the ownership
    // proof for this canonical heap-handle transform.
    assert!(function.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Move { result, .. } if result == &left
            )
        })
    }));
    assert!(function.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Move { result, .. } if result == &right
            )
        })
    }));
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&owner, &[])
        .expect("reference List.concat method execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::List(vec![
            crate::core::mir::reference::MirRuntimeValue::Int(1),
            crate::core::mir::reference::MirRuntimeValue::Int(2),
            crate::core::mir::reference::MirRuntimeValue::Int(3),
            crate::core::mir::reference::MirRuntimeValue::Int(4),
        ])
    );
}

#[test]
fn canonical_list_concat_receipt_rejects_missing_second_input_type() {
    let source = "func main() -> List<i32> { let left = [1]; let right = [2]; let joined = left.concat(right); joined }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical.functions().get(&owner).cloned().expect("main");
    let instruction = forged
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::ListOp {
                    operation: MirListOperation::Concat,
                    ..
                }
            )
        })
        .expect("List.concat operation");
    let MirInstructionKind::ListOp {
        list_operation_contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!()
    };
    receipt.argument_ty = None;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner, forged)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("missing List.concat argument TypeDesc must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("List operation receipt disagrees with TypeDesc")
    }));
}

#[test]
fn canonical_list_concat_rejects_aliasing_move_inputs_before_consumers() {
    let source =
        "func main() -> List<i32> { let values = [1, 2]; let joined = values.concat(values); joined }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("List.concat must not consume one List value twice");
    let debug = format!("{error:?}");
    assert!(
        debug.contains("use after consuming non-Copy value"),
        "unexpected aliasing diagnostic: {debug}"
    );
}

#[test]
fn canonical_program_gate_rejects_missing_or_stale_list_operation_receipt() {
    let source = "func main() -> i32 { let values = [10, 20]; len(values) }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());

    let mut missing = canonical.functions().get(&owner).cloned().expect("main");
    let instruction = missing
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::ListOp { .. }))
        .expect("List operation");
    let MirInstructionKind::ListOp {
        list_operation_contract,
        ..
    } = &mut instruction.kind
    else {
        unreachable!()
    };
    list_operation_contract.take();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner.clone(), missing)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("missing List operation receipt must fail before backend");
    assert!(errors.iter().any(|error| error
        .message
        .contains("List operation has no canonical receipt")));

    let mut stale = canonical.functions().get(&owner).cloned().expect("main");
    let instruction = stale
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::ListOp { .. }))
        .expect("List operation");
    let MirInstructionKind::ListOp {
        list_operation_contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!()
    };
    receipt.result_ty = receipt.list_ty.clone();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner, stale)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("stale List operation receipt must fail before backend");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("List operation receipt disagrees with TypeDesc")
    }));
}

#[test]
fn non_copy_record_projection_lowers_to_explicit_move_project() {
    let source = "type Named { name: string, count: i32 }\nfunc main() -> string { let p = Named { name: \"owned\", count: 41 }; p.name }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main");
    assert!(function.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|instruction| matches!(&instruction.kind, MirInstructionKind::MoveProject { .. }))
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&owner, &[])
        .expect("reference move projection");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::String("owned".into())
    );
}

#[test]
fn owned_string_return_lowers_to_move_and_reference_preserves_transfer() {
    let source = include_str!("../../../tests/fixtures/mir_native_owned_string_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned String return MIR");
    let owner = crate::core::NodeId("function:echo".into());
    let function = canonical
        .functions()
        .get(&owner)
        .expect("echo MIR function");
    let block = function
        .blocks
        .get(&function.entry)
        .expect("echo entry block");
    let [instruction] = block.instructions.as_slice() else {
        panic!("owned String return must have one ownership instruction");
    };
    let MirInstructionKind::Move { result, source } = &instruction.kind else {
        panic!("direct owned String return must move its source");
    };
    assert_eq!(source, &function.parameters[0]);
    assert!(matches!(
        &block.terminator,
        MirTerminator::Return { value: Some(value) } if value == result
    ));

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::String(
                "oracle".into(),
            )],
        )
        .expect("reference direct owned String return");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::String("oracle".into())
    );
}

#[test]
fn direct_owned_string_calls_remain_canonical_and_reference_transfers_arguments() {
    let source = include_str!("../../../tests/fixtures/mir_verifier_owned_string_call_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("direct owned String calls must lower to canonical MIR");
    let forward = program
        .functions()
        .get(&crate::core::NodeId("function:forward".into()))
        .expect("forward MIR function");
    assert!(forward
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .any(|instruction| matches!(
            instruction.kind,
            crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(ref owner),
                result: Some(_),
                ..
            } if owner.0 == "function:echo"
        )));

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &crate::core::NodeId("function:forward".into()),
            &[crate::core::mir::reference::MirRuntimeValue::String(
                "oracle".into(),
            )],
        )
        .expect("reference direct owned String call execution");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::String("oracle".into())
    );
}

#[test]
fn non_copy_record_projection_with_non_copy_sibling_fails_closed() {
    let source = "type Pair { left: string, right: string }\nfunc main() -> string { let p = Pair { left: \"left\", right: \"right\" }; p.left }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("record with a non-Copy sibling must not invent a residual");
    let text = format!("{error:?}");
    assert!(
        text.contains("non-Copy") || text.contains("move projection"),
        "unexpected fail-closed error: {text}"
    );
}

#[test]
fn non_copy_tuple_materializes_field_drop_schedule_before_backend() {
    let source = "func main() -> (string, i32) { (\"owned\", 41) }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("tuple glue must be materialized");
    let tuple_id = canonical
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            matches!(descriptor.layout, crate::core::mir::types::MirLayout::Tuple(ref fields) if fields.len() == 2)
                .then(|| id.clone())
        })
        .expect("tuple descriptor");
    let descriptor = canonical
        .type_catalog()
        .get(&tuple_id)
        .expect("tuple TypeDesc");
    assert_eq!(
        descriptor.glue,
        crate::core::mir::types::MirGlueContract {
            move_out: crate::core::mir::types::MirGlueKind::Aggregate,
            clone: crate::core::mir::types::MirGlueKind::Aggregate,
            drop: crate::core::mir::types::MirGlueKind::Aggregate,
        }
    );
    assert_eq!(
        descriptor
            .drop_plan
            .as_ref()
            .expect("drop plan")
            .fields
            .iter()
            .map(|field| field.index)
            .collect::<Vec<_>>(),
        vec![1, 0]
    );
}

#[test]
fn lowers_move_owned_option_payload_to_explicit_mir_node() {
    let source = "func main() -> Option<string> { Some(\"owned\") }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned variant glue must be materialized");
    let main = canonical
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("main MIR");
    assert!(main.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::ConstructVariantMove { .. }
            )
        })
    }));
}

#[test]
fn rejects_shallow_variant_construction_before_any_backend() {
    let source = "func main() -> Option<string> { Some(\"owned\") }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut function = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("main MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::ConstructVariantMove { .. }
            )
        })
        .expect("move variant construction");
    let crate::core::mir::MirInstructionKind::ConstructVariantMove {
        result,
        nominal,
        variant,
        fields,
    } = instruction.kind.clone()
    else {
        unreachable!();
    };
    instruction.kind = crate::core::mir::MirInstructionKind::ConstructVariant {
        result,
        nominal,
        variant,
        fields,
    };
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        std::collections::BTreeMap::from([(owner, function)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("shallow construction must fail closed");
    assert!(errors.iter().any(|error| {
        error.message.contains("ConstructVariantMove") || error.message.contains("non-Copy")
    }));
}

#[test]
fn flat_copy_user_enum_construction_rejects_forged_field_before_consumers() {
    let fixture = crate::core::mir::test_support::direct_flat_copy_enum_construct_fixture();
    let owner = fixture.function.clone();
    let mut function = fixture
        .program
        .functions()
        .get(&owner)
        .cloned()
        .expect("construct_signal MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::ConstructVariant { .. }
            )
        })
        .expect("flat Copy ConstructVariant");
    let crate::core::mir::MirInstructionKind::ConstructVariant { fields, .. } =
        &mut instruction.kind
    else {
        unreachable!();
    };
    fields[0].0 = crate::core::NodeId("variant:forged-field".into());

    let mut functions = fixture.program.functions().clone();
    functions.insert(owner, function);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        fixture.program.type_catalog().clone(),
    )
    .expect_err("forged flat Copy construction field must fail at canonical MIR");
    assert!(errors.iter().any(|error| {
        error.message.contains("variant payload field") && error.message.contains("absent")
    }));
}

#[test]
fn surface_flat_copy_user_enum_constructor_materializes_construct_variant() {
    let source = include_str!("../../../tests/fixtures/mir_custom_enum_flat_copy.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("flat Copy user-enum constructor must materialize to MIR");
    let function = canonical
        .functions()
        .get(&crate::core::NodeId("function:make_signal".into()))
        .expect("make_signal MIR");
    assert!(function.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::ConstructVariant { .. }
            )
        })
    }));
}

#[test]
fn surface_flat_copy_user_enum_match_materializes_switch() {
    let source = include_str!("../../../tests/fixtures/mir_custom_enum_flat_copy.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("flat Copy user-enum match must materialize to MIR");
    let function = canonical
        .functions()
        .get(&crate::core::NodeId("function:read_signal".into()))
        .expect("read_signal MIR");
    let switches = function
        .blocks
        .values()
        .filter_map(|block| match &block.terminator {
            crate::core::mir::MirTerminator::Switch { scrutinee, arms } => Some((scrutinee, arms)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(switches.len(), 1);
    let (scrutinee, arms) = switches[0];
    assert_eq!(arms.len(), 2);
    let scrutinee_ty = &function.values.get(scrutinee).expect("switch scrutinee").ty;
    assert!(canonical
        .type_catalog()
        .validate_flat_copy_variant(scrutinee_ty)
        .is_ok());
    assert!(arms.iter().any(|arm| !arm.bindings.is_empty()));
}

#[test]
fn surface_non_flat_user_enum_constructor_stays_outside_canonical_mir() {
    let source = include_str!("../../../tests/real_world/custom_enum_string_payload.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("non-flat user-enum constructor must remain outside this MIR slice");
    let text = format!("{error:?}");
    assert!(text.contains("Constructor") && text.contains("not a materialized MIR function"));
}

#[test]
fn surface_mixed_copy_user_enum_match_stays_outside_canonical_mir() {
    let source = "type Mixed { Number(i32) | Wide(i64) | Empty }\nfunc inspect(value: Mixed) -> i32 { match value { Number(n) => n, Wide(_) => 0, Empty => 0 } }\nfunc main() -> i32 { 0 }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("mixed Copy user-enum match must fail before consumers");
    let text = format!("{error:?}");
    assert!(text.contains("flat Copy variant contract"), "{text}");
}

#[test]
fn non_copy_record_materializes_field_drop_schedule_before_backend() {
    let source = "type Named { name: string, count: i32 }\nfunc main() -> i32 { let p = Named { count: 41, name: \"owned\" }; drop(p); 42 }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("record glue must be materialized");
    let (record_id, descriptor) = canonical
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            matches!(
                descriptor.layout,
                crate::core::mir::types::MirLayout::Record { ref nominal, .. }
                    if nominal.as_str().ends_with("Named")
            )
            .then(|| (id.clone(), descriptor))
        })
        .expect("record descriptor");
    assert_eq!(
        descriptor.ownership,
        crate::core::mir::types::MirOwnership::Move
    );
    assert_eq!(
        descriptor.glue,
        crate::core::mir::types::MirGlueContract {
            move_out: crate::core::mir::types::MirGlueKind::Aggregate,
            clone: crate::core::mir::types::MirGlueKind::Aggregate,
            drop: crate::core::mir::types::MirGlueKind::Aggregate,
        }
    );
    assert_eq!(
        descriptor
            .drop_plan
            .as_ref()
            .expect("record drop plan")
            .fields
            .iter()
            .map(|field| field.index)
            .collect::<Vec<_>>(),
        vec![1, 0]
    );
    canonical
        .type_catalog()
        .validate_aggregate_glue(&record_id, crate::core::mir::types::MirGlueOperation::Drop)
        .expect("record drop schedule");
}

#[test]
fn rejects_reuse_of_record_field_after_aggregate_construction() {
    let source = "type Named { name: string, count: i32 }\nfunc main() -> i32 { let p = Named { count: 41, name: \"owned\" }; drop(p); 42 }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("record MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut function = canonical.functions().get(&owner).cloned().expect("main");
    let block = function
        .blocks
        .values_mut()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::Construct { .. }
                )
            })
        })
        .expect("record construction block");
    let field = block
        .instructions
        .iter()
        .find_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Construct { fields, .. } => fields
                .iter()
                .find(|field| {
                    function
                        .values
                        .get(*field)
                        .and_then(|value| canonical.type_catalog().get(&value.ty))
                        .is_some_and(|descriptor| {
                            descriptor.ownership == crate::core::mir::types::MirOwnership::Move
                        })
                })
                .cloned(),
            _ => None,
        })
        .expect("owned record field");
    block.instructions.push(crate::core::mir::MirInstruction {
        id: crate::core::mir::MirInstructionId::new("synthetic/reuse-record-field")
            .expect("instruction id"),
        kind: crate::core::mir::MirInstructionKind::Drop { value: field },
    });
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        std::collections::BTreeMap::from([(owner, function)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("record field reuse must fail before a backend");
    assert!(errors.iter().any(|error| {
        error.message.contains("use after consuming")
            || error.message.contains("already consumed")
            || error.message.contains("multiple")
            || error.message.contains("reuse")
    }));
}

#[test]
fn materializes_record_with_move_owned_variant_field() {
    let source = "type Bad { value: Option<string> }\nfunc main() -> i32 { let p = Bad { value: Some(\"owned\") }; drop(p); 42 }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("variant field glue is materialized recursively");
    assert!(canonical.type_catalog().iter().any(|(_, descriptor)| {
        matches!(
            descriptor.layout,
            crate::core::mir::types::MirLayout::Record { .. }
        ) && descriptor.glue.move_out == crate::core::mir::types::MirGlueKind::Aggregate
    }));
}

#[test]
fn malformed_aggregate_drop_schedule_is_rejected_before_backend() {
    let source = "func main() -> (string, i32) { (\"owned\", 41) }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).cloned().expect("main");
    let tuple_id = function
        .values
        .values()
        .map(|value| value.ty.clone())
        .find(|ty| {
            canonical
                .type_catalog()
                .get(ty)
                .is_some_and(|descriptor| descriptor.drop_plan.is_some())
        })
        .expect("tuple value");
    let mut catalog = canonical.type_catalog().clone();
    let descriptor = catalog
        .iter()
        .find_map(|(id, descriptor)| (id == &tuple_id).then(|| descriptor.clone()))
        .expect("tuple descriptor");
    let mut malformed = descriptor.drop_plan.clone().expect("tuple drop plan");
    malformed.fields.reverse();
    catalog.replace_for_test_only(
        tuple_id,
        crate::core::mir::types::MirTypeDesc {
            drop_plan: Some(malformed),
            variant_drop_plan: descriptor.variant_drop_plan.clone(),
            ..descriptor
        },
    );
    let error = crate::core::mir::reference::MirProgram::with_type_catalog(
        std::collections::BTreeMap::from([(owner, function)]),
        catalog,
    )
    .expect_err("malformed drop plan must fail closed");
    assert!(error
        .iter()
        .any(|error| error.message.contains("drop plan")));
}

#[test]
fn rejects_reuse_of_tuple_field_after_aggregate_construction() {
    let source = "func main() -> (string, i32) { (\"owned\", 41) }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut function = canonical.functions().get(&owner).cloned().expect("main");
    let block = function
        .blocks
        .values_mut()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::Construct { .. }
                )
            })
        })
        .expect("tuple construction block");
    let field = block
        .instructions
        .iter()
        .find_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Construct { fields, .. } => fields
                .iter()
                .find(|field| {
                    function
                        .values
                        .get(*field)
                        .and_then(|value| canonical.type_catalog().get(&value.ty))
                        .is_some_and(|descriptor| {
                            descriptor.ownership == crate::core::mir::types::MirOwnership::Move
                        })
                })
                .cloned(),
            _ => None,
        })
        .expect("owned tuple field");
    block.instructions.push(crate::core::mir::MirInstruction {
        id: crate::core::mir::MirInstructionId::new("synthetic/reuse-tuple-field")
            .expect("instruction id"),
        kind: crate::core::mir::MirInstructionKind::Drop { value: field },
    });
    let error = crate::core::mir::reference::MirProgram::with_type_catalog(
        std::collections::BTreeMap::from([(owner, function)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("an aggregate construction must consume each owned field once");
    assert!(error
        .iter()
        .any(|error| error.message.contains("use after consuming non-Copy value")));
}

#[test]
fn flat_copy_variant_predicates_materialize_checker_receipts() {
    let source = include_str!("../../../tests/fixtures/mir_native_variant_predicate.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("flat Copy variant predicates must lower");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main MIR");
    let predicates = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::VariantPredicate {
                predicate,
                result,
                variant,
                contract: Some(receipt),
                ..
            } => Some((*predicate, result.clone(), variant.clone(), receipt.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(predicates.len(), 4);
    for (predicate, result, variant, receipt) in predicates {
        assert_eq!(receipt.variant_ty, function.values[&variant].ty);
        assert_eq!(receipt.result_ty, function.values[&result].ty);
        assert_eq!(
            canonical
                .type_catalog()
                .get(&receipt.result_ty)
                .map(|desc| &desc.kind),
            Some(&crate::core::mir::types::MirTypeKind::Primitive(
                crate::core::PrimitiveType::Bool
            ))
        );
        assert_eq!(receipt.predicate, predicate);
        assert_eq!(
            receipt.nominal.as_str(),
            match predicate {
                crate::core::mir::MirVariantPredicate::IsSome
                | crate::core::mir::MirVariantPredicate::IsNone => "builtin:type:Option",
                crate::core::mir::MirVariantPredicate::IsOk
                | crate::core::mir::MirVariantPredicate::IsErr => "builtin:type:Result",
            }
        );
        assert_eq!(
            receipt.variant_name,
            match predicate {
                crate::core::mir::MirVariantPredicate::IsSome => "Some",
                crate::core::mir::MirVariantPredicate::IsNone => "None",
                crate::core::mir::MirVariantPredicate::IsOk => "Ok",
                crate::core::mir::MirVariantPredicate::IsErr => "Err",
            }
        );
        assert!(receipt.discriminant <= u8::MAX as u16);
    }
    let text = function.canonical_text();
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("variant_predicate "))
            .count(),
        4
    );
    assert!(text.contains("variant_contract=MirVariantPredicateContract"));
}

#[test]
fn variant_predicate_receipt_drift_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_variant_predicate.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("main MIR");
    let instruction = forged
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::VariantPredicate { .. }
            )
        })
        .expect("variant predicate");
    let crate::core::mir::MirInstructionKind::VariantPredicate {
        contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!("predicate receipt is mandatory in canonical MIR");
    };
    receipt.discriminant = receipt.discriminant.wrapping_add(1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner, forged)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("stale variant predicate receipt must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant predicate receipt disagrees with TypeDesc")
    }));
}

#[test]
fn non_copy_variant_predicate_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_variant_predicate_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("non-Copy variant predicate must fail closed");
    let debug = format!("{error:?}");
    assert!(
        debug.contains("Aggregate/Copy") && debug.contains("canonical no-op glue"),
        "unexpected non-Copy predicate diagnostic: {debug}"
    );
}

#[test]
fn direct_flat_copy_variant_calls_materialize_signature_receipts() {
    let source = include_str!("../../../tests/fixtures/mir_native_variant_call.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("flat Copy variant calls must lower");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main MIR");
    let calls = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                variant_call_contract: Some(receipt),
                ..
            } => Some((callee.clone(), receipt.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    for (callee, receipt) in calls {
        assert_eq!(callee, receipt.callee);
        assert_eq!(
            receipt.type_arguments,
            Vec::<crate::core::ResolvedTypeId>::new()
        );
        assert_eq!(receipt.parameter_types.len(), 1);
        assert_eq!(receipt.nominal.as_str(), "builtin:type:Option");
        assert_eq!(receipt.variants.len(), 2);
        assert!(receipt
            .variants
            .iter()
            .all(|variant| variant.discriminant <= u8::MAX as u16));
        canonical
            .type_catalog()
            .validate_variant_call_abi_receipt(
                &receipt.callee,
                &receipt.type_arguments,
                &receipt.parameter_types,
                &receipt.result_ty,
                &receipt,
            )
            .expect("receipt must be TypeDesc-derived");
    }
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&owner, &[])
        .expect("reference direct variant call execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(4)
    );
}

#[test]
fn direct_move_owned_result_calls_materialize_signature_receipts() {
    let source = include_str!("../../../tests/fixtures/mir_result_string_i32_call_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned Result calls must lower");
    let owner = crate::core::NodeId("function:main".into());
    let calls = canonical
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                variant_call_contract: Some(receipt),
                ..
            } => Some((callee.clone(), receipt.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    for (callee, receipt) in calls {
        assert_eq!(callee, receipt.callee);
        assert_eq!(
            receipt.mode,
            crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
        );
        assert_eq!(
            receipt.return_mode,
            crate::core::mir::types::MirVariantCallReturnMode::OwnershipPathExclusiveMerge
        );
        assert_eq!(receipt.payload_types.len(), 2);
        assert_eq!(receipt.payload_ty, receipt.payload_types[0]);
        canonical
            .type_catalog()
            .validate_variant_call_abi_receipt(
                &receipt.callee,
                &receipt.type_arguments,
                &receipt.parameter_types,
                &receipt.result_ty,
                &receipt,
            )
            .expect("move-owned call receipt must be TypeDesc-derived");
    }
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&owner, &[])
        .expect("reference move-owned Result call execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(48)
    );
}

#[test]
fn move_owned_result_call_receipt_drift_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_result_string_i32_call_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let mut forged = canonical.functions().clone();
    let instruction = forged
        .values_mut()
        .flat_map(|function| function.blocks.values_mut())
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::Call {
                    variant_call_contract: Some(_),
                    ..
                }
            )
        })
        .expect("move-owned Result call");
    let crate::core::mir::MirInstructionKind::Call {
        variant_call_contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!("receipt selected above");
    };
    receipt.payload_types[1] = receipt.payload_types[0].clone();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged,
        canonical.type_catalog().clone(),
    )
    .expect_err("stale move-owned call ABI receipt must fail before a backend");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant call ABI receipt disagrees with TypeDesc")
    }));
}

#[test]
fn move_owned_result_return_merge_rejects_switch_before_consumers() {
    let source =
        include_str!("../../../tests/fixtures/mir_result_string_i32_call_return_multipath.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:choose".into());
    let mut forged = canonical.functions().clone();
    let choose = forged.get_mut(&owner).expect("choose MIR");
    let entry = choose.entry.clone();
    let old_terminator = choose
        .blocks
        .get(&entry)
        .expect("choose entry")
        .terminator
        .clone();
    let crate::core::mir::MirTerminator::Branch {
        condition,
        then_edge,
        then_target,
        then_arguments,
        else_edge,
        else_target,
        else_arguments,
    } = old_terminator
    else {
        panic!("choose must start with a canonical Branch");
    };
    choose
        .blocks
        .get_mut(&entry)
        .expect("choose entry")
        .terminator = crate::core::mir::MirTerminator::Switch {
        scrutinee: condition,
        arms: vec![
            crate::core::mir::MirSwitchArm {
                edge: then_edge,
                target: then_target,
                arguments: then_arguments,
                bindings: vec![],
                case: crate::core::mir::MirSwitchCase::Literal(crate::core::ResolvedLiteral::Bool(
                    true,
                )),
            },
            crate::core::mir::MirSwitchArm {
                edge: else_edge,
                target: else_target,
                arguments: else_arguments,
                bindings: vec![],
                case: crate::core::mir::MirSwitchCase::Default,
            },
        ],
    };
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged,
        canonical.type_catalog().clone(),
    )
    .expect_err("unsupported ownership merge CFG must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("MIR verifier direct variant call return merge only admits Goto/Branch CFG")
    }));
}

#[test]
fn result_switch_move_projection_receipt_carries_field_ownership_and_glue() {
    let source =
        include_str!("../../../tests/fixtures/mir_result_string_i32_call_return_multipath.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let checked_fn = canonical
        .functions()
        .get(&crate::core::NodeId("function:checked".into()))
        .expect("checked MIR");
    let arms = checked_fn
        .blocks
        .values()
        .find_map(|block| match &block.terminator {
            crate::core::mir::MirTerminator::SwitchMove { arms, .. } => Some(arms),
            _ => None,
        })
        .expect("Result SwitchMove");
    let mut projections = arms
        .iter()
        .filter_map(|arm| {
            let variant = match &arm.case {
                crate::core::mir::MirSwitchCase::Variant(variant) => variant,
                _ => return None,
            };
            Some((variant.clone(), arm.bindings.first()?.projection.clone()))
        })
        .collect::<Vec<_>>();
    projections.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(projections.len(), 2);
    let ok = projections
        .iter()
        .find(|(variant, _)| variant.0.ends_with("Result::Ok"))
        .expect("Ok projection");
    assert_eq!(ok.1.ownership, crate::core::mir::types::MirOwnership::Move);
    assert_eq!(
        ok.1.move_out_glue,
        crate::core::mir::types::MirGlueKind::OwnedString
    );
    let err = projections
        .iter()
        .find(|(variant, _)| variant.0.ends_with("Result::Err"))
        .expect("Err projection");
    assert_eq!(err.1.ownership, crate::core::mir::types::MirOwnership::Copy);
    assert_eq!(
        err.1.move_out_glue,
        crate::core::mir::types::MirGlueKind::Noop
    );
}

#[test]
fn result_switch_move_projection_receipt_drift_is_rejected_before_consumers() {
    let source =
        include_str!("../../../tests/fixtures/mir_result_string_i32_call_return_multipath.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let mut forged = canonical.functions().clone();
    let checked_fn = forged
        .get_mut(&crate::core::NodeId("function:checked".into()))
        .expect("checked MIR");
    let binding = checked_fn
        .blocks
        .values_mut()
        .flat_map(|block| match &mut block.terminator {
            crate::core::mir::MirTerminator::SwitchMove { arms, .. } => arms
                .iter_mut()
                .flat_map(|arm| arm.bindings.iter_mut())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .find(|binding| binding.projection.variant.0.ends_with("Result::Ok"))
        .expect("Ok projection binding");
    binding.projection.move_out_glue = crate::core::mir::types::MirGlueKind::Noop;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged,
        canonical.type_catalog().clone(),
    )
    .expect_err("forged projection glue must fail before a consumer");
    assert!(errors
        .iter()
        .any(|error| { error.message.contains("variant payload projection receipt") }));
}

#[test]
fn custom_enum_switch_move_receipt_drift_is_rejected_before_consumers() {
    let fixture = crate::core::mir::test_support::direct_enum_switch_move_fixture();
    let owner = crate::core::NodeId("function:take".into());
    let mut forged = fixture.program.functions().clone();
    let forged_fn = forged.get_mut(&owner).expect("enum take MIR");
    let binding = forged_fn
        .blocks
        .values_mut()
        .flat_map(|block| match &mut block.terminator {
            crate::core::mir::MirTerminator::SwitchMove { arms, .. } => arms
                .iter_mut()
                .flat_map(|arm| arm.bindings.iter_mut())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .next()
        .expect("Keep projection binding");
    binding.projection.move_out_glue = crate::core::mir::types::MirGlueKind::Noop;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged,
        fixture.program.type_catalog().clone(),
    )
    .expect_err("forged enum projection glue must fail before consumers");
    assert!(errors
        .iter()
        .any(|error| { error.message.contains("variant payload projection receipt") }));
}

#[test]
fn result_read_only_switch_rejects_move_owned_payload_projection() {
    let source =
        include_str!("../../../tests/fixtures/mir_result_string_i32_call_return_multipath.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let mut forged = canonical.functions().clone();
    let checked_fn = forged
        .get_mut(&crate::core::NodeId("function:checked".into()))
        .expect("checked MIR");
    let block = checked_fn
        .blocks
        .values_mut()
        .find(|block| {
            matches!(
                block.terminator,
                crate::core::mir::MirTerminator::SwitchMove { .. }
            )
        })
        .expect("Result SwitchMove");
    let crate::core::mir::MirTerminator::SwitchMove { scrutinee, arms } = block.terminator.clone()
    else {
        unreachable!("Result SwitchMove selected above");
    };
    block.terminator = crate::core::mir::MirTerminator::Switch { scrutinee, arms };
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged,
        canonical.type_catalog().clone(),
    )
    .expect_err("read-only Switch cannot transport an owned Result payload");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("read-only variant payload projection field")
    }));
}

#[test]
fn variant_call_abi_receipt_drift_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_variant_call.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical.functions().clone();
    let main = forged.get_mut(&owner).expect("main MIR");
    let instruction = main
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::Call {
                    variant_call_contract: Some(_),
                    ..
                }
            )
        })
        .expect("variant call");
    let crate::core::mir::MirInstructionKind::Call {
        variant_call_contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!("receipt selected above");
    };
    receipt.variants[0].discriminant = receipt.variants[0].discriminant.wrapping_add(1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged,
        canonical.type_catalog().clone(),
    )
    .expect_err("stale call ABI receipt must fail before a backend");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant call ABI receipt disagrees with TypeDesc")
    }));
}
