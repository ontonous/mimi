use super::*;
use crate::core::ir::{PrimitiveType, ResolvedType, ResolvedTypeTable};

fn type_id(table: &mut ResolvedTypeTable, ty: ResolvedType) -> ResolvedTypeId {
    table.intern_resolved(ty).expect("test type must intern")
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
