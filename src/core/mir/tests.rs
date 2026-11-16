use super::*;
use crate::core::ir::{
    PrimitiveType, ResolvedExprKind, ResolvedPatternKind, ResolvedStmtKind, ResolvedType,
    ResolvedTypeTable,
};
use crate::core::mir::types::{MirGlueKind, MirLayout, MirOwnership, MirTypeKind};

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
fn keys_snapshot_checked_route_executes_in_reference_after_owner_drops() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_keys_snapshot.mimi"
    ));
    let canonical = reference::MirProgram::from_checked_program(&checked)
        .expect("exact KeysSnapshot source lowers to canonical MIR");
    let route = validate_ordinary_map_owned_any_v2_keys_snapshot_route(&checked, &canonical)
        .expect("exact Checker/MIR pair validates the KeysSnapshot route");
    assert_eq!(
        route.profile,
        ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE
    );

    let main = canonical
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("fixture main function");
    let checker_route = main
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .expect("Checker-owned owner-v2 source receipt");
    let [source_receipt] = checker_route.keys_snapshot_receipts.as_slice() else {
        panic!("exact KeysSnapshot fixture has one Checker ownership receipt");
    };
    assert_eq!(source_receipt.static_key_utf8.as_slice(), b"answer");
    assert_eq!(
        main.ownership.checker_ordinary_map_owned_any_v2.len(),
        5,
        "New, Set, Keys, and the two Map-root drops are all receipted"
    );

    let reference = reference::MirReferenceInterpreter::new(&canonical)
        .with_route_receipt(&route)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("validated KeysSnapshot route executes in the reference interpreter");
    assert_eq!(reference.value, reference::MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "answer\n");
}

#[test]
fn canonical_identity_uses_explicit_effect_and_projection_text() {
    assert_eq!(
        MirTransitionEffect::RecoverableBoundary.canonical_text(),
        "recoverable_boundary"
    );
    assert_eq!(
        MirProjection::Field(crate::core::NodeId("field:amount".into())).canonical_text(),
        "field(field:amount)"
    );
    let expression = MirContractExpr::Project {
        base: Box::new(MirContractExpr::Value(
            MirValueId::new("v.amount").expect("value id"),
        )),
        projection: MirProjection::Tuple(1),
    };
    assert_eq!(expression.canonical_text(), "project(v.amount, tuple(1))");
}

#[test]
fn canonical_ffi_scalar_kind_is_closed_at_the_mir_boundary() {
    use crate::core::mir::types::{MirAbiClass, MirFfiScalarKind};

    assert_eq!(
        MirAbiClass::Integer {
            bits: 32,
            signed: true,
        }
        .canonical_ffi_scalar_kind(),
        Some(MirFfiScalarKind::I32)
    );
    assert_eq!(
        MirAbiClass::Integer {
            bits: 64,
            signed: true,
        }
        .canonical_ffi_scalar_kind(),
        Some(MirFfiScalarKind::I64)
    );
    assert_eq!(
        MirAbiClass::Bool.canonical_ffi_scalar_kind(),
        Some(MirFfiScalarKind::Bool)
    );
    assert_eq!(
        MirAbiClass::Float { bits: 64 }.canonical_ffi_scalar_kind(),
        Some(MirFfiScalarKind::F64)
    );
    assert_eq!(
        MirAbiClass::Float { bits: 32 }.canonical_ffi_scalar_kind(),
        Some(MirFfiScalarKind::F32)
    );
    assert_eq!(
        MirAbiClass::Unit.canonical_ffi_scalar_kind(),
        Some(MirFfiScalarKind::Unit)
    );
    for abi in [
        MirAbiClass::Integer {
            bits: 32,
            signed: false,
        },
        MirAbiClass::Char,
        MirAbiClass::StringHandle,
        MirAbiClass::Aggregate,
    ] {
        assert_eq!(abi.canonical_ffi_scalar_kind(), None);
    }

    for kind in [
        MirFfiScalarKind::I32,
        MirFfiScalarKind::I64,
        MirFfiScalarKind::Bool,
        MirFfiScalarKind::F32,
        MirFfiScalarKind::F64,
        MirFfiScalarKind::Unit,
    ] {
        assert_eq!(kind.abi_class().canonical_ffi_scalar_kind(), Some(kind));
    }
}

#[test]
fn ordinary_map_lifecycle_runtime_helpers_are_reserved_source_ffi_symbols() {
    for symbol in [
        "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new",
        "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_set_string",
        "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop",
    ] {
        assert!(!canonical_ffi_symbol_is_manifest_safe(symbol));
        assert_eq!(
            validate_ffi_symbol_manifest_safety(symbol),
            Err(
                "FFI symbol is not manifest-safe (FFI symbol names a reserved Canonical MIR ordinary Map lifecycle runtime helper)"
                    .into()
            )
        );
    }
}

#[test]
fn ordinary_map_owned_get_runtime_helpers_are_reserved_source_ffi_symbols() {
    const RESERVED: &str = "mimi_mir_ordinary_map_owned_get_i32_string_v1_get";
    assert_eq!(
        validate_ffi_symbol_manifest_safety(RESERVED),
        Err("FFI symbol is not manifest-safe (FFI symbol names a reserved Canonical MIR ordinary Map owned-get runtime helper)".into())
    );
    assert!(validate_ffi_symbol_manifest_safety(
        "mimi_mir_ordinary_map_owned_get_i32_string_v1x_user_function"
    )
    .is_ok());

    let checked = checked_program(
        r#"
extern "C" { func mimi_mir_ordinary_map_owned_get_i32_string_v1_get(value: i64) -> i64; }
func main() -> i64 { mimi_mir_ordinary_map_owned_get_i32_string_v1_get(1 as i64) }
"#,
    );
    let errors = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("source imports must not claim ordinary Map owned-get runtime symbols");
    assert!(
        errors.to_diagnostics().iter().any(|diagnostic| {
            diagnostic.code.is_none()
                && diagnostic
                    .message
                    .contains("FFI symbol is not manifest-safe")
                && diagnostic
                    .message
                    .contains("reserved Canonical MIR ordinary Map owned-get runtime helper")
        }),
        "{errors:?}"
    );
}

#[test]
fn owned_get_profile_receipt_binds_typed_get_and_root_lifecycle() {
    use crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind as K;
    use crate::core::OrdinaryMapOwnedGetI32StringV1ProfileVersion as V;

    let owner_ty = types::ordinary_map_owned_get_i32_string_v1_type_id();
    let i32_ty = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let string_ty = crate::core::ir::primitive_type_id(PrimitiveType::String);
    let result_ty = crate::core::ir::result_type_id(i32_ty.clone(), i32_ty.clone());
    let result_string_ty = crate::core::ir::result_type_id(string_ty.clone(), i32_ty.clone());
    let root = MirValueId::new("local:root").expect("root identity");
    let key = MirValueId::new("expr:node:key").expect("key identity");
    let getter_result = MirValueId::new("expr:pget").expect("getter result identity");
    let answer = MirValueId::new("v.answer").expect("return identity");
    let root_name = "ordinary-map-owned-get-i32-string-v1:root".to_string();

    let action = |kind: K,
                  point: &str,
                  result: Option<MirValueId>,
                  key_node: Option<NodeId>,
                  action_result_type: Option<ResolvedTypeId>| {
        let key = key_node
            .as_ref()
            .and_then(|node| MirValueId::new(format!("expr:{}", node.0)).ok());
        let key_type = key_node.as_ref().map(|_| string_ty.clone());
        MirCheckerOrdinaryMapOwnedGetI32StringV1Action {
            profile_version: V::V1,
            kind,
            point: NodeId(point.into()),
            local: root.clone(),
            result,
            root: root_name.clone(),
            key_node,
            key,
            key_type,
            key_local: None,
            value: None,
            value_type: None,
            result_type: action_result_type,
            source_local: None,
            source: None,
            instruction: MirInstructionId::new(format!(
                "inst:ordinary_map_owned_get_i32_string_v1_{}:{}:{point}",
                kind.as_str(),
                root
            ))
            .expect("owned-get instruction identity"),
        }
    };
    let receipts = vec![
        action(K::New, "pnew", Some(root.clone()), None, None),
        action(
            K::GetI32,
            "pget",
            Some(getter_result.clone()),
            Some(NodeId("node:key".into())),
            Some(result_ty.clone()),
        ),
        action(K::Drop, "pdrop", None, None, None),
    ];
    let mut values = BTreeMap::new();
    for (id, ty) in [
        (root.clone(), owner_ty),
        (key.clone(), string_ty.clone()),
        (getter_result.clone(), result_ty.clone()),
        (answer.clone(), i32_ty.clone()),
    ] {
        values.insert(id.clone(), MirValue { id, ty });
    }
    let entry = MirBlockId::new("bb.entry").expect("entry block");
    let instructions = vec![
        MirInstruction {
            id: receipts[0].instruction.clone(),
            kind: MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New {
                result: root.clone(),
            },
        },
        MirInstruction {
            id: MirInstructionId::new("inst:key").expect("key instruction"),
            kind: MirInstructionKind::Const {
                result: key.clone(),
                literal: ResolvedLiteral::String("missing".into()),
            },
        },
        MirInstruction {
            id: receipts[1].instruction.clone(),
            kind: MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 {
                result: getter_result.clone(),
                root: root.clone(),
                key: key.clone(),
            },
        },
        MirInstruction {
            id: MirInstructionId::new("inst:key_drop").expect("key drop instruction"),
            kind: MirInstructionKind::Drop { value: key.clone() },
        },
        MirInstruction {
            id: receipts[2].instruction.clone(),
            kind: MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { root: root.clone() },
        },
        MirInstruction {
            id: MirInstructionId::new("inst:return").expect("return instruction"),
            kind: MirInstructionKind::Const {
                result: answer.clone(),
                literal: ResolvedLiteral::Int(0),
            },
        },
    ];
    let function = MirFunction {
        owner: NodeId("function:test_owned_get".into()),
        parameters: Vec::new(),
        parameter_permissions: Some(Vec::new()),
        result: i32_ty.clone(),
        entry: entry.clone(),
        values,
        blocks: BTreeMap::from([(
            entry.clone(),
            MirBlock {
                id: entry,
                parameters: Vec::new(),
                instructions,
                terminator: MirTerminator::Return {
                    value: Some(answer),
                },
            },
        )]),
        contracts: Vec::new(),
        proof_worklist: None,
        ownership: MirOwnershipSummary {
            checker_ordinary_map_owned_get_i32_string_v1: receipts,
            checker_ordinary_map_owned_any_v2_map_root_get_any_caller_receipt: None,
            ..MirOwnershipSummary::default()
        },
    };

    assert!(function.validate().is_ok(), "{:?}", function.validate());
    let mut forged_result = function.clone();
    forged_result
        .values
        .get_mut(&getter_result)
        .expect("getter result value")
        .ty = result_string_ty.clone();
    forged_result
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1[1]
        .result_type = Some(result_string_ty.clone());
    assert!(
        forged_result.validate().is_err(),
        "GetI32 must reject a forged Result<string, i32> result shape"
    );

    let mut get_string_function = function.clone();
    let old_instruction = get_string_function
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1[1]
        .instruction
        .clone();
    let string_get_instruction = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_get_i32_string_v1_get_string:{root}:pget"
    ))
    .expect("GetString instruction identity");
    {
        let receipt = &mut get_string_function
            .ownership
            .checker_ordinary_map_owned_get_i32_string_v1[1];
        receipt.kind = K::GetString;
        receipt.result_type = Some(result_string_ty.clone());
        receipt.instruction = string_get_instruction.clone();
    }
    for instruction in get_string_function
        .blocks
        .values_mut()
        .flat_map(|block| &mut block.instructions)
    {
        if instruction.id == old_instruction {
            instruction.id = string_get_instruction.clone();
            instruction.kind = MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString {
                result: getter_result.clone(),
                root: root.clone(),
                key: key.clone(),
            };
        }
    }
    get_string_function
        .values
        .get_mut(&getter_result)
        .expect("GetString result value")
        .ty = result_string_ty.clone();
    assert!(
        get_string_function.validate().is_ok(),
        "canonical GetString receipt and instruction must validate"
    );

    let mut forged_string_result = get_string_function.clone();
    forged_string_result
        .values
        .get_mut(&getter_result)
        .expect("GetString result value")
        .ty = result_ty.clone();
    forged_string_result
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1[1]
        .result_type = Some(result_ty.clone());
    assert!(
        forged_string_result.validate().is_err(),
        "GetString must reject a forged Result<i32, i32> result shape"
    );

    let forge_kind = |function: &mut MirFunction, kind: K| {
        let old_instruction = function
            .ownership
            .checker_ordinary_map_owned_get_i32_string_v1[1]
            .instruction
            .clone();
        let new_instruction = MirInstructionId::new(format!(
            "inst:ordinary_map_owned_get_i32_string_v1_{}:{root}:pget",
            kind.as_str()
        ))
        .expect("forged getter instruction identity");
        {
            let receipt = &mut function
                .ownership
                .checker_ordinary_map_owned_get_i32_string_v1[1];
            receipt.kind = kind;
            receipt.instruction = new_instruction.clone();
        }
        for instruction in function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
        {
            if instruction.id == old_instruction {
                instruction.id = new_instruction.clone();
            }
        }
    };
    let mut forged_i32_kind = function.clone();
    forge_kind(&mut forged_i32_kind, K::GetString);
    assert!(
        forged_i32_kind.validate().is_err(),
        "GetI32 instruction must reject a forged GetString Checker kind"
    );
    let mut forged_string_kind = get_string_function.clone();
    forge_kind(&mut forged_string_kind, K::GetI32);
    assert!(
        forged_string_kind.validate().is_err(),
        "GetString instruction must reject a forged GetI32 Checker kind"
    );

    assert!(function.canonical_text().contains(
        "ordinary_map_owned_get_i32_string_v1_get_i32 expr:pget <- local:root[expr:node:key]"
    ));
    let getter = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .find(|instruction| {
            instruction
                .id
                .as_str()
                .starts_with("inst:ordinary_map_owned_get_i32_string_v1_get_i32:")
        })
        .expect("typed owned-get instruction");
    assert_eq!(mir_instruction_value_uses(&getter.kind), [root, key]);
}

#[test]
fn canonical_ownership_summary_reports_map_root_get_any_caller_receipt() {
    use crate::core::ir::ResolvedLocalId;
    use crate::core::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerProfileVersion as CallerProfile;
    use crate::core::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerReceipt as CallerReceipt;

    let node = |value: &str| NodeId(value.to_owned());
    let local = |value: &str| ResolvedLocalId(node(value));
    let receipt = CallerReceipt {
        profile_version: CallerProfile::V1,
        producer_owner: node("function:map_root_producer"),
        producer_map_set_operation: node("operation:producer_set"),
        producer_return_value: node("expression:producer_return"),
        producer_final_local: local("local:producer_root"),
        producer_final_binding_point: node("point:producer_root_binding"),
        logical_root: crate::core::ownership::ResourceId(node("resource:logical_root")),
        caller_owner: node("function:get_any_caller"),
        call_node: node("expression:producer_call"),
        call_target: node("function:map_root_producer"),
        caller_root_local: local("local:caller_root"),
        caller_root_binding_point: node("point:caller_root_binding"),
        caller_root_type: types::ordinary_map_owned_any_v2_type_id(),
        get_any_node: node("expression:get_any"),
        get_any_receiver_node: node("expression:get_any_receiver"),
        get_any_key_node: node("expression:get_any_key"),
        pair_local: local("local:get_any_pair"),
        pair_binding_point: node("point:pair_binding"),
        pair_type: types::ordinary_map_owned_any_v2_get_result_type_id(),
        unpack_projection: node("projection:pair_string"),
        unpack_cast: node("expression:unpack_string"),
        unpack_local: local("local:unpacked_string"),
        unpack_binding_point: node("point:string_binding"),
        string_type: crate::core::ir::primitive_type_id(PrimitiveType::String),
        pair_drop_point: node("point:pair_drop"),
        root_drop_point: node("point:root_drop"),
        string_drop_point: node("point:string_drop"),
        string_drop_place_index: 0,
        caller_return: node("expression:caller_return"),
        caller_return_type: crate::core::ir::primitive_type_id(PrimitiveType::I32),
        caller_return_literal: crate::core::ir::ResolvedLiteral::Int(0),
    };
    let canonical = MirOwnershipSummary {
        checker_ordinary_map_owned_any_v2_map_root_get_any_caller_receipt: Some(Box::new(receipt)),
        ..MirOwnershipSummary::default()
    }
    .canonical_text();

    assert!(
        canonical.contains("checker_ordinary_map_owned_any_v2_map_root_get_any_caller receipt=")
    );
}

#[test]
fn actor_execution_domain_runtime_helpers_are_reserved_source_ffi_symbols() {
    let checked = checked_program(
        r#"
extern "C" { func mimi_mir_actor_domain_test_helper(value: i64) -> i64; }
func main() -> i64 { mimi_mir_actor_domain_test_helper(1 as i64) }
"#,
    );
    let errors = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("source imports must not claim ActorExecutionDomain runtime symbols");
    assert!(
        errors.to_diagnostics().iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("FFI symbol is not manifest-safe")
                && diagnostic
                    .message
                    .contains("reserved Canonical MIR ActorExecutionDomain runtime helper")
        }),
        "{errors:?}"
    );
}

#[test]
fn canonical_ffi_unit_endpoint_requires_complete_shape() {
    let checked = checked_program(
        r#"
extern "C" {
    func noop();
}
func main() -> i32 {
    noop();
    0
}
"#,
    );
    let catalog = types::MirTypeCatalog::from_checked_program(&checked)
        .expect("unit-return extern must materialize a MIR type catalog");
    let unit_id = checked
        .resolved_types()
        .iter()
        .find_map(|(id, ty)| {
            matches!(ty, ResolvedType::Primitive(PrimitiveType::Unit)).then_some(id.clone())
        })
        .expect("unit TypeDesc must be present");
    let descriptor = catalog.get(&unit_id).expect("unit TypeDesc entry");
    assert!(descriptor.is_canonical_ffi_unit());

    let mut forged = descriptor.clone();
    forged.ownership = MirOwnership::Move;
    assert!(!forged.is_canonical_ffi_unit());

    let mut forged = descriptor.clone();
    forged.kind = MirTypeKind::Nominal;
    assert!(!forged.is_canonical_ffi_unit());

    let mut forged = descriptor.clone();
    forged.glue.move_out = MirGlueKind::Unsupported;
    assert!(!forged.is_canonical_ffi_unit());

    let mut forged = descriptor.clone();
    forged.needs_drop_glue = true;
    assert!(!forged.is_canonical_ffi_unit());

    let mut forged = descriptor.clone();
    forged.session_protocol = Some(unit_id.clone());
    assert!(!forged.is_canonical_ffi_unit());

    let mut forged = descriptor.clone();
    forged.drop_plan = Some(types::MirDropGluePlan { fields: Vec::new() });
    assert!(!forged.is_canonical_ffi_unit());
}

#[test]
fn canonical_copy_scalar_requires_complete_shape() {
    let checked = checked_program("func main() -> i64 { 0 }");
    let catalog = types::MirTypeCatalog::from_checked_program(&checked)
        .expect("scalar-returning function must materialize a MIR type catalog");
    let i64_id = checked
        .resolved_types()
        .iter()
        .find_map(|(id, ty)| {
            matches!(ty, ResolvedType::Primitive(PrimitiveType::I64)).then_some(id.clone())
        })
        .expect("i64 TypeDesc must be present");
    let descriptor = catalog.get(&i64_id).expect("i64 TypeDesc entry");
    assert!(descriptor.is_canonical_copy_scalar(false));
    assert!(descriptor.is_canonical_ffi_scalar());
    assert!(catalog.validate_copy_scalar(&i64_id).is_ok());

    let mut forged = descriptor.clone();
    forged.kind = MirTypeKind::Nominal;
    assert!(!forged.is_canonical_copy_scalar(false));

    let mut forged = descriptor.clone();
    forged.abi = crate::core::mir::types::MirAbiClass::Integer {
        bits: 32,
        signed: true,
    };
    assert!(!forged.is_canonical_copy_scalar(false));

    let mut forged = descriptor.clone();
    forged.glue.drop = MirGlueKind::Unsupported;
    assert!(!forged.is_canonical_copy_scalar(false));

    let mut forged = descriptor.clone();
    forged.needs_drop_glue = true;
    assert!(!forged.is_canonical_copy_scalar(false));

    let mut forged = descriptor.clone();
    forged.session_protocol = Some(i64_id.clone());
    assert!(!forged.is_canonical_copy_scalar(false));

    let mut forged = descriptor.clone();
    forged.drop_plan = Some(types::MirDropGluePlan { fields: Vec::new() });
    assert!(!forged.is_canonical_copy_scalar(false));

    let mut forged = descriptor.clone();
    forged.variant_drop_plan = Some(Vec::new());
    assert!(!forged.is_canonical_copy_scalar(false));
}

#[test]
fn canonical_ffi_identity_ignores_diagnostic_span() {
    let checked = checked_program(
        r#"
extern "C" { func foreign(value: i64) -> i64; }
func main() -> i64 { foreign(1 as i64) }
"#,
    );
    let mut program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("scalar FFI MIR");
    let before = program.route_receipt("test-v1");

    let mut receipts = program.ffi_calls().clone();
    receipts
        .values_mut()
        .next()
        .expect("scalar FFI receipt")
        .span = crate::span::Span::new(91, 17, 93, 29).with_source(crate::span::SourceId::new(99));
    program.replace_ffi_calls_for_test_only(receipts);
    let after = program.route_receipt("test-v1");

    assert_eq!(before.mir_digest, after.mir_digest);
    assert_eq!(before.ffi_digest, after.ffi_digest);
}

#[test]
fn canonical_ffi_endpoint_predicate_rejects_forged_metadata() {
    let checked = checked_program("func main() -> i64 { 0 }");
    let catalog = types::MirTypeCatalog::from_checked_program(&checked)
        .expect("scalar-returning function must materialize a MIR type catalog");
    let i64_id = checked
        .resolved_types()
        .iter()
        .find_map(|(id, ty)| {
            matches!(ty, ResolvedType::Primitive(PrimitiveType::I64)).then_some(id.clone())
        })
        .expect("i64 TypeDesc must be present");
    assert!(catalog.is_canonical_ffi_endpoint(&i64_id, false));

    let unit_id = checked
        .resolved_types()
        .iter()
        .find_map(|(id, ty)| {
            matches!(ty, ResolvedType::Primitive(PrimitiveType::Unit)).then_some(id.clone())
        })
        .expect("unit TypeDesc must be present");
    assert!(!catalog.is_canonical_ffi_endpoint(&unit_id, false));
    assert!(catalog.is_canonical_ffi_endpoint(&unit_id, true));

    let descriptor = catalog.get(&i64_id).expect("i64 TypeDesc entry");
    for mutation in 0..4 {
        let mut forged = descriptor.clone();
        match mutation {
            0 => forged.session_protocol = Some(i64_id.clone()),
            1 => forged.needs_drop_glue = true,
            2 => forged.drop_plan = Some(types::MirDropGluePlan { fields: Vec::new() }),
            3 => forged.variant_drop_plan = Some(Vec::new()),
            _ => unreachable!(),
        }
        let mut forged_catalog = catalog.clone();
        forged_catalog.replace_for_test_only(i64_id.clone(), forged);
        assert!(!forged_catalog.is_canonical_ffi_endpoint(&i64_id, false));
    }
}

#[test]
fn ffi_record_verifier_receipt_is_verifier_only_and_binds_repr_c_field_order() {
    let checked = checked_program(
        r#"
#[repr(C)] type Pair { low: i32, high: i64 }
extern "C" { func foreign(value: Pair) -> Pair requires: value.low >= 0; }
func caller(value: Pair) -> Pair {
    requires: value.low >= 0
    foreign(value)
}
func main() -> i64 { 0 }
"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR retains a checker-sealed record FFI receipt");
    let (instruction, receipt) = program.ffi_calls().iter().next().expect("FFI call receipt");
    let function = program
        .functions()
        .get(&receipt.caller)
        .expect("FFI caller body");
    let verifier_errors = validate_ffi_call_contract_verifier_record_receipt(
        program.type_catalog(),
        function,
        instruction,
        &receipt.callee,
        receipt.result.as_ref(),
        &receipt.arguments,
        receipt,
    );
    assert!(verifier_errors.is_empty(), "{verifier_errors:?}");
    let execution_errors = validate_ffi_call_contract_receipt(
        program.type_catalog(),
        function,
        instruction,
        &receipt.callee,
        receipt.result.as_ref(),
        &receipt.arguments,
        receipt,
    );
    assert!(
        !execution_errors.is_empty(),
        "record endpoints must remain outside ScalarFfi execution admission"
    );

    let mut forged_variadic = receipt.clone();
    forged_variadic.variadic = true;
    let variadic_errors = validate_ffi_call_contract_verifier_record_receipt(
        program.type_catalog(),
        function,
        instruction,
        &receipt.callee,
        receipt.result.as_ref(),
        &receipt.arguments,
        &forged_variadic,
    );
    assert!(
        variadic_errors
            .iter()
            .any(|error| error.contains("does not admit variadic declarations")),
        "variadic record receipts must be rejected explicitly: {variadic_errors:?}"
    );

    let record_id = &receipt.parameter_types[0];
    let original = program
        .type_catalog()
        .get(record_id)
        .expect("record TypeDesc")
        .clone();
    assert!(program
        .type_catalog()
        .is_canonical_ffi_verifier_record(record_id));
    let original_abi_text = original.abi_canonical_text();
    assert!(original_abi_text.contains("repr_c=true"));
    assert!(original_abi_text.contains("name=low"));
    assert!(original_abi_text.contains("name=high"));

    let validate_forged = |descriptor: types::MirTypeDesc| {
        let mut catalog = program.type_catalog().clone();
        catalog.replace_for_test_only(record_id.clone(), descriptor);
        validate_ffi_call_contract_verifier_record_receipt(
            &catalog,
            function,
            instruction,
            &receipt.callee,
            receipt.result.as_ref(),
            &receipt.arguments,
            receipt,
        )
    };

    let mut forged_repr = original.clone();
    forged_repr
        .native_record_abi
        .as_mut()
        .expect("native declaration identity")
        .repr_c = false;
    assert_ne!(forged_repr.abi_canonical_text(), original_abi_text);
    assert!(
        !validate_forged(forged_repr).is_empty(),
        "tampered repr(C) identity must be rejected"
    );

    let mut forged_order = original.clone();
    forged_order
        .native_record_abi
        .as_mut()
        .expect("native declaration identity")
        .fields
        .reverse();
    assert_ne!(forged_order.abi_canonical_text(), original_abi_text);
    assert!(
        !validate_forged(forged_order).is_empty(),
        "tampered native declaration order must be rejected"
    );

    let mut forged_layout_order = original;
    let MirLayout::Record { fields, .. } = &mut forged_layout_order.layout else {
        panic!("Pair TypeDesc must have record layout");
    };
    fields.reverse();
    assert!(
        !validate_forged(forged_layout_order).is_empty(),
        "tampered canonical layout order must disagree with the native receipt"
    );
}

#[test]
fn ffi_record_verifier_catalog_rejects_non_repr_generic_string_tuple_and_enum_shapes() {
    let checked = checked_program(
        r#"
#[repr(C)] type Good { value: i32, tail: i64 }
type Plain { value: i32 }
#[repr(C)] type Generic<T> { value: T }
#[repr(C)] type WithString { value: i32, label: string }
#[repr(C)] type WithF32 { value: f32 }
type Choice { Empty, Value(i32) }
func consume(good: Good, plain: Plain, generic: Generic<i32>, with_string: WithString, with_f32: WithF32, choice: Choice, pair: (i32, i64)) -> i64 {
    drop(plain);
    drop(generic);
    drop(with_string);
    drop(with_f32);
    drop(choice);
    drop(pair);
    good.tail
}
func main() -> i64 { 0 }
"#,
    );
    let catalog = types::MirTypeCatalog::from_checked_program(&checked).expect("MIR type catalog");
    let record_id = |name: &str| {
        catalog
            .iter()
            .find_map(|(id, descriptor)| match &descriptor.layout {
                MirLayout::Record { nominal, .. } if nominal.as_str().ends_with(name) => {
                    Some(id.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{name} record TypeDesc"))
    };
    let good = record_id("Good");
    assert!(catalog.is_canonical_ffi_verifier_record(&good));
    for (name, reason) in [
        ("Plain", "missing repr(C)"),
        ("Generic", "generic binder"),
        ("WithString", "managed String field"),
        ("WithF32", "f32 outside the verifier leaf domain"),
    ] {
        let id = record_id(name);
        assert!(
            !catalog.is_canonical_ffi_verifier_record(&id),
            "{name} must be outside the record proof profile ({reason})"
        );
    }
    assert!(catalog.iter().any(|(_, descriptor)| {
        descriptor.kind == MirTypeKind::Nominal
            && matches!(
                &descriptor.layout,
                MirLayout::Enum { nominal, .. } if nominal.as_str().ends_with("Choice")
            )
    }));
    let choice = catalog
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == MirTypeKind::Nominal
                && matches!(
                    &descriptor.layout,
                    MirLayout::Enum { nominal, .. } if nominal.as_str().ends_with("Choice")
                ))
            .then_some(id.clone())
        })
        .expect("enum TypeDesc");
    assert!(!catalog.is_canonical_ffi_verifier_record(&choice));
    let tuple = checked
        .resolved_types()
        .iter()
        .find_map(|(id, ty)| matches!(ty, ResolvedType::Tuple(_)).then_some(id.clone()))
        .expect("tuple TypeDesc identity");
    assert!(catalog.get(&tuple).is_some());
    assert!(!catalog.is_canonical_ffi_verifier_record(&tuple));
}

#[test]
fn shared_ffi_receipt_gate_rejects_forged_endpoint_metadata() {
    let checked = checked_program(
        r#"
extern "C" { func foreign(value: i64) -> i64; }
func main() -> i64 { foreign(1 as i64) }
"#,
    );
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("scalar FFI MIR");
    let (instruction, receipt) = program
        .ffi_calls()
        .iter()
        .next()
        .expect("scalar FFI receipt");
    let function = program
        .functions()
        .get(&receipt.caller)
        .expect("caller MIR");
    let call = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find(|candidate| candidate.id == *instruction)
        .expect("extern call instruction");
    let MirInstructionKind::Call {
        callee: ResolvedCallee::Extern(callee),
        result,
        arguments,
        ..
    } = &call.kind
    else {
        panic!("fixture must contain an extern call");
    };
    let scalar_id = receipt
        .parameter_types
        .first()
        .expect("parameter TypeDesc")
        .clone();
    let descriptor = program
        .type_catalog()
        .get(&scalar_id)
        .expect("scalar TypeDesc")
        .clone();

    for mutation in 0..4 {
        let mut forged = descriptor.clone();
        match mutation {
            0 => forged.session_protocol = Some(scalar_id.clone()),
            1 => forged.needs_drop_glue = true,
            2 => forged.drop_plan = Some(types::MirDropGluePlan { fields: Vec::new() }),
            3 => forged.variant_drop_plan = Some(Vec::new()),
            _ => unreachable!(),
        }
        let mut catalog = program.type_catalog().clone();
        catalog.replace_for_test_only(scalar_id.clone(), forged);
        let errors = validate_ffi_call_contract_receipt(
            &catalog,
            function,
            instruction,
            callee,
            result.as_ref(),
            arguments,
            receipt,
        );
        assert!(
            errors.iter().any(|error| {
                error.contains("declaration parameter 0 TypeDesc")
                    && error.contains("complete scalar endpoint contract")
            }),
            "unexpected forged receipt diagnostics: {errors:?}"
        );
    }
}

#[test]
fn materialized_call_result_presence_rejects_noncopy_unit() {
    let checked = checked_program(
        r#"
func noop() { }
func main() { noop() }
"#,
    );
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("unit-return function must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let caller = program.functions().get(&owner).expect("main MIR");
    let (callee, target_owner) = caller
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                ..
            } if callee.0 == "function:noop" => Some((
                crate::core::ir::ResolvedCallee::Function(callee.clone()),
                crate::core::NodeId(callee.0.clone()),
            )),
            _ => None,
        })
        .expect("direct unit function call");
    let target = program
        .functions()
        .get(&target_owner)
        .expect("unit function target MIR");
    let unit_id = target.result.clone();
    let descriptor = program
        .type_catalog()
        .get(&unit_id)
        .expect("unit TypeDesc entry");
    assert!(descriptor.is_canonical_ffi_unit());

    let mut catalog = program.type_catalog().clone();
    let mut forged = descriptor.clone();
    forged.ownership = MirOwnership::Move;
    catalog.replace_for_test_only(unit_id.clone(), forged);
    let missing_result = crate::core::mir::validate_materialized_call_result_presence(
        &callee, target, None, &catalog,
    );
    assert_eq!(
        missing_result,
        vec![format!(
            "non-unit callee '{}' has no MIR result value",
            unit_id.as_str()
        )]
    );
}

#[test]
fn scalar_ffi_conversion_kind_is_directional_and_closed() {
    use crate::core::mir::types::MirAbiClass;

    let i32_abi = MirAbiClass::Integer {
        bits: 32,
        signed: true,
    };
    let i64_abi = MirAbiClass::Integer {
        bits: 64,
        signed: true,
    };
    let f64_abi = MirAbiClass::Float { bits: 64 };

    let argument_pairs = [(i32_abi, i64_abi), (i32_abi, f64_abi), (i64_abi, f64_abi)];
    for (from, to) in argument_pairs {
        let conversion = MirFfiAbiConversion { from, to };
        assert!(conversion.is_supported_argument());
        assert!(!conversion.is_supported_result());
        assert!(conversion.kind().is_some());
    }

    let result_pairs = [(i64_abi, i32_abi), (f64_abi, i32_abi), (f64_abi, i64_abi)];
    for (from, to) in result_pairs {
        let conversion = MirFfiAbiConversion { from, to };
        assert!(!conversion.is_supported_argument());
        assert!(conversion.is_supported_result());
        assert!(conversion.kind().is_some());
    }

    let narrow_result = MirFfiAbiConversion {
        from: i64_abi,
        to: i32_abi,
    };
    assert!(narrow_result.kind().is_some());
    assert!(MirFfiAbiConversion {
        from: f64_abi,
        to: f64_abi,
    }
    .is_supported_argument());
    assert!(!MirFfiAbiConversion {
        from: MirAbiClass::Unit,
        to: MirAbiClass::Unit,
    }
    .is_supported_argument());
    assert!(MirFfiAbiConversion {
        from: MirAbiClass::Unit,
        to: MirAbiClass::Unit,
    }
    .is_supported_result());
    assert!(MirFfiAbiConversion {
        from: MirAbiClass::Integer {
            bits: 32,
            signed: false,
        },
        to: i64_abi,
    }
    .kind()
    .is_none());
}

#[test]
fn scalar_ffi_float_result_bounds_are_shared_and_half_open() {
    use crate::core::mir::MirFfiAbiConversion;

    let (i32_lower, i32_upper) =
        MirFfiAbiConversion::float_to_signed_integer_bounds(32).expect("i32 bounds");
    assert_eq!(i32_lower, i32::MIN as f64);
    assert_eq!(i32_upper, 2_147_483_648.0);

    let (i64_lower, i64_upper) =
        MirFfiAbiConversion::float_to_signed_integer_bounds(64).expect("i64 bounds");
    assert_eq!(i64_lower, -9_223_372_036_854_775_808.0);
    assert_eq!(i64_upper, 9_223_372_036_854_775_808.0);
    assert!(MirFfiAbiConversion::float_to_signed_integer_bounds(16).is_none());
}

#[test]
fn scalar_ffi_integer_narrow_bounds_are_shared_and_closed() {
    use crate::core::mir::MirFfiAbiConversion;

    assert_eq!(
        MirFfiAbiConversion::signed_integer_narrow_bounds(32),
        Some((i32::MIN as i64, i32::MAX as i64))
    );
    assert_eq!(
        MirFfiAbiConversion::signed_integer_narrow_bounds(64),
        Some((i64::MIN, i64::MAX))
    );
    assert!(MirFfiAbiConversion::signed_integer_narrow_bounds(16).is_none());
}

#[test]
fn scalar_ffi_result_conversion_factory_is_inverse_and_closed() {
    use crate::core::mir::reference::MirProgram;
    use crate::core::mir::types::MirAbiClass;

    let checked = checked_program(
        r#"
extern "C" { func result_factory(value: i64) -> i64; }
func i32_marker(value: i32) -> i32 { value }
func main() -> i64 { result_factory(7) }
"#,
    );
    let program = MirProgram::from_checked_program(&checked)
        .expect("result conversion factory fixture materialization");
    let receipt = program
        .ffi_calls()
        .values()
        .next()
        .expect("result conversion factory receipt");
    let actual = receipt
        .result
        .as_ref()
        .and_then(|value| program.functions()[&receipt.caller].values.get(value))
        .map(|value| value.ty.clone())
        .expect("result conversion factory actual type");
    let declared = receipt.result_type.clone();
    assert_eq!(
        crate::core::mir::MirFfiAbiConversion::for_result(
            program.type_catalog(),
            &actual,
            &declared,
        )
        .expect("same ABI result identity"),
        crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            to: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
        }
    );
    let i32_type = program
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi
                == MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
            .then(|| id.clone())
        })
        .expect("i32 marker TypeDesc");
    assert_eq!(
        crate::core::mir::MirFfiAbiConversion::for_result(
            program.type_catalog(),
            &i32_type,
            &declared,
        ),
        Some(crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            to: MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
        })
    );
    assert_eq!(
        crate::core::mir::MirFfiAbiConversion::for_result(
            program.type_catalog(),
            &declared,
            &i32_type,
        ),
        None,
        "result widening remains outside the closed scalar result contract"
    );
}

#[test]
fn materializes_terminal_session_close_with_backend_neutral_receipt() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_close.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("terminal SessionChan close must lower to canonical MIR");
    let owner = crate::core::NodeId("function:close_endpoint".into());
    let function = program.functions().get(&owner).expect("close_endpoint MIR");
    let session_calls = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::SessionCall {
                operation,
                endpoint,
                contract: Some(contract),
                ..
            } => Some((*operation, endpoint.clone(), contract.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        session_calls.len(),
        1,
        "close must be one explicit SessionCall"
    );
    let (operation, endpoint, contract) = &session_calls[0];
    assert_eq!(
        *operation,
        crate::core::mir::types::MirSessionOperation::Close
    );
    let endpoint_ty = function
        .values
        .get(endpoint)
        .expect("endpoint value")
        .ty
        .clone();
    assert_eq!(contract.endpoint_ty, endpoint_ty);
    assert!(contract.payload_ty.is_none());
    assert!(contract.terminal);
    assert_eq!(contract.after.as_str(), "closed");
    program
        .type_catalog()
        .validate_session_channel(&endpoint_ty)
        .expect("SessionChan TypeDesc contract");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(17)],
        )
        .expect("reference SessionCall execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));

    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume the same canonical SessionCall");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume the same canonical SessionCall");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume the same canonical SessionCall");
}

#[test]
fn materializes_session_open_as_canonical_builtin_with_backend_neutral_oracle() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_open.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("session_open must lower to canonical MIR");
    let owner = crate::core::NodeId("function:open_close".into());
    let function = program.functions().get(&owner).expect("open_close MIR");
    let (result, kind) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::BuiltinCall {
                result,
                kind: crate::core::mir::types::MirBuiltinKind::SessionOpen,
                arguments,
                ..
            } => Some((result.clone(), arguments.len())),
            _ => None,
        })
        .expect("session_open builtin instruction");
    assert_eq!(kind, 0, "session_open has no value arguments");
    let result_ty = function
        .values
        .get(&result)
        .expect("session_open result value")
        .ty
        .clone();
    program
        .type_catalog()
        .validate_session_channel(&result_ty)
        .expect("session_open result must be transfer-only SessionChan");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&owner, &[])
        .expect("reference session_open/session_close execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));

    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume canonical SessionOpen");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume canonical SessionOpen");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume canonical SessionOpen");
}

#[test]
fn rejects_forged_session_open_result_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_open.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical session_open MIR");
    let owner = crate::core::NodeId("function:open_close".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("open_close MIR");
    let result = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::BuiltinCall {
                kind: crate::core::mir::types::MirBuiltinKind::SessionOpen,
                result,
                ..
            } => Some(result.clone()),
            _ => None,
        })
        .expect("session_open result");
    let scalar_i32 = program
        .type_catalog()
        .iter()
        .find_map(|(ty, descriptor)| {
            (descriptor.abi
                == crate::core::mir::types::MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
            .then(|| ty.clone())
        })
        .expect("canonical i32 TypeDesc");
    function.values.get_mut(&result).expect("result value").ty = scalar_i32;
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("non-SessionChan SessionOpen result must be rejected");
    assert!(errors.iter().any(|error| {
        error.message.contains("canonical SessionChan contract")
            || error.message.contains("SessionChan endpoint type")
    }));
}

#[test]
fn materializes_plain_session_pair_as_copy_tuple_for_all_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_pair.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("plain session_pair must lower to canonical MIR");
    let owner = crate::core::NodeId("function:pair_order".into());
    let function = program.functions().get(&owner).expect("pair_order MIR");
    let result = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::BuiltinCall {
                result,
                kind: crate::core::mir::types::MirBuiltinKind::SessionPair,
                arguments,
                ..
            } => {
                assert!(arguments.is_empty(), "session_pair has no value arguments");
                Some(result.clone())
            }
            _ => None,
        })
        .expect("session_pair builtin instruction");
    let result_ty = function
        .values
        .get(&result)
        .expect("session_pair result value")
        .ty
        .clone();
    program
        .type_catalog()
        .validate_plain_session_pair(&result_ty)
        .expect("session_pair result Copy tuple contract");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&owner, &[])
        .expect("reference session_pair execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));

    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume canonical SessionPair");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume canonical SessionPair");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume canonical SessionPair");
}

#[test]
fn rejects_forged_session_pair_result_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_pair.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical session_pair MIR");
    let owner = crate::core::NodeId("function:pair_order".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("pair_order MIR");
    let result = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::BuiltinCall {
                kind: crate::core::mir::types::MirBuiltinKind::SessionPair,
                result,
                ..
            } => Some(result.clone()),
            _ => None,
        })
        .expect("session_pair result");
    let scalar_i32 = program
        .type_catalog()
        .iter()
        .find_map(|(ty, descriptor)| {
            (descriptor.abi
                == crate::core::mir::types::MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
            .then(|| ty.clone())
        })
        .expect("canonical i32 TypeDesc");
    function.values.get_mut(&result).expect("result value").ty = scalar_i32;
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("non-tuple SessionPair result must be rejected");
    assert!(errors.iter().any(|error| {
        error.message.contains("canonical session-pair contract")
            || error.message.contains("two-field tuple layout")
    }));
}

#[test]
fn materializes_typed_session_pair_direct_binding_for_all_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_typed_session_pair_bind.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("typed SessionPair direct binding must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = program
        .functions()
        .get(&owner)
        .expect("typed pair main MIR");
    let pair = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::SessionPairBind {
                lo,
                hi,
                contract: Some(contract),
            } => Some((lo.clone(), hi.clone(), contract.clone())),
            _ => None,
        })
        .expect("typed pair binding instruction");
    let (lo, hi, contract) = pair;
    let lo_ty = function.values.get(&lo).expect("lo value").ty.clone();
    let hi_ty = function.values.get(&hi).expect("hi value").ty.clone();
    program
        .type_catalog()
        .validate_session_pair_bind_receipt(&contract.pair_ty, &lo_ty, &hi_ty, &contract)
        .expect("typed pair binding receipt");
    program
        .type_catalog()
        .validate_session_channel(&lo_ty)
        .expect("lo endpoint TypeDesc");
    program
        .type_catalog()
        .validate_session_channel(&hi_ty)
        .expect("hi endpoint TypeDesc");
    assert_eq!(
        program
            .type_catalog()
            .get(&lo_ty)
            .and_then(|descriptor| descriptor.session_protocol.as_ref()),
        Some(&contract.lo_protocol),
        "lo endpoint protocol identity must be carried by its TypeDesc receipt"
    );
    assert_eq!(
        program
            .type_catalog()
            .get(&hi_ty)
            .and_then(|descriptor| descriptor.session_protocol.as_ref()),
        Some(&contract.hi_protocol),
        "hi endpoint protocol identity must be carried by its TypeDesc receipt"
    );

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&owner, &[])
        .expect("reference typed session_pair execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));

    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume canonical typed SessionPair binding");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume canonical typed SessionPair binding");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume canonical typed SessionPair binding");
}

#[test]
fn rejects_typed_session_pair_non_binding_pattern_before_consumers() {
    let checked = checked_program(
        r#"
session S = end
func main() -> i32 {
    let pair = session_pair::<S>()
    drop(pair)
    0
}
"#,
    );
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("typed SessionPair aggregate binding must remain fail-closed");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("direct endpoint bindings")
            || rendered.contains("canonical aggregate")
            || rendered.contains("glue"),
        "typed SessionPair rejection must identify the unsupported aggregate shape: {rendered}"
    );
}

#[test]
fn executes_typed_session_pair_send_recv_roundtrip_across_mir_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_typed_session_pair_send_recv.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("typed SessionPair send/recv roundtrip must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = program.functions().get(&owner).expect("roundtrip main MIR");
    let session_calls = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::SessionCall {
                operation,
                contract: Some(contract),
                ..
            } => Some((*operation, contract.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(session_calls.len(), 4);
    assert_eq!(
        session_calls
            .iter()
            .filter(|(operation, _)| {
                *operation == crate::core::mir::types::MirSessionOperation::Send
            })
            .count(),
        1
    );
    assert_eq!(
        session_calls
            .iter()
            .filter(|(operation, _)| {
                *operation == crate::core::mir::types::MirSessionOperation::Recv
            })
            .count(),
        1
    );
    assert!(session_calls.iter().all(|(operation, contract)| {
        contract.terminal == (*operation == crate::core::mir::types::MirSessionOperation::Close)
    }));
    assert_eq!(
        session_calls
            .iter()
            .filter(|(operation, _)| {
                *operation == crate::core::mir::types::MirSessionOperation::Close
            })
            .count(),
        2
    );

    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&owner, &[])
        .expect("reference pair send/recv execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(41)
    );

    let bytecode = crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume the canonical pair roundtrip MIR");
    let value = crate::interp::bytecode::BytecodeVM::new(bytecode)
        .run_value()
        .expect("bytecode pair send/recv execution");
    assert!(matches!(value, crate::interp::Value::Int(41)));

    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume the canonical pair roundtrip MIR");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume the canonical pair roundtrip MIR");
}

#[test]
fn rejects_forged_typed_session_pair_roundtrip_receipt_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_typed_session_pair_send_recv.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical pair roundtrip MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("roundtrip main MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::SessionPairBind { .. }))
        .expect("typed pair binding instruction");
    let MirInstructionKind::SessionPairBind { contract, .. } = &mut instruction.kind else {
        unreachable!();
    };
    let receipt = contract.as_mut().expect("typed pair receipt");
    receipt.lo_ty = receipt.hi_ty.clone();
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("forged pair receipt must be rejected before consumers");
    assert!(errors.iter().any(|error| {
        error.message.contains("typed session_pair binding receipt")
            || error.message.contains("endpoint identities")
    }));
}

#[test]
fn rejects_forged_typed_session_pair_protocol_receipt_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_typed_session_pair_send_recv.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical pair roundtrip MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("roundtrip main MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::SessionPairBind { .. }))
        .expect("typed pair binding instruction");
    let MirInstructionKind::SessionPairBind { contract, .. } = &mut instruction.kind else {
        unreachable!();
    };
    let receipt = contract.as_mut().expect("typed pair receipt");
    receipt.lo_protocol = receipt.hi_protocol.clone();
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("forged protocol receipt must be rejected before consumers");
    assert!(errors.iter().any(|error| {
        error.message.contains("typed session_pair binding receipt")
            || error.message.contains("protocol identity")
    }));
}

#[test]
fn materializes_generic_tuple_copy_projection_for_all_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_native_generic_tuple_projection.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic tuple projection must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarTupleProjection { .. }
            )
        })
        .expect("generic tuple projection instance");
    let MirGenericInstanceContract::ScalarTupleProjection { contract } = &instance.contract else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.arity, 2);
    assert_eq!(contract.field_index, 0);
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized tuple projection target");
    assert_eq!(contract.field_ty, target.result);
    assert!(target.canonical_text().contains("project"));
    let parameter = target.parameters.first().expect("tuple parameter");
    let parameter_ty = target
        .values
        .get(parameter)
        .expect("tuple parameter value")
        .ty
        .clone();
    program
        .type_catalog()
        .validate_flat_copy_tuple(&parameter_ty)
        .expect("specialized tuple must be flat Copy");

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic tuple projection execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume generic tuple projection MIR");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume generic tuple projection MIR");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume generic tuple projection MIR");
}

#[test]
fn rejects_generic_tuple_projection_outside_two_element_copy_island() {
    let checked = checked_program(
        r#"
func first<T>(pair: (T, T, T)) -> T { pair.0 }
func main() -> i32 { first((1, 2, 3)) }
"#,
    );
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("three-element generic tuple projection must remain fail-closed");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("two-element")
            || rendered.contains("generic tuple projection")
            || rendered.contains("scalar contract"),
        "unsupported generic tuple projection must have a stable diagnostic: {rendered}"
    );
}

#[test]
fn materializes_integer_session_send_with_backend_neutral_receipt() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_send.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("integer SessionChan send must lower to canonical MIR");
    let owner = crate::core::NodeId("function:send_once".into());
    let function = program.functions().get(&owner).expect("send_once MIR");
    let session_calls = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::SessionCall {
                operation,
                endpoint,
                payload: Some(payload),
                contract: Some(contract),
                ..
            } => Some((
                *operation,
                endpoint.clone(),
                payload.clone(),
                contract.clone(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        session_calls.len(),
        1,
        "send must be one explicit SessionCall"
    );
    let (operation, endpoint, payload, contract) = &session_calls[0];
    assert_eq!(
        *operation,
        crate::core::mir::types::MirSessionOperation::Send
    );
    let endpoint_ty = function
        .values
        .get(endpoint)
        .expect("endpoint value")
        .ty
        .clone();
    let payload_ty = function
        .values
        .get(payload)
        .expect("payload value")
        .ty
        .clone();
    assert_eq!(contract.endpoint_ty, endpoint_ty);
    assert_eq!(contract.payload_ty.as_ref(), Some(&payload_ty));
    assert!(!contract.terminal);
    assert!(contract.before.as_str().starts_with("!i64"));
    assert_eq!(contract.after.as_str(), "end");
    let payload_conversions = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| matches!(instruction.kind, MirInstructionKind::Convert { .. }))
        .count();
    assert_eq!(
        payload_conversions, 1,
        "literal i32 payload must carry one explicit checker-approved i64 conversion"
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(17)],
        )
        .expect("reference SessionCall execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(17));

    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume the same canonical SessionCall");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume the same canonical SessionCall");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume the same canonical SessionCall");
}

#[test]
fn materializes_integer_session_recv_with_deterministic_queue_oracle() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_recv.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("integer SessionChan recv must lower to canonical MIR");
    let owner = crate::core::NodeId("function:recv_once".into());
    let function = program.functions().get(&owner).expect("recv_once MIR");
    let session_calls = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::SessionCall {
                operation,
                contract: Some(contract),
                payload,
                ..
            } => Some((*operation, contract.clone(), payload.is_some())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        session_calls.len(),
        2,
        "recv followed by close is explicit MIR"
    );
    let (operation, recv_contract, has_payload) = session_calls
        .iter()
        .find(|(operation, _, _)| *operation == crate::core::mir::types::MirSessionOperation::Recv)
        .expect("recv receipt");
    assert_eq!(
        *operation,
        crate::core::mir::types::MirSessionOperation::Recv
    );
    assert!(!has_payload);
    assert!(recv_contract.before.as_str().starts_with("?i64"));
    assert_eq!(recv_contract.after.as_str(), "end");
    assert!(!recv_contract.terminal);
    assert!(recv_contract.payload_ty.is_none());

    let interpreter = crate::core::mir::reference::MirReferenceInterpreter::new(&program);
    let value = interpreter
        .execute_with_session_queues(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(17)],
            &[crate::core::mir::reference::MirSessionQueueInput {
                endpoint: 17,
                values: vec![73],
            }],
        )
        .expect("reference SessionCall recv execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(73));
    let missing = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(17)],
        )
        .expect_err("recv without an explicit queue must fail closed");
    assert!(missing.message.contains("queue is missing or exhausted"));

    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume the same canonical SessionCall");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume the same canonical SessionCall");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume the same canonical SessionCall");
}

#[test]
fn materializes_i32_session_recv_with_range_checked_queue_oracle() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_recv_i32.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("i32 SessionChan recv must lower to canonical MIR");
    let owner = crate::core::NodeId("function:recv_once".into());
    let function = program.functions().get(&owner).expect("recv_once MIR");
    let (result, contract) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::SessionCall {
                operation: crate::core::mir::types::MirSessionOperation::Recv,
                result,
                contract: Some(contract),
                ..
            } => Some((result.clone(), contract.clone())),
            _ => None,
        })
        .expect("i32 recv receipt");
    assert!(contract.before.as_str().starts_with("?i32"));
    assert_eq!(
        function
            .values
            .get(&result)
            .expect("recv result")
            .ty
            .clone(),
        contract.result_ty
    );
    assert_eq!(
        program
            .type_catalog()
            .get(&contract.result_ty)
            .expect("i32 result TypeDesc")
            .abi,
        crate::core::mir::types::MirAbiClass::Integer {
            bits: 32,
            signed: true
        }
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute_with_session_queues(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(17)],
            &[crate::core::mir::reference::MirSessionQueueInput {
                endpoint: 17,
                values: vec![2_147_483_647],
            }],
        )
        .expect("i32 queue value in range");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::Int(2_147_483_647)
    );
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute_with_session_queues(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(17)],
            &[crate::core::mir::reference::MirSessionQueueInput {
                endpoint: 17,
                values: vec![2_147_483_648],
            }],
        )
        .expect_err("i32 queue overflow must trap");
    assert!(error.message.contains("outside the canonical signed range"));
}

#[test]
fn rejects_forged_session_close_receipt_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_close.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical SessionCall");
    let owner = crate::core::NodeId("function:close_endpoint".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("close_endpoint MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::SessionCall { .. }))
        .expect("SessionCall instruction");
    let MirInstructionKind::SessionCall { contract, .. } = &mut instruction.kind else {
        unreachable!();
    };
    contract.as_mut().expect("receipt").after =
        crate::core::SessionResidualId::new("not-closed").expect("test residual");
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("forged SessionCall receipt must be rejected");
    assert!(errors.iter().any(|error| {
        error.message.contains("session_close must be terminal")
            || error.message.contains("SessionCall receipt disagrees")
    }));
}

#[test]
fn rejects_forged_session_send_payload_receipt_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_send.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical SessionCall");
    let owner = crate::core::NodeId("function:send_once".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("send_once MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::SessionCall {
                    operation: crate::core::mir::types::MirSessionOperation::Send,
                    ..
                }
            )
        })
        .expect("SessionCall instruction");
    let MirInstructionKind::SessionCall { contract, .. } = &mut instruction.kind else {
        unreachable!();
    };
    let receipt = contract.as_mut().expect("receipt");
    receipt.payload_ty = Some(receipt.result_ty.clone());
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("forged SessionCall payload receipt must be rejected");
    assert!(errors.iter().any(|error| {
        error.message.contains("session_send payload")
            || error.message.contains("SessionCall payload")
    }));
}

#[test]
fn rejects_forged_session_recv_result_receipt_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_recv.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical SessionCall");
    let owner = crate::core::NodeId("function:recv_once".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("recv_once MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::SessionCall {
                    operation: crate::core::mir::types::MirSessionOperation::Recv,
                    ..
                }
            )
        })
        .expect("SessionCall instruction");
    let MirInstructionKind::SessionCall { contract, .. } = &mut instruction.kind else {
        unreachable!();
    };
    let endpoint_ty = contract.as_ref().expect("receipt").endpoint_ty.clone();
    contract.as_mut().expect("receipt").result_ty = endpoint_ty;
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("forged SessionCall result receipt must be rejected");
    assert!(errors.iter().any(|error| {
        error.message.contains("session_recv result")
            || error.message.contains("SessionCall receipt disagrees")
    }));
}

#[test]
fn ordinary_session_call_materializes_transfer_effect_receipt() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_call_transfer.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("ordinary SessionChan call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:forward".into());
    let function = program.functions().get(&owner).expect("forward MIR");
    let (argument, receipts) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                arguments,
                effect_receipts,
                ..
            } if callee.0 == "function:pass" => {
                Some((arguments[0].clone(), effect_receipts.clone()))
            }
            _ => None,
        })
        .expect("ordinary SessionChan call");
    assert_eq!(receipts.len(), 1);
    let receipt = &receipts[0];
    assert_eq!(receipt.argument_index, 0);
    assert_eq!(
        receipt.kind,
        crate::core::mir::types::MirCallEffectKind::TransferSession
    );
    assert_eq!(
        receipt.argument_ty,
        function.values.get(&argument).expect("call argument").ty
    );
    program
        .type_catalog()
        .validate_session_channel(&receipt.argument_ty)
        .expect("receipt argument TypeDesc");

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(29)],
        )
        .expect("reference ordinary SessionChan call");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(29));
    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume the canonical call receipt");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume the canonical call receipt");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier must consume the canonical call receipt");
}

#[test]
fn missing_session_call_transfer_receipt_is_rejected_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_call_transfer.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("ordinary SessionChan call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:forward".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("forward MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::Call { .. }))
        .expect("ordinary SessionChan call");
    let MirInstructionKind::Call {
        effect_receipts, ..
    } = &mut instruction.kind
    else {
        unreachable!();
    };
    effect_receipts.clear();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("missing SessionChan call receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("SessionChan call argument 0 has no canonical TransferSession effect receipt")
    }));
}

#[test]
fn multi_session_call_materializes_ordered_transfer_effect_receipts() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_call_transfer_multi.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("multi-argument SessionChan call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:forward".into());
    let function = program.functions().get(&owner).expect("forward MIR");
    let (arguments, receipts) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                arguments,
                effect_receipts,
                ..
            } if callee.0 == "function:sink" => Some((arguments.clone(), effect_receipts.clone())),
            _ => None,
        })
        .expect("multi-argument SessionChan call");
    assert_eq!(arguments.len(), 2);
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0].argument_index, 0);
    assert_eq!(receipts[1].argument_index, 1);
    assert!(receipts
        .windows(2)
        .all(|window| { window[0].argument_index < window[1].argument_index }));
    for receipt in &receipts {
        assert_eq!(
            receipt.kind,
            crate::core::mir::types::MirCallEffectKind::TransferSession
        );
        assert_eq!(
            receipt.argument_ty,
            function
                .values
                .get(&arguments[receipt.argument_index])
                .expect("call argument")
                .ty
        );
        program
            .type_catalog()
            .validate_call_effect_contract(receipt)
            .expect("SessionChan effect receipt TypeDesc");
    }
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &owner,
            &[
                crate::core::mir::reference::MirRuntimeValue::Int(11),
                crate::core::mir::reference::MirRuntimeValue::Int(22),
            ],
        )
        .expect("reference multi-argument SessionChan call");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(11));
    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume multi-argument call receipts");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume multi-argument call receipts");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier must consume multi-argument call receipts");
}

#[test]
fn missing_second_multi_session_call_receipt_is_rejected_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_call_transfer_multi.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("multi-argument SessionChan call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:forward".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("forward MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::Call { .. }))
        .expect("multi-argument SessionChan call");
    let MirInstructionKind::Call {
        effect_receipts, ..
    } = &mut instruction.kind
    else {
        unreachable!();
    };
    effect_receipts.retain(|receipt| receipt.argument_index != 1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("missing second SessionChan call receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("SessionChan call argument 1 has no canonical TransferSession effect receipt")
    }));
}

#[test]
fn reordered_multi_session_call_receipts_are_rejected_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_call_transfer_multi.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("multi-argument SessionChan call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:forward".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("forward MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::Call { .. }))
        .expect("multi-argument SessionChan call");
    let MirInstructionKind::Call {
        effect_receipts, ..
    } = &mut instruction.kind
    else {
        unreachable!();
    };
    effect_receipts.reverse();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("reordered SessionChan call receipts must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("call effect receipts are not in canonical argument order")
    }));
}

#[test]
fn protocol_method_call_uses_checker_method_identity_across_mir_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_protocol_method.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("protocol method must lower to canonical MIR");
    let main = program
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("main MIR");
    let method = main
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::ProtocolMethod { protocol, method },
                arguments,
                ..
            } => Some((protocol.clone(), method.clone(), arguments.clone())),
            _ => None,
        })
        .expect("ProtocolMethod MIR call");
    assert_eq!(method.0 .0, "trait:Read");
    assert!(method
        .1
        .as_str()
        .starts_with("function:Read:for:Counter::read:"));
    let target = crate::core::mir::canonical_protocol_call_target(
        &crate::core::ir::ResolvedCallee::ProtocolMethod {
            protocol: method.0,
            method: method.1,
        },
    )
    .expect("ProtocolMethod has a concrete MIR target");
    assert!(program.functions().contains_key(&target));
    assert_eq!(method.2.len(), 1);

    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference ProtocolMethod execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(42)
    );
    let bytecode = crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode ProtocolMethod consumer");
    assert!(!bytecode.functions.is_empty());
    assert!(
        bytecode.ast.is_none(),
        "ProtocolMethod bytecode must be AST-free"
    );
    let bytecode_value = crate::interp::bytecode::BytecodeVM::new(bytecode)
        .run_value()
        .expect("bytecode ProtocolMethod execution");
    assert!(matches!(bytecode_value, crate::interp::Value::Int(42)));
    crate::codegen::mir::validate_mir_native(&program).expect("native ProtocolMethod validator");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier ProtocolMethod capability gate");
    if crate::tests::can_link() {
        let context = inkwell::context::Context::create();
        let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_protocol_method");
        generator
            .compile_mir_native(&program)
            .expect("native ProtocolMethod consumer");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native ProtocolMethod execution");
        assert_eq!(native.exit_code, Some(42));
        assert_eq!(native.stdout, "");
        assert_eq!(native.stderr, "");
    }
}

#[test]
fn protocol_method_with_non_function_identity_is_rejected_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_protocol_method.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("protocol method must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("main MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::Call { .. }))
        .expect("ProtocolMethod MIR call");
    let MirInstructionKind::Call { callee, .. } = &mut instruction.kind else {
        unreachable!();
    };
    *callee = crate::core::ir::ResolvedCallee::ProtocolMethod {
        protocol: crate::core::NodeId("trait:Read".into()),
        method: crate::core::ir::MethodId::new("method:Read::read").expect("method id"),
    };
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("non-function ProtocolMethod identity must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("is not a canonical function identity")
    }));
}

#[test]
fn protocol_method_protocol_identity_drift_is_rejected_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_protocol_method.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("protocol method must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("main MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::Call { .. }))
        .expect("ProtocolMethod MIR call");
    let MirInstructionKind::Call { callee, .. } = &mut instruction.kind else {
        unreachable!();
    };
    let crate::core::ir::ResolvedCallee::ProtocolMethod { protocol, .. } = callee else {
        unreachable!();
    };
    *protocol = crate::core::NodeId("trait:Other".into());
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("protocol/method identity drift must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("disagrees with protocol 'trait:Other'")
    }));
}

#[test]
fn protocol_method_argument_typedesc_drift_is_rejected_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_protocol_method.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("protocol method must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("main MIR");
    let (argument, result) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                arguments,
                result: Some(result),
                ..
            } => Some((arguments[0].clone(), result.clone())),
            _ => None,
        })
        .expect("ProtocolMethod MIR call");
    let i64_ty = program
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi
                == crate::core::mir::types::MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                })
            .then(|| id.clone())
        })
        .expect("canonical i64 TypeDesc");
    function
        .values
        .get_mut(&argument)
        .expect("receiver value")
        .ty = i64_ty.clone();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("protocol method argument TypeDesc drift must fail closed");
    assert!(errors.iter().any(|error| {
        error.message.contains("Clone result type")
            || error
                .message
                .contains("protocol method argument 0 TypeDesc")
    }));

    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("main MIR");
    function.values.get_mut(&result).expect("call result").ty = i64_ty;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("protocol method result TypeDesc drift must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("protocol method result TypeDesc disagrees with canonical method ABI")
    }));
}

#[test]
fn protocol_method_abi_contract_is_shared_and_rejects_arity_drift() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_protocol_method.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("protocol method must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let caller = program.functions().get(&owner).expect("main MIR");
    let (callee, arguments, result) = caller
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::ProtocolMethod { protocol, method },
                arguments,
                result,
                ..
            } => Some((
                crate::core::ir::ResolvedCallee::ProtocolMethod {
                    protocol: protocol.clone(),
                    method: method.clone(),
                },
                arguments.clone(),
                result.clone(),
            )),
            _ => None,
        })
        .expect("ProtocolMethod MIR call");
    let target_owner = crate::core::mir::canonical_protocol_call_target(&callee)
        .expect("ProtocolMethod has canonical target");
    let target = program
        .functions()
        .get(&target_owner)
        .expect("protocol method target MIR");
    assert!(crate::core::mir::validate_protocol_method_abi(
        &callee,
        caller,
        target,
        result.as_ref(),
        &arguments,
    )
    .is_empty());
    assert!(
        crate::core::mir::validate_materialized_call_result_presence(
            &callee,
            target,
            result.as_ref(),
            program.type_catalog(),
        )
        .is_empty()
    );

    let missing_result = crate::core::mir::validate_materialized_call_result_presence(
        &callee,
        target,
        None,
        program.type_catalog(),
    );
    assert_eq!(
        missing_result,
        vec!["protocol method result value is absent for non-unit canonical method ABI"]
    );

    let mut malformed_arguments = arguments;
    malformed_arguments.pop();
    let errors = crate::core::mir::validate_protocol_method_abi(
        &callee,
        caller,
        target,
        result.as_ref(),
        &malformed_arguments,
    );
    assert_eq!(
        errors,
        vec!["protocol method call arity disagrees with canonical method ABI"]
    );
}

#[test]
fn direct_function_abi_contract_is_shared_and_rejects_arity_drift() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_native_f64_add.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("direct function must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let caller = program.functions().get(&owner).expect("main MIR");
    let (callee, arguments, result) = caller
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                arguments,
                result,
                ..
            } if callee.0 == "function:add" => Some((
                crate::core::ir::ResolvedCallee::Function(callee.clone()),
                arguments.clone(),
                result.clone(),
            )),
            _ => None,
        })
        .expect("direct function MIR call");
    let target_owner = crate::core::mir::canonical_protocol_call_target(&callee)
        .expect("Function has canonical target");
    let target = program
        .functions()
        .get(&target_owner)
        .expect("direct function target MIR");
    assert!(crate::core::mir::validate_materialized_call_abi(
        &callee,
        caller,
        target,
        result.as_ref(),
        &arguments,
    )
    .is_empty());
    assert!(
        crate::core::mir::validate_materialized_call_result_presence(
            &callee,
            target,
            result.as_ref(),
            program.type_catalog(),
        )
        .is_empty()
    );

    let missing_result = crate::core::mir::validate_materialized_call_result_presence(
        &callee,
        target,
        None,
        program.type_catalog(),
    );
    assert_eq!(
        missing_result,
        vec![format!(
            "non-unit callee '{}' has no MIR result value",
            target.result.as_str()
        )]
    );

    let mut malformed_arguments = arguments;
    malformed_arguments.pop();
    let errors = crate::core::mir::validate_materialized_call_abi(
        &callee,
        caller,
        target,
        result.as_ref(),
        &malformed_arguments,
    );
    assert_eq!(
        errors,
        vec!["call to 'function:add' supplies 1 arguments but its MIR signature requires 2"]
    );
}

#[test]
fn canonical_gate_rejects_non_unit_call_without_result_value() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_native_f64_add.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("direct function must lower to canonical MIR");
    let main_owner = crate::core::NodeId("function:main".into());
    let mut functions = program.functions().clone();
    let main = functions.get_mut(&main_owner).expect("main MIR");
    let call = main
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(owner),
                result,
                ..
            } if owner.0 == "function:add" => Some(result),
            _ => None,
        })
        .expect("direct function call");
    *call = None;

    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("non-unit direct call without a result must fail at MIR admission");
    assert!(errors
        .iter()
        .any(|error| { error.message == "value is declared but never defined" }));
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
fn materializes_generic_option_unwrap_with_a_specialized_projection_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { .. }
            )
        })
        .expect("generic Option projection instance");
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.variant_name, "Some");
    assert_eq!(contract.discriminant, 1);
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.projection.ownership, MirOwnership::Copy);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn materializes_generic_option_unwrap_f64_with_float_projection_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_f64.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<f64> unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Copy
            )
        })
        .expect("generic Option<f64> projection instance");
    assert_eq!(instance.arguments.len(), 1);
    let payload = program
        .type_catalog()
        .get(&instance.arguments[0])
        .expect("generic Option<f64> payload TypeDesc");
    assert_eq!(
        payload.abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 64 }
    );
    assert_eq!(payload.layout, crate::core::mir::types::MirLayout::Scalar);
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered to generic variant projection");
    };
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<f64> unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn materializes_generic_option_unwrap_f32_with_float_projection_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_f32.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<f32> unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Copy
            )
        })
        .expect("generic Option<f32> projection instance");
    assert_eq!(instance.arguments.len(), 1);
    let payload = program
        .type_catalog()
        .get(&instance.arguments[0])
        .expect("generic Option<f32> payload TypeDesc");
    assert_eq!(
        payload.abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 32 }
    );
    assert_eq!(payload.layout, crate::core::mir::types::MirLayout::Scalar);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<f32> unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn materializes_generic_option_unwrap_owned_string_with_move_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_owned_string.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<string> unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Move
            )
        })
        .expect("owned generic Option projection instance");
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.variant_name, "Some");
    assert_eq!(contract.discriminant, 1);
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.projection.ownership, MirOwnership::Move);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::OwnedString);
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized owned generic Option target");
    assert!(target.blocks.values().any(|block| {
        matches!(
            block.instructions.as_slice(),
            [
                MirInstruction {
                    kind: MirInstructionKind::Move { .. },
                    ..
                },
                MirInstruction {
                    kind: MirInstructionKind::VariantProjectMove {
                        contract: Some(_),
                        ..
                    },
                    ..
                }
            ]
        )
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<string> unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn materializes_generic_option_unwrap_owned_list_with_move_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_owned_list.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<List<i32>> unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Move
            )
        })
        .expect("owned generic Option<List> projection instance");
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::List);
    let (_, payload_glue) = program
        .type_catalog()
        .validate_option_move_variant(&contract.source_ty)
        .expect("Option<List<i32>> TypeDesc receipt");
    assert_eq!(payload_glue, MirGlueKind::List);
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized owned generic Option<List> target");
    assert!(target.blocks.values().any(|block| {
        matches!(
            block.instructions.as_slice(),
            [
                MirInstruction {
                    kind: MirInstructionKind::Move { .. },
                    ..
                },
                MirInstruction {
                    kind: MirInstructionKind::VariantProjectMove {
                        contract: Some(_),
                        ..
                    },
                    ..
                }
            ]
        )
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<List> unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn materializes_generic_option_unwrap_owned_list_i64_and_bool_family() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_owned_list_scalars.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<List<i64|bool>> unwrap must lower to canonical MIR");
    let instances = program
        .instances()
        .values()
        .filter(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Move
                        && contract.projection.move_out_glue == MirGlueKind::List
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        instances.len(),
        2,
        "both concrete List scalar instances must materialize"
    );
    let element_kinds = instances
        .iter()
        .filter_map(|instance| {
            let descriptor = program.type_catalog().get(&instance.arguments[0])?;
            let crate::core::mir::types::MirLayout::List { element } = &descriptor.layout else {
                return None;
            };
            Some(program.type_catalog().get(element)?.kind.clone())
        })
        .collect::<Vec<_>>();
    assert!(element_kinds.iter().any(|kind| {
        matches!(
            kind,
            crate::core::mir::types::MirTypeKind::Primitive(PrimitiveType::I64)
        )
    }));
    assert!(element_kinds.iter().any(|kind| {
        matches!(
            kind,
            crate::core::mir::types::MirTypeKind::Primitive(PrimitiveType::Bool)
        )
    }));
    for instance in &instances {
        let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
        else {
            unreachable!("filtered to generic variant projections");
        };
        let (_, payload_glue) = program
            .type_catalog()
            .validate_option_move_variant(&contract.source_ty)
            .expect("Option<List<Copy scalar>> TypeDesc receipt");
        assert_eq!(payload_glue, MirGlueKind::List);
    }
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<List<i64|bool>> unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn rejects_generic_option_unwrap_owned_float_list_before_legacy() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_owned_float_list_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic Option<List<f64>> unwrap must fail closed");
    let text = error.to_string();
    let debug = format!("{error:?}");
    assert!(text.contains("MIR lowering failed"));
    assert!(
        debug.contains("not a Copy scalar/bool with no-op glue"),
        "{debug}"
    );
}

#[test]
fn generic_option_unwrap_none_preserves_the_canonical_trap() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_none.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option unwrap None MIR");
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("generic Option unwrap None must trap");
    assert!(
        error.to_string().contains("E0800"),
        "unexpected trap: {error}"
    );
}

#[test]
fn materializes_generic_option_unwrap_or_with_a_specialized_fallback_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("generic Option fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.variant_name, "Some");
    assert_eq!(contract.discriminant, 1);
    assert_eq!(contract.fallback_variant_name, "None");
    assert_eq!(contract.fallback_discriminant, 0);
    assert_eq!(contract.fallback_arity, 0);
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.projection.ownership, MirOwnership::Copy);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn materializes_generic_option_unwrap_or_f64_with_float_fallback_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or_f64.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<f64>.unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Copy
            )
        })
        .expect("generic Option<f64> fallback projection instance");
    assert_eq!(instance.arguments.len(), 1);
    let payload = program
        .type_catalog()
        .get(&instance.arguments[0])
        .expect("generic Option<f64> fallback payload TypeDesc");
    assert_eq!(
        payload.abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 64 }
    );
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered to generic fallback projection");
    };
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.fallback_arity, 0);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<f64>.unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn materializes_generic_option_unwrap_or_f32_with_float_fallback_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or_f32.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<f32>.unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Copy
            )
        })
        .expect("generic Option<f32> fallback projection instance");
    assert_eq!(
        program
            .type_catalog()
            .get(&instance.arguments[0])
            .expect("generic Option<f32> fallback payload TypeDesc")
            .abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 32 }
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<f32>.unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_option_unwrap_or_f32_none_selects_the_fallback() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or_f32_none.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<f32> unwrap_or None MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<f32>.unwrap_or None execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_option_unwrap_or_f64_none_selects_the_fallback() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or_f64_none.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<f64> unwrap_or None MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<f64>.unwrap_or None execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_option_unwrap_or_none_selects_the_explicit_fallback() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or_none.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option unwrap_or None MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option unwrap_or None execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(7));
}

#[test]
fn materializes_generic_option_unwrap_or_owned_string_with_consuming_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_string.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic managed Option unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Move
            )
        })
        .expect("managed generic Option fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::OwnedString);
    let target = program
        .functions()
        .get(&instance.function)
        .expect("managed generic Option fallback target");
    assert!(target.blocks.values().any(|block| {
        matches!(
            block.instructions.as_slice(),
            [MirInstruction {
                kind: MirInstructionKind::VariantProjectOr {
                    base,
                    fallback,
                    contract: Some(_),
                    ..
                },
                ..
            }] if base == &target.parameters[0] && fallback == &target.parameters[1]
        )
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Option unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn generic_option_unwrap_or_owned_string_none_transfers_fallback() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_string_none.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic managed Option unwrap_or None must lower to canonical MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Option unwrap_or None execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(7));
}

#[test]
fn materializes_generic_option_unwrap_or_owned_list_with_consuming_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_list.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic managed Option<List<i32>> unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                    if contract.projection.ownership == MirOwnership::Move
                        && contract.projection.move_out_glue == MirGlueKind::List
            )
        })
        .expect("managed generic Option<List> fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.nominal.as_str(), "builtin:type:Option");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Option<List> unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn generic_option_unwrap_or_owned_list_none_transfers_fallback() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_list_none.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic managed Option<List> unwrap_or None must lower to canonical MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Option<List> unwrap_or None execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(7));
}

#[test]
fn rejects_generic_option_unwrap_or_unsupported_managed_payload_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("unsupported managed Option fallback must remain fail-closed");
    assert!(error.to_string().contains("generic MIR instance"));
}

#[test]
fn rejects_generic_option_unwrap_or_owned_list_with_non_copy_element_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_list_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("List<string> fallback must remain outside the managed List island");
    assert!(error.to_string().contains("generic MIR instance"));
}

#[test]
fn generic_option_unwrap_or_stale_receipt_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or.mimi");
    let checked = checked_program(source);
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option unwrap_or MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("generic Option fallback projection instance");
    let mut functions = canonical.functions().clone();
    let target = functions
        .get_mut(&instance.function)
        .expect("materialized generic Option fallback target");
    let receipt = target
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::VariantProjectOr {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("fallback projection receipt");
    receipt.fallback_discriminant = receipt.fallback_discriminant.wrapping_add(1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog_and_instances(
        functions,
        canonical.type_catalog().clone(),
        canonical.instances().clone(),
    )
    .expect_err("stale generic Option fallback receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("generic MIR variant fallback projection contract is invalid")
    }));
}

#[test]
fn generic_option_unwrap_stale_receipt_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap.mimi");
    let checked = checked_program(source);
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option unwrap MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { .. }
            )
        })
        .expect("generic Option projection instance");
    let mut functions = canonical.functions().clone();
    let target = functions
        .get_mut(&instance.function)
        .expect("materialized generic Option projection target");
    let receipt = target
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::VariantProject {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("projection receipt");
    receipt.discriminant = receipt.discriminant.wrapping_add(1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog_and_instances(
        functions,
        canonical.type_catalog().clone(),
        canonical.instances().clone(),
    )
    .expect_err("stale generic Option projection receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant projection trap receipt disagrees with TypeDesc")
    }));
}

#[test]
fn materializes_generic_result_unwrap_with_a_specialized_projection_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { .. }
            )
        })
        .expect("generic Result projection instance");
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.variant_name, "Ok");
    assert_eq!(contract.discriminant, 0);
    assert_eq!(contract.projection.nominal.as_str(), "builtin:type:Result");
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.projection.ownership, MirOwnership::Copy);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Result unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn generic_result_unwrap_err_preserves_the_canonical_trap() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_none.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result unwrap Err MIR");
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("generic Result unwrap Err must trap");
    assert!(
        error.to_string().contains("E0800"),
        "unexpected trap: {error}"
    );
}

#[test]
fn materializes_generic_result_distinct_unwrap_with_copy_scalar_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_distinct_unwrap.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result<T, i32> unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Result"
            )
        })
        .expect("generic distinct Result projection instance");
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered above");
    };
    let crate::core::mir::types::MirLayout::Result { ok, error, .. } = &program
        .type_catalog()
        .get(&contract.source_ty)
        .expect("specialized Result TypeDesc")
        .layout
    else {
        panic!("specialized source must retain a Result layout");
    };
    assert_ne!(
        ok, error,
        "distinct projection must preserve separate payload IDs"
    );
    program
        .type_catalog()
        .validate_copy_result_scalar_variant(&contract.source_ty)
        .expect("heterogeneous Copy Result TypeDesc contract");
    assert!(
        program
            .type_catalog()
            .validate_flat_copy_variant(&contract.source_ty)
            .is_err(),
        "the homogeneous concrete Result island remains closed"
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference distinct Result unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn generic_result_distinct_unwrap_stale_receipt_is_rejected_before_consumers() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_distinct_unwrap.mimi");
    let checked = checked_program(source);
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic distinct Result unwrap MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Result"
            )
        })
        .expect("generic distinct Result projection instance");
    let mut functions = canonical.functions().clone();
    let target = functions
        .get_mut(&instance.function)
        .expect("materialized generic distinct Result target");
    let receipt = target
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::VariantProject {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("distinct Result projection receipt");
    receipt.discriminant = receipt.discriminant.wrapping_add(1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog_and_instances(
        functions,
        canonical.type_catalog().clone(),
        canonical.instances().clone(),
    )
    .expect_err("stale generic distinct Result receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant projection trap receipt disagrees with TypeDesc")
    }));
}

#[test]
fn generic_result_distinct_unwrap_err_preserves_the_canonical_trap() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_distinct_unwrap_err.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic distinct Result Err MIR");
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("generic distinct Result unwrap Err must trap");
    assert!(
        error.to_string().contains("E0800"),
        "unexpected trap: {error}"
    );
}

#[test]
fn materializes_generic_result_f64_unwrap_with_heterogeneous_copy_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_f64.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result<T, i32> f64 unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Result"
            )
        })
        .expect("generic Result f64 projection instance");
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.ownership, MirOwnership::Copy);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let crate::core::mir::types::MirLayout::Result { ok, error, .. } = &program
        .type_catalog()
        .get(&contract.source_ty)
        .expect("specialized heterogeneous Result TypeDesc")
        .layout
    else {
        panic!("specialized source must retain a Result layout");
    };
    assert_ne!(ok, error);
    assert!(matches!(
        program
            .type_catalog()
            .get(ok)
            .map(|descriptor| descriptor.abi),
        Some(crate::core::mir::types::MirAbiClass::Float { bits: 64 })
    ));
    assert!(matches!(
        program
            .type_catalog()
            .get(error)
            .map(|descriptor| descriptor.abi),
        Some(crate::core::mir::types::MirAbiClass::Integer {
            bits: 32,
            signed: true,
        })
    ));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Result f64 unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_result_f64_unwrap_err_preserves_the_canonical_trap() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_f64_err.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result<T, i32> f64 Err MIR");
    let helper = crate::core::NodeId("function:unwrap_f64_err".into());
    assert!(
        program.functions().contains_key(&helper),
        "the f64 Result trap belongs to the exact helper owner"
    );
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&helper, &[])
        .expect_err("generic Result f64 unwrap Err must trap");
    assert!(
        error.to_string().contains("E0800"),
        "unexpected trap: {error}"
    );

    let bytecode = crate::interp::bytecode::mir::compile_mir_program(&program)
        .expect("generic Result f64 unwrap Err bytecode");
    assert!(bytecode.ast.is_none());
    let bytecode_error = crate::interp::bytecode::BytecodeVM::new(bytecode)
        .call_named(&helper.0, Vec::new())
        .expect_err("bytecode Err(7).unwrap must trap in the exact helper owner");
    assert_eq!(bytecode_error.code(), "E0800");
}

#[test]
fn generic_result_homogeneous_f64_unwrap_is_rejected_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_homogeneous_f64_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("homogeneous Result<T, T> f64 must remain outside the generic island");
    let message = format!("{error:?}");
    assert!(
        message.contains("generic MIR instance") && message.contains("outside scalar contract"),
        "unexpected lowering rejection: {message}"
    );
}

#[test]
fn direct_result_f64_unwrap_remains_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_result_f64_unwrap_rejected.mimi");
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("direct concrete Result<f64, i32> must remain outside the generic island");
    let message = format!("{error:?}");
    assert!(
        message.contains("Result projection") || message.contains("variant projection"),
        "unexpected direct Result<f64> rejection: {message}"
    );
}

#[test]
fn materializes_generic_result_unwrap_or_with_a_specialized_fallback_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_or.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("generic Result fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.nominal.as_str(), "builtin:type:Result");
    assert_eq!(contract.variant_name, "Ok");
    assert_eq!(contract.discriminant, 0);
    assert_eq!(contract.fallback_variant_name, "Err");
    assert_eq!(contract.fallback_discriminant, 1);
    assert_eq!(contract.fallback_arity, 1);
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.projection.ownership, MirOwnership::Copy);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Result unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(48));
}

#[test]
fn materializes_generic_result_f64_unwrap_or_with_heterogeneous_copy_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_or_f64.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result<T, i32> f64 unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Result"
            )
        })
        .expect("generic Result f64 fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.ownership, MirOwnership::Copy);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let crate::core::mir::types::MirLayout::Result { ok, error, .. } = &program
        .type_catalog()
        .get(&contract.source_ty)
        .expect("specialized heterogeneous Result TypeDesc")
        .layout
    else {
        panic!("specialized source must retain a Result layout");
    };
    assert_ne!(ok, error);
    assert!(matches!(
        program
            .type_catalog()
            .get(ok)
            .map(|descriptor| descriptor.abi),
        Some(crate::core::mir::types::MirAbiClass::Float { bits: 64 })
    ));
    assert!(matches!(
        program
            .type_catalog()
            .get(error)
            .map(|descriptor| descriptor.abi),
        Some(crate::core::mir::types::MirAbiClass::Integer {
            bits: 32,
            signed: true,
        })
    ));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Result f64 unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_result_f64_unwrap_or_err_selects_the_fallback() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_or_f64_err.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result<T, i32> f64 Err unwrap_or MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Result f64 Err unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_result_homogeneous_f64_unwrap_or_is_rejected_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_homogeneous_f64_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("homogeneous Result<T, T> f64 unwrap_or must remain fail-closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("generic MIR instance") && message.contains("outside scalar contract"),
        "unexpected lowering rejection: {message}"
    );
}

#[test]
fn direct_result_f64_unwrap_or_remains_rejected_before_consumers() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_result_f64_unwrap_or_rejected.mimi");
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("direct concrete Result<f64, i32> unwrap_or must remain fail-closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("Result fallback") || message.contains("variant projection"),
        "unexpected direct Result<f64> fallback rejection: {message}"
    );
}

#[test]
fn materializes_generic_result_bool_f64_unwrap_or_with_heterogeneous_copy_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_f64.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result<T, bool> f64 unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Result"
                        && contract.projection.ownership == MirOwnership::Copy
            )
        })
        .expect("generic Result bool/f64 fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    let crate::core::mir::types::MirLayout::Result { ok, error, .. } = &program
        .type_catalog()
        .get(&contract.source_ty)
        .expect("specialized heterogeneous Result TypeDesc")
        .layout
    else {
        panic!("specialized source must retain a Result layout");
    };
    assert_ne!(ok, error);
    assert!(matches!(
        program
            .type_catalog()
            .get(ok)
            .map(|descriptor| descriptor.abi),
        Some(crate::core::mir::types::MirAbiClass::Float { bits: 64 })
    ));
    assert!(matches!(
        program
            .type_catalog()
            .get(error)
            .map(|descriptor| descriptor.abi),
        Some(crate::core::mir::types::MirAbiClass::Bool)
    ));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Result bool/f64 unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_result_bool_f64_unwrap_or_err_selects_the_fallback() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_f64_err.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result<T, bool> f64 Err unwrap_or MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Result bool/f64 Err unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn direct_result_bool_f64_unwrap_or_remains_rejected_before_consumers() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_result_bool_f64_unwrap_or_rejected.mimi");
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("direct concrete Result<f64, bool> unwrap_or must remain fail-closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("Result fallback") || message.contains("variant projection"),
        "unexpected direct Result<bool> fallback rejection: {message}"
    );
}

#[test]
fn materializes_generic_distinct_result_unwrap_or_with_two_payload_slots() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_distinct_unwrap_or.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic heterogeneous Result unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("generic heterogeneous Result fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    let crate::core::mir::types::MirLayout::Result { ok, error, .. } = &program
        .type_catalog()
        .get(&contract.source_ty)
        .expect("specialized heterogeneous Result TypeDesc")
        .layout
    else {
        panic!("specialized source must retain a Result layout");
    };
    assert_ne!(ok, error);
    assert_eq!(ok, &contract.result_ty);
    assert!(matches!(
        program.type_catalog().get(error).map(|desc| &desc.kind),
        Some(crate::core::mir::types::MirTypeKind::Primitive(
            crate::core::PrimitiveType::I32
        ))
    ));
    assert_eq!(contract.fallback_ty, contract.result_ty);
    assert_eq!(contract.fallback_variant_name, "Err");
    assert_eq!(contract.fallback_discriminant, 1);
    assert_eq!(contract.fallback_arity, 1);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference heterogeneous Result unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(50));
}

#[test]
fn materializes_managed_generic_result_unwrap_or_with_move_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_string.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("managed Result unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("managed Result fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.nominal.as_str(), "builtin:type:Result");
    assert_eq!(contract.variant_name, "Ok");
    assert_eq!(contract.fallback_variant_name, "Err");
    assert_eq!(contract.fallback_arity, 1);
    assert_eq!(contract.projection.ownership, MirOwnership::Move);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::OwnedString);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Result unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn materializes_managed_generic_bool_result_unwrap_or_with_move_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_owned_string.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("managed Result<bool> unwrap_or must lower to canonical MIR");
    let contracts = program
        .instances()
        .values()
        .filter_map(|instance| match &instance.contract {
            MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Result" =>
            {
                Some(contract)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        contracts.len(),
        1,
        "one deduplicated String specialization serves Ok and Err"
    );
    assert!(contracts.iter().all(|contract| {
        contract.projection.ownership == MirOwnership::Move
            && contract.projection.move_out_glue == MirGlueKind::OwnedString
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Result<bool> unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn rejects_managed_generic_bool_result_unwrap_or_outside_move_payload_contract() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_owned_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("Result<List<string>, bool> fallback must fail closed");
    let message = error.to_string();
    assert!(message.contains("generic MIR instance") || message.contains("generic Result"));
}

#[test]
fn managed_generic_result_unwrap_or_moves_the_fallback_on_err() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_string_err.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("managed Result Err unwrap_or must lower to canonical MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Result Err unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(7));
}

#[test]
fn rejects_managed_generic_result_unwrap_or_outside_move_payload_contract() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_rejected.mimi"
    );
    let checked = checked_program(source);
    let errors = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("unsupported managed Result payload must fail closed");
    let message = errors.to_string();
    assert!(
        message.contains("generic Result fallback projection")
            || message.contains("generic MIR instance argument")
    );
}

#[test]
fn materializes_generic_result_unwrap_or_owned_list_with_move_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Result<List<i32>> unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("Result<List> fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.nominal.as_str(), "builtin:type:Result");
    assert_eq!(contract.projection.ownership, MirOwnership::Move);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::List);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Result<List> unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn generic_result_unwrap_or_owned_list_err_transfers_fallback() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list_err.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Result<List<i32>> Err unwrap_or must lower to canonical MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Result<List> Err unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(7));
}

#[test]
fn rejects_generic_result_unwrap_or_owned_list_with_non_copy_element_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list_rejected.mimi"
    );
    let checked = checked_program(source);
    let errors = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("Result<List<string>> fallback must fail closed");
    let message = errors.to_string();
    assert!(
        message.contains("generic Result fallback projection")
            || message.contains("generic MIR instance argument")
    );
}

#[test]
fn materializes_generic_result_unwrap_or_owned_list_copy_scalar_family() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list_scalars.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Result<List<i64|bool>> unwrap_or must lower to canonical MIR");
    let contracts = program
        .instances()
        .values()
        .filter_map(|instance| match &instance.contract {
            MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Result" =>
            {
                Some(contract)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(contracts.len(), 2, "i64 and bool specializations");
    assert!(contracts.iter().all(|contract| {
        contract.projection.ownership == MirOwnership::Move
            && contract.projection.move_out_glue == MirGlueKind::List
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Result<List<i64|bool>> unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(49));
}

#[test]
fn generic_result_unwrap_or_stale_receipt_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_or.mimi");
    let checked = checked_program(source);
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result unwrap_or MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("generic Result fallback projection instance");
    let mut functions = canonical.functions().clone();
    let target = functions
        .get_mut(&instance.function)
        .expect("materialized generic Result fallback target");
    let receipt = target
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::VariantProjectOr {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("fallback projection receipt");
    receipt.fallback_discriminant = receipt.fallback_discriminant.wrapping_add(1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog_and_instances(
        functions,
        canonical.type_catalog().clone(),
        canonical.instances().clone(),
    )
    .expect_err("stale generic Result fallback receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant projection fallback receipt disagrees with TypeDesc")
    }));
}

#[test]
fn rejects_generic_option_unwrap_for_unsupported_copy_payload_before_legacy() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_rejected.mimi");
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("unsupported generic Option unwrap must fail closed");
    assert!(error.to_string().contains("MIR lowering failed"));
}

#[test]
fn rejects_generic_result_unwrap_and_option_unwrap_or_before_legacy() {
    {
        let source =
            include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_rejected.mimi");
        let checked = checked_program(source);
        let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect_err("unsupported generic variant projection must fail closed");
        assert!(error.to_string().contains("MIR lowering failed"));
    }
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
fn lowers_result_i32_i32_unwrap_to_copy_variant_projection() {
    let source = include_str!("../../../tests/fixtures/mir_native_result_i32_unwrap.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Result<i32, i32>.unwrap must lower to canonical MIR");
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
        .expect("Result unwrap must carry a read-only variant projection receipt");
    assert_eq!(receipt.variant_name, "Ok");
    assert_eq!(receipt.projection.nominal.as_str(), "builtin:type:Result");
    assert_eq!(receipt.projection.field_index, 0);
    assert_eq!(receipt.projection.arity, 1);
    assert_eq!(receipt.projection.ownership, MirOwnership::Copy);
    assert_eq!(receipt.projection.move_out_glue, MirGlueKind::Noop);
    program
        .type_catalog()
        .validate_copy_result_i32_variant(&receipt.source_ty)
        .expect("Result<i32, i32> TypeDesc contract");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Result<i32, i32>.unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:unwrap_err".into()), &[])
        .expect_err("Result::unwrap on Err must trap");
    assert!(
        error.message.contains("E0800"),
        "unexpected Result active-tag trap: {error}"
    );
}

#[test]
fn lowers_option_bool_unwrap_to_copy_variant_projection() {
    let source = include_str!("../../../tests/fixtures/mir_native_option_bool_unwrap.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Option<bool>.unwrap must lower to canonical MIR");
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
        .expect("Copy bool unwrap must carry a read-only variant projection receipt");
    assert_eq!(receipt.variant_name, "Some");
    assert_eq!(receipt.projection.field_index, 0);
    assert_eq!(receipt.projection.arity, 1);
    assert_eq!(receipt.projection.ownership, MirOwnership::Copy);
    assert_eq!(receipt.projection.move_out_glue, MirGlueKind::Noop);
}

#[test]
fn lowers_option_i64_unwrap_to_copy_variant_projection() {
    let source = include_str!("../../../tests/fixtures/mir_native_option_i64_unwrap.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Option<i64>.unwrap must lower to canonical MIR");
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
        .expect("Copy i64 unwrap must carry a read-only variant projection receipt");
    assert_eq!(receipt.variant_name, "Some");
    assert_eq!(receipt.projection.field_index, 0);
    assert_eq!(receipt.projection.arity, 1);
    assert_eq!(receipt.projection.ownership, MirOwnership::Copy);
    assert_eq!(receipt.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Option<i64>.unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn lowers_option_f64_unwrap_to_copy_variant_projection() {
    let source = include_str!("../../../tests/fixtures/mir_native_option_f64_unwrap.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Option<f64>.unwrap must lower to canonical MIR");
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
        .expect("Copy f64 unwrap must carry a read-only variant projection receipt");
    assert_eq!(receipt.variant_name, "Some");
    assert_eq!(receipt.projection.field_index, 0);
    assert_eq!(receipt.projection.arity, 1);
    assert_eq!(receipt.projection.ownership, MirOwnership::Copy);
    assert_eq!(receipt.projection.move_out_glue, MirGlueKind::Noop);
    let option_desc = program
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == crate::core::mir::types::MirTypeKind::Option)
                .then_some((id, descriptor))
        })
        .expect("Option<f64> TypeDesc");
    let crate::core::mir::types::MirLayout::Option { inner, .. } = &option_desc.1.layout else {
        panic!("Option<f64> must retain its Option layout");
    };
    let inner_desc = program
        .type_catalog()
        .get(inner)
        .expect("Option<f64> inner TypeDesc");
    assert_eq!(
        inner_desc.abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 64 }
    );
    assert_eq!(
        inner_desc.layout,
        crate::core::mir::types::MirLayout::Scalar
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:unwrap_copy".into()), &[])
        .expect("reference Option<f64>.unwrap execution");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::FloatBits(41.5f64.to_bits())
    );
}

#[test]
fn lowers_f64_unary_negate_with_explicit_copy_float_contract() {
    let source = include_str!("../../../tests/fixtures/mir_native_f64_negate.mimi");
    let program =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("f64 unary negate must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:negate".into()))
        .expect("negate MIR function");
    let (result_ty, operand_ty) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Unary {
                result,
                op: crate::core::ir::ResolvedUnaryOp::Negate,
                operand,
            } => Some((
                function.values.get(result)?.ty.clone(),
                function.values.get(operand)?.ty.clone(),
            )),
            _ => None,
        })
        .expect("canonical f64 negate instruction");
    assert_eq!(result_ty, operand_ty);
    program
        .type_catalog()
        .validate_copy_float_unary(
            &result_ty,
            &operand_ty,
            crate::core::ir::ResolvedUnaryOp::Negate,
        )
        .expect("f64 negate TypeDesc contract");
    let descriptor = program
        .type_catalog()
        .get(&result_ty)
        .expect("f64 negate TypeDesc");
    assert_eq!(
        descriptor.abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 64 }
    );
    assert_eq!(descriptor.ownership, MirOwnership::Copy);
    assert_eq!(descriptor.glue.move_out, MirGlueKind::Noop);

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &crate::core::NodeId("function:negate".into()),
            &[crate::core::mir::reference::MirRuntimeValue::FloatBits(
                2.5f64.to_bits(),
            )],
        )
        .expect("reference f64 negate execution");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::FloatBits((-2.5f64).to_bits())
    );
    assert!(program
        .type_catalog()
        .validate_copy_float_unary(
            &result_ty,
            &operand_ty,
            crate::core::ir::ResolvedUnaryOp::Not,
        )
        .is_err());
}

#[test]
fn lowers_f64_add_with_finite_only_copy_float_contract() {
    let source = include_str!("../../../tests/fixtures/mir_native_f64_add.mimi");
    let program =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("f64 add must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:add".into()))
        .expect("add MIR function");
    let (result_ty, left_ty, right_ty) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Binary {
                result,
                op: crate::core::ir::ResolvedBinaryOp::Add,
                left,
                right,
            } => Some((
                function.values.get(result)?.ty.clone(),
                function.values.get(left)?.ty.clone(),
                function.values.get(right)?.ty.clone(),
            )),
            _ => None,
        })
        .expect("canonical f64 add instruction");
    program
        .type_catalog()
        .validate_copy_float_binary(
            &result_ty,
            &left_ty,
            &right_ty,
            crate::core::ir::ResolvedBinaryOp::Add,
        )
        .expect("f64 add TypeDesc contract");
    let descriptor = program
        .type_catalog()
        .get(&result_ty)
        .expect("f64 add TypeDesc");
    assert_eq!(
        descriptor.abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 64 }
    );
    assert_eq!(descriptor.ownership, MirOwnership::Copy);
    assert_eq!(descriptor.glue.move_out, MirGlueKind::Noop);

    let owner = crate::core::NodeId("function:add".into());
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&program);
    let value = reference
        .execute(
            &owner,
            &[
                crate::core::mir::reference::MirRuntimeValue::FloatBits(1.25f64.to_bits()),
                crate::core::mir::reference::MirRuntimeValue::FloatBits(2.75f64.to_bits()),
            ],
        )
        .expect("reference f64 add execution");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::FloatBits(4.0f64.to_bits())
    );
    for (left, right, label) in [
        (f64::NAN, 1.0, "NaN operand"),
        (f64::INFINITY, 1.0, "Inf operand"),
        (f64::MAX, f64::MAX, "non-finite result"),
    ] {
        let error = reference
            .execute(
                &owner,
                &[
                    crate::core::mir::reference::MirRuntimeValue::FloatBits(left.to_bits()),
                    crate::core::mir::reference::MirRuntimeValue::FloatBits(right.to_bits()),
                ],
            )
            .expect_err(label);
        assert!(
            error
                .message
                .contains(crate::core::mir::types::MIR_FLOAT_NOT_FINITE_TRAP_CODE),
            "{label} must retain E0813 classification: {error}"
        );
    }
    // R6-1067 restatement: Multiply joined the shared Copy f64 validator
    // beside Add/Subtract, so the former rejection pin flips to an
    // admission pin; Remainder is the operator that stays outside.
    assert!(program
        .type_catalog()
        .validate_copy_float_binary(
            &result_ty,
            &left_ty,
            &right_ty,
            crate::core::ir::ResolvedBinaryOp::Multiply,
        )
        .is_ok());
    assert!(program
        .type_catalog()
        .validate_copy_float_binary(
            &result_ty,
            &left_ty,
            &right_ty,
            crate::core::ir::ResolvedBinaryOp::Remainder,
        )
        .is_err());
}

// R6-1068: the comparison face — f64×f64 → Bool — is admitted by the
// shared validator, and the reference executor and bytecode VM both answer
// the IEEE predicate.  The -0.0 operand is the teeth: the runtime bit
// patterns of `-1.0 * 0.0` and `0.0` differ, so a consumer comparing
// bit patterns instead of decoded f64 values answers 0 where IEEE
// equality (and both AST backends) answer 1.
#[test]
fn executes_f64_comparisons_with_ieee_decoding_across_consumers() {
    let source = r#"
        func is_zero(a: f64) -> i32 {
            if a == 0.0 {
                1
            } else {
                0
            }
        }
        func main() -> i32 { is_zero(-1.0 * 0.0) }
    "#;
    let program =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("f64 comparison MIR lowers");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:is_zero".into()))
        .expect("is_zero MIR function");
    let (result_ty, left_ty, right_ty) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Binary {
                result,
                op: crate::core::ir::ResolvedBinaryOp::Equal,
                left,
                right,
            } => Some((
                function.values.get(result)?.ty.clone(),
                function.values.get(left)?.ty.clone(),
                function.values.get(right)?.ty.clone(),
            )),
            _ => None,
        })
        .expect("canonical f64 Equal instruction");
    let catalog = program.type_catalog();
    catalog
        .validate_copy_float_binary(
            &result_ty,
            &left_ty,
            &right_ty,
            crate::core::ir::ResolvedBinaryOp::Equal,
        )
        .expect("f64 Equal comparison TypeDesc contract");
    // Validator teeth: a comparison whose result is not the Bool TypeDesc
    // stays outside the face (the f64-result shape is arithmetic-only).
    assert!(catalog
        .validate_copy_float_binary(
            &left_ty,
            &left_ty,
            &right_ty,
            crate::core::ir::ResolvedBinaryOp::Equal,
        )
        .is_err());

    let owner = crate::core::NodeId("function:is_zero".into());
    let negzero = crate::core::mir::reference::MirRuntimeValue::FloatBits((-0.0f64).to_bits());
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&owner, std::slice::from_ref(&negzero))
        .expect("reference f64 comparison execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(1)
    );
    let bytecode = crate::interp::bytecode::BytecodeVM::new(
        crate::interp::bytecode::compile_mir_program(&program)
            .expect("f64 comparison MIR bytecode"),
    )
    .run_value()
    .expect("bytecode f64 comparison execution");
    assert_eq!(bytecode, crate::interp::Value::Int(1));
}

#[test]
fn lowers_f64_subtract_with_finite_only_copy_float_contract() {
    let source = include_str!("../../../tests/fixtures/mir_native_f64_subtract.mimi");
    let program =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("f64 subtract must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:subtract".into()))
        .expect("subtract MIR function");
    let (result_ty, left_ty, right_ty) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Binary {
                result,
                op: crate::core::ir::ResolvedBinaryOp::Subtract,
                left,
                right,
            } => Some((
                function.values.get(result)?.ty.clone(),
                function.values.get(left)?.ty.clone(),
                function.values.get(right)?.ty.clone(),
            )),
            _ => None,
        })
        .expect("canonical f64 subtract instruction");
    program
        .type_catalog()
        .validate_copy_float_binary(
            &result_ty,
            &left_ty,
            &right_ty,
            crate::core::ir::ResolvedBinaryOp::Subtract,
        )
        .expect("f64 subtract TypeDesc contract");
    let owner = crate::core::NodeId("function:subtract".into());
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&program);
    let value = reference
        .execute(
            &owner,
            &[
                crate::core::mir::reference::MirRuntimeValue::FloatBits(5.5f64.to_bits()),
                crate::core::mir::reference::MirRuntimeValue::FloatBits(2.25f64.to_bits()),
            ],
        )
        .expect("reference f64 subtract execution");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::FloatBits(3.25f64.to_bits())
    );
    for (left, right, label) in [
        (f64::NAN, 1.0, "NaN operand"),
        (f64::INFINITY, 1.0, "Inf operand"),
        (f64::MAX, -f64::MAX, "non-finite result"),
    ] {
        let error = reference
            .execute(
                &owner,
                &[
                    crate::core::mir::reference::MirRuntimeValue::FloatBits(left.to_bits()),
                    crate::core::mir::reference::MirRuntimeValue::FloatBits(right.to_bits()),
                ],
            )
            .expect_err(label);
        assert!(
            error
                .message
                .contains(crate::core::mir::types::MIR_FLOAT_NOT_FINITE_TRAP_CODE),
            "{label} must retain E0813 classification: {error}"
        );
    }
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
    assert!(error.contains("expected I32"), "{error}");

    let bool_program =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(
            "func main() -> i32 { let value: Option<bool> = Some(true); drop(value); 0 }",
        ))
        .expect("Option<bool> construction MIR");
    let option_bool = bool_program
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == crate::core::mir::types::MirTypeKind::Option).then_some(id.clone())
        })
        .expect("Option<bool> TypeDesc");
    assert!(bool_program
        .type_catalog()
        .validate_copy_option_variant(&option_bool, PrimitiveType::Bool)
        .is_ok());
    let error = bool_program
        .type_catalog()
        .validate_copy_option_i32_variant(&option_bool)
        .expect_err("Option<bool> must not satisfy the i32 island");
    assert!(error.contains("expected I32"), "{error}");
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
fn result_i64_unwrap_remains_fail_closed_outside_copy_result_i32_island() {
    let source = r#"
        func main() -> i64 {
            let value: Result<i64, i32> = Ok(41)
            value.unwrap()
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked).expect_err(
        "Result<i64, i32>::unwrap must remain outside the Copy Result<i32, i32> island",
    );
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
fn option_f64_unwrap_or_remains_fail_closed_outside_copy_projection_island() {
    let source = r#"
        func unwrap_option_f64() -> f64 {
            let value: Option<f64> = Some(41.0)
            value.unwrap_or(0.0)
        }
        func main() -> i64 {
            let value = unwrap_option_f64()
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("Copy Option<f64>::unwrap_or must remain outside the copy projection island");
    let crate::core::mir::reference::MirProgramBuildError::Lowering(errors) = error else {
        panic!("Copy Option<f64>::unwrap_or must fail during MIR lowering");
    };
    assert!(
        errors.iter().any(|error| error.message.contains(
            "Option/Result unwrap shape is outside the canonical variant projection contract"
        )),
        "unexpected fail-closed diagnostics: {errors:?}"
    );
}

#[test]
fn ffi_verifier_singleton_for_keeps_checked_construction_projection_call_and_drop() {
    let source = r#"
        extern "C" { func positive(value: i32) -> i32 requires: value > 0; }
        func one_positive(value: i32) -> i32 {
            requires: value == 0
            for item in [value + 1] { positive(item); }
            0
        }
        func main() -> i64 { 0 }
    "#;
    let checked = checked_program(source);
    let owner = NodeId("function:one_positive".into());
    let callable = checked.callable(&owner).expect("checked caller");
    let catalog = types::MirTypeCatalog::from_checked_program(&checked).expect("Checker TypeDesc");
    let program = reference::MirProgram::from_checked_program_ffi_caller(&checked, &owner)
        .expect("isolated FFI verifier singleton loop");
    let function = program
        .functions()
        .get(&owner)
        .expect("materialized caller");
    function.validate().expect("canonical singleton-loop MIR");

    let ResolvedStmtKind::For { pattern, body, .. } = &callable
        .body
        .root
        .statements
        .iter()
        .find(|statement| matches!(&statement.kind, ResolvedStmtKind::For { .. }))
        .expect("source singleton loop")
        .kind
    else {
        unreachable!();
    };
    let ResolvedPatternKind::Binding { local, .. } = &pattern.kind else {
        panic!("direct element binding");
    };
    let ResolvedStmtKind::Expr(call_expression) = &body.statements[0].kind else {
        panic!("single body call");
    };
    let ResolvedExprKind::Call(source_call) = &call_expression.kind else {
        panic!("resolved body call");
    };

    let block = function.blocks.get(&function.entry).expect("entry block");
    let construct_index = block
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::ConstructList {
                    elements,
                    list_construct_contract: Some(_),
                    ..
                } if elements.len() == 1
            )
        })
        .expect("Checker-receipted one-element List construction");
    let (list, element_input) = match &block.instructions[construct_index].kind {
        MirInstructionKind::ConstructList {
            result, elements, ..
        } => (result.clone(), elements[0].clone()),
        _ => unreachable!(),
    };
    let element_eval_index = block
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Binary { result, .. } if result == &element_input
            )
        })
        .expect("element expression is evaluated exactly as checked");
    let zero_index = block
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Const {
                    literal: crate::core::ir::ResolvedLiteral::Int(0),
                    ..
                }
            )
        })
        .expect("synthetic literal zero index");
    let (index_value, projected) = match &block.instructions[zero_index + 1].kind {
        MirInstructionKind::Project {
            result,
            base,
            projection: MirProjection::Index(index),
            list_index_contract: Some(_),
        } if base == &list => (index.clone(), result.clone()),
        other => panic!("expected checked List index projection, got {other:?}"),
    };
    assert!(matches!(
        &block.instructions[zero_index].kind,
        MirInstructionKind::Const { result, .. } if result == &index_value
    ));
    let checked_binding = MirValueId::new(format!("local:{}", local.0 .0)).expect("local id");
    assert!(
        block.instructions.iter().any(|instruction| matches!(
            &instruction.kind,
            MirInstructionKind::Move { result, source }
                if result == &checked_binding && source == &projected
        )),
        "the extracted element must be installed under the Checker local identity"
    );

    let exact_call_id = MirInstructionId::new(format!("inst:call:{}", call_expression.node_id.0))
        .expect("source callsite id");
    let call_index = block
        .instructions
        .iter()
        .position(|instruction| instruction.id == exact_call_id)
        .expect("original Checker callsite identity");
    assert!(matches!(
        &block.instructions[call_index].kind,
        MirInstructionKind::Call {
            callee,
            arguments,
            ..
        } if callee == &source_call.callee && arguments.len() == 1
    ));
    let drop_index = block
        .instructions
        .iter()
        .position(|instruction| {
            matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == &list)
        })
        .expect("Checker-authorized iterable scope Drop");
    assert!(
        element_eval_index < construct_index
            && construct_index < zero_index
            && zero_index < call_index
            && call_index < drop_index,
        "construction, checked index and body call must precede scope cleanup"
    );

    let normal = super::lower::lower_callable_with_type_catalog(callable, &catalog)
        .expect_err("ordinary Phase 0 and backend lowering must keep For fail-closed");
    assert!(normal.iter().any(|error| {
        error
            .message
            .contains("structured control flow is not lowered by MIR Phase 0")
    }));
}

#[test]
fn verifier_only_ffi_caller_lowers_checker_owned_constants() {
    let checked = checked_program(
        r#"
            const SOURCE_VALUE: i32 = 41
            extern "C" { func foreign_identity(value: i32) -> i32; }
            func caller() -> i32 { foreign_identity(SOURCE_VALUE) }
            func main() -> i64 { 0 }
        "#,
    );
    let owner = NodeId("function:caller".into());
    let mir = reference::MirProgram::from_checked_program_ffi_caller(&checked, &owner)
        .expect("verifier-only source builder retains Checker constants");
    let caller = mir.functions().get(&owner).expect("isolated caller body");
    assert!(caller.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Const {
                    literal: crate::core::ir::ResolvedLiteral::Int(41),
                    ..
                }
            )
        })
    }));
}

#[test]
fn checked_scalar_ffi_singleton_for_uses_only_the_verifier_owner_slice() {
    let checked = checked_program(
        r#"
            extern "C" { func positive(value: i32) -> i32 requires: value > 0; }
            func main() -> i64 {
                for item in [1] { positive(item); }
                0
            }
        "#,
    );

    let executable_route = materialize_canonical_mir_route(&checked, None)
        .expect_err("the singleton For remains outside executable MIR route admission");
    assert!(executable_route
        .to_string()
        .contains("structured control flow is not lowered by MIR Phase 0"));

    let owner = NodeId("function:main".into());
    let verifier_slice = reference::MirProgram::from_checked_program_ffi_caller(&checked, &owner)
        .expect("the exact Checker-backed verifier-only owner slice");
    assert_eq!(
        verifier_slice.purpose(),
        reference::MirProgramPurpose::VerifierOnly
    );

    let has_z3 = crate::verifier::is_z3_available();
    let results = crate::verifier::verify_ffi_checked(&checked)
        .expect("the exact singleton-For owner slice is verifier-admissible");
    assert_eq!(results.len(), 1, "main's FFI call remains visible");
    if has_z3 {
        assert_eq!(results[0].status, crate::verifier::VerifStatus::Proven);
        assert_eq!(
            results[0].artifact.as_ref().unwrap().engine,
            crate::verifier::ProofArtifact::ENGINE_MIR
        );
    } else {
        assert_eq!(
            results[0].status,
            crate::verifier::VerifStatus::InfrastructureError
        );
        assert!(results[0].artifact.is_none());
    }
}

#[test]
fn scalar_ffi_singleton_for_near_miss_keeps_the_complete_route_error() {
    let checked = checked_program(
        r#"
            extern "C" { func positive(value: i32) -> i32 requires: value > 0; }
            func main() -> i64 {
                let items: List<i32> = [1]
                for item in items { positive(item); }
                0
            }
        "#,
    );
    let owner = NodeId("function:main".into());
    assert!(reference::MirProgram::from_checked_program_ffi_caller(&checked, &owner).is_err());

    let error = crate::verifier::verify_ffi_checked(&checked)
        .expect_err("a named iterable is not eligible for the verifier-only singleton-For slice");
    assert!(
        error.contains("structured control flow is not lowered by MIR Phase 0"),
        "the complete ScalarFfi materialization failure must remain visible: {error}"
    );
}

#[test]
fn ffi_verifier_singleton_for_rejects_unsupported_shapes_and_exits() {
    let source = r#"
        extern "C" { func positive(value: i32) -> i32 requires: value > 0; }
        func many_elements(value: i32) -> i32 {
            for item in [value, value + 1] { positive(item); }
            0
        }
        func named_iterable(value: i32) -> i32 {
            let xs: List<i32> = [value]
            for item in xs { positive(item); }
            0
        }
        func multiple_body_calls(value: i32) -> i32 {
            for item in [value] { positive(item); positive(item); }
            0
        }
        func break_exit(value: i32) -> i32 {
            for item in [value] { positive(item); break; }
            0
        }
        func return_exit(value: i32) -> i32 {
            for item in [value] { return positive(item); }
            0
        }
        func main() -> i64 { 0 }
    "#;
    let checked = checked_program(source);
    let catalog = types::MirTypeCatalog::from_checked_program(&checked).expect("Checker TypeDesc");
    for name in [
        "many_elements",
        "named_iterable",
        "multiple_body_calls",
        "break_exit",
        "return_exit",
    ] {
        let owner = NodeId(format!("function:{name}"));
        let callable = checked.callable(&owner).expect("checked negative caller");
        let errors = super::lower::lower_ffi_verifier_caller(callable, &catalog, None)
            .expect_err("unsupported singleton-loop shape must remain fail-closed");
        assert!(!errors.is_empty(), "{name} unexpectedly lowered");
    }

    // The special lowering never becomes a general production lowering
    // allowance: even the admitted loop remains outside the ordinary Phase 0
    // entry point. A mutated typed body is rejected at the body-shape gate.
    let one = checked_program(
        r#"
            extern "C" { func positive(value: i32) -> i32 requires: value > 0; }
            func one(value: i32) -> i32 {
                for item in [value] { positive(item); }
                0
            }
            func main() -> i64 { 0 }
        "#,
    );
    let owner = NodeId("function:one".into());
    let mut forged = one.callable(&owner).expect("checked caller").clone();
    let ResolvedStmtKind::For { pattern, body, .. } = &mut forged.body.root.statements[0].kind
    else {
        panic!("source singleton loop");
    };
    let ResolvedPatternKind::Binding { local, .. } = &pattern.kind else {
        panic!("direct binding");
    };
    let ResolvedStmtKind::Expr(call_expression) = &body.statements[0].kind else {
        panic!("source call");
    };
    let ResolvedExprKind::Call(call) = &call_expression.kind else {
        panic!("resolved source call");
    };
    let assigned_value = call.arguments[0].value.clone();
    let conversion = call.arguments[0].conversion.clone();
    let statement = &mut body.statements[0];
    statement.kind = ResolvedStmtKind::Assign {
        target: crate::core::ir::ResolvedPlace::root(local.clone()),
        value: assigned_value,
        conversion,
    };
    let forged_catalog =
        types::MirTypeCatalog::from_checked_program(&one).expect("Checker TypeDesc");
    let errors = super::lower::lower_ffi_verifier_caller(&forged, &forged_catalog, None)
        .expect_err("an assignment to the bound item must not bypass the body gate");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("one direct FFI call expression")));
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
fn materializes_generic_nested_list_construct_with_recursive_list_glue() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_list_nested_owned.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("one-level nested List construction must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarListConstruct { .. }
            ) && instance.arguments.len() == 1
        })
        .expect("nested List construction instance");
    let argument = &instance.arguments[0];
    let argument_desc = program
        .type_catalog()
        .get(argument)
        .expect("nested List argument TypeDesc");
    assert!(matches!(
        argument_desc.layout,
        crate::core::mir::types::MirLayout::List { .. }
    ));
    program
        .type_catalog()
        .validate_move_owned_list_payload(argument)
        .expect("nested List child glue must be fully materialized");
    let nested_list_ty = program
        .type_catalog()
        .iter()
        .find_map(|(ty, descriptor)| {
            matches!(descriptor.layout, crate::core::mir::types::MirLayout::List { ref element }
                if program
                    .type_catalog()
                    .get(element)
                    .is_some_and(|child| matches!(child.layout, crate::core::mir::types::MirLayout::List { .. })))
            .then(|| ty.clone())
        })
        .expect("nested List result TypeDesc");
    assert!(
        program
            .type_catalog()
            .validate_move_owned_payload(&nested_list_ty)
            .is_err(),
        "nested List must not widen managed variant payloads"
    );
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized nested List construction target");
    assert!(target.canonical_text().contains("list_construct_contract"));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference nested List construction execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn rejects_generic_list_construct_beyond_one_nested_level_at_the_mir_gate() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_list_nested_deep_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("deep nested List construction must fail closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("one-level nested List contract")
            || message.contains("outside the canonical Copy scalar"),
        "unexpected deep nested List rejection: {message}"
    );
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
        message.contains("outside the canonical Copy scalar")
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
fn materializes_nested_list_index_with_deep_clone_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_nested_list_index.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("nested List index must lower to canonical MIR");
    let target = program
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("nested List index target");
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
    assert_eq!(receipts.len(), 2);
    assert!(receipts.iter().any(|receipt| {
        receipt.mode == crate::core::mir::types::MirListIndexProjectionMode::CloneNestedList
    }));
    assert!(receipts.iter().any(|receipt| {
        receipt.mode == crate::core::mir::types::MirListIndexProjectionMode::CopyScalar
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference nested List index execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(3));
}

#[test]
fn materializes_nested_list_reverse_with_recursive_clone_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_nested_list_reverse.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("nested List.reverse must lower to canonical MIR");
    let target = program
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("nested List.reverse target");
    let receipt = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                operation: crate::core::mir::MirListOperation::Reverse,
                list_operation_contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("nested List.reverse receipt");
    assert_eq!(
        receipt.mode,
        crate::core::mir::types::MirListOperationMode::Nested
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference nested List.reverse execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(3));
}

#[test]
fn materializes_nested_list_concat_with_child_move_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_nested_list_concat.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("nested List.concat must lower to canonical MIR");
    let target = program
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("nested List.concat target");
    let receipt = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                operation: crate::core::mir::MirListOperation::Concat,
                list_operation_contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("nested List.concat receipt");
    assert_eq!(
        receipt.mode,
        crate::core::mir::types::MirListOperationMode::Nested
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference nested List.concat execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(3));
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
        message.contains("outside the canonical Copy scalar"),
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
        parameter_permissions: None,
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
        proof_worklist: None,
        ownership: MirOwnershipSummary::default(),
    }
}

#[test]
fn valid_function_passes_structural_validation() {
    let function = fixture();
    assert!(function.validate().is_ok(), "{:?}", function.validate());
}

#[test]
fn validation_error_to_diagnostic_preserves_route_provenance_only_for_registered_codes() {
    let route_error = MirValidationError {
        subject: "extern:foreign".into(),
        message: format!(
            "{}: FFI declaration uses an unsupported variadic ABI",
            crate::core::mir::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE
        ),
    };
    let diagnostic = route_error.to_diagnostic();
    assert_eq!(
        diagnostic.code.as_deref(),
        Some(crate::core::mir::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE)
    );
    assert_eq!(
        diagnostic.origin.expect("route provenance").rule.as_deref(),
        Some("mir.route")
    );
    assert_eq!(diagnostic.span, crate::span::Span::UNKNOWN);

    let ordinary_error = MirValidationError {
        subject: "value:missing".into(),
        message: "value is used before its definition".into(),
    };
    let ordinary_diagnostic = ordinary_error.to_diagnostic();
    assert!(ordinary_diagnostic.code.is_none());
    assert!(ordinary_diagnostic.origin.is_none());
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
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("record move/drop projection receipt disagrees")
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
        loan: None,
        checker_block: None,
        checker_edge: None,
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
        loan: None,
        checker_block: None,
        checker_edge: None,
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
        loan: None,
        checker_block: None,
        checker_edge: None,
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
fn ownership_return_event_must_match_a_mir_return_value() {
    let mut function = fixture();
    function.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::Return,
        resource: "resource:result".into(),
        value: Some(MirValueId::new("v.result").unwrap()),
        source: Some("result".into()),
        target: None,
        point: NodeId("node:return".into()),
        loan: None,
        checker_block: None,
        checker_edge: None,
    });
    assert!(validate_ownership_event_receipts(&function).is_empty());
}

#[test]
fn ownership_move_event_without_a_mir_transfer_is_rejected() {
    let mut function = fixture();
    function.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::Move,
        resource: "resource:arg".into(),
        value: Some(MirValueId::new("v.arg").unwrap()),
        source: Some("arg".into()),
        target: Some("consumed".into()),
        point: NodeId("node:move".into()),
        loan: None,
        checker_block: None,
        checker_edge: None,
    });
    let errors = validate_ownership_event_receipts(&function);
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("move event value 'v.arg' has no matching canonical MIR transfer boundary")
    }));
}

#[test]
fn checker_loan_start_is_bound_to_mir_and_end_receipt_by_identity() {
    let (canonical, owner, function) = synthetic_loan_receipt_fixture();
    let checker_function = canonical.functions().get(&owner).expect("checker MIR");
    let checker_start = checker_function
        .ownership
        .events
        .iter()
        .find(|event| {
            matches!(
                event.kind,
                MirOwnershipEventKind::BorrowShared | MirOwnershipEventKind::BorrowMut
            )
        })
        .expect("checker borrow-start receipt");
    let checker_loan = checker_start.loan.as_ref().expect("checker loan id");
    assert!(!checker_function
        .ownership
        .events
        .iter()
        .any(|event| event.kind == MirOwnershipEventKind::BorrowEnd));
    assert!(checker_function
        .ownership
        .checker_loans
        .get(checker_loan)
        .expect("independent checker loan contract")
        .ends
        .is_empty());
    let rebuilt = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner.clone(), function.clone())]),
        canonical.type_catalog().clone(),
    )
    .expect("paired checker loan receipts should validate");
    let function = rebuilt.functions().get(&owner).expect("rebuilt MIR");
    let borrow = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Borrow {
                loan,
                mutable,
                checker_resource,
                checker_source,
                ..
            } => Some((loan, mutable, checker_resource, checker_source)),
            _ => None,
        })
        .expect("MIR borrow operation");
    let starts = function
        .ownership
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                MirOwnershipEventKind::BorrowShared | MirOwnershipEventKind::BorrowMut
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].loan.as_ref(), Some(borrow.0));
    assert_eq!(
        starts[0].kind == MirOwnershipEventKind::BorrowMut,
        *borrow.1
    );
    assert_eq!(borrow.2.as_deref(), Some(starts[0].resource.as_str()));
    assert_eq!(borrow.3.as_deref(), starts[0].source.as_deref());
    assert!(starts[0].checker_block.is_some());

    let ends = function
        .ownership
        .events
        .iter()
        .filter(|event| event.kind == MirOwnershipEventKind::BorrowEnd)
        .collect::<Vec<_>>();
    assert_eq!(
        ends.len(),
        1,
        "the fixture carries one synthetic end receipt"
    );
    assert!(ends
        .iter()
        .all(|event| event.loan.as_ref() == starts[0].loan.as_ref()));
    assert!(ends.iter().all(|event| event.checker_block.is_some()));
    assert!(function.canonical_text().contains(&format!(
        "loan={} checker_block=",
        starts[0].loan.as_ref().expect("loan id").0
    )));
}

fn synthetic_loan_receipt_fixture() -> (crate::core::mir::reference::MirProgram, NodeId, MirFunction)
{
    let checked = checked_program(
        "func borrow_receipt(value: i64) -> i64 { *(&value) }\nfunc main() -> i64 { borrow_receipt(0) }",
    );
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("single-expression borrow MIR");
    let owner = NodeId("function:borrow_receipt".into());
    let function = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("borrow helper MIR");
    let start = function
        .ownership
        .events
        .iter()
        .find(|event| {
            matches!(
                event.kind,
                MirOwnershipEventKind::BorrowShared | MirOwnershipEventKind::BorrowMut
            )
        })
        .cloned()
        .expect("checker borrow-start receipt");
    // End actions are synthesized here to exercise identity pairing. Their
    // point/block/edge are test data, so this test makes no claim about
    // Checker-CFG to MIR-CFG path or timing equivalence.
    let mut end = start;
    end.kind = MirOwnershipEventKind::BorrowEnd;
    end.point = NodeId("synthetic/end-point".into());
    end.checker_block = Some(NodeId("synthetic/end-block".into()));
    end.checker_edge = Some(NodeId("synthetic/end-edge".into()));
    let mut function = function;
    let loan = end.loan.clone().expect("loan identity");
    function
        .ownership
        .checker_loans
        .get_mut(&loan)
        .expect("independent checker loan contract")
        .ends
        .push(super::MirCheckerLoanEndSite {
            location: super::MirCheckerLoanLocation {
                block: end.checker_block.clone().expect("checker block"),
                point: end.point.clone(),
                edge: end.checker_edge.clone(),
            },
            resource: end.resource.clone(),
            value: end.value.clone(),
            source: end.source.clone(),
        });
    function.ownership.events.push(end);
    (canonical, owner, function)
}

#[test]
fn forged_or_duplicate_checker_loan_end_receipts_are_rejected_before_consumers() {
    let (canonical, owner, original) = synthetic_loan_receipt_fixture();
    let rebuild = |function| {
        crate::core::mir::reference::MirProgram::with_type_catalog(
            BTreeMap::from([(owner.clone(), function)]),
            canonical.type_catalog().clone(),
        )
    };

    let mut missing_start = original.clone();
    missing_start.ownership.events.retain(|event| {
        !matches!(
            event.kind,
            MirOwnershipEventKind::BorrowShared | MirOwnershipEventKind::BorrowMut
        )
    });
    let errors = rebuild(missing_start).expect_err("MIR borrow needs a checker-owned start");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("has no checker borrow-start receipt")
    }));

    let mut missing_all_ends = original.clone();
    missing_all_ends
        .ownership
        .events
        .retain(|event| event.kind != MirOwnershipEventKind::BorrowEnd);
    let errors = rebuild(missing_all_ends)
        .expect_err("all checker-owned end sites must be represented in the event stream");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("borrow-end receipts do not match the checker-owned loan contract")
    }));

    let mut wrong_kind = original.clone();
    let start = wrong_kind
        .ownership
        .events
        .iter_mut()
        .find(|event| event.kind == MirOwnershipEventKind::BorrowShared)
        .expect("shared borrow receipt");
    start.kind = MirOwnershipEventKind::BorrowMut;
    let errors = rebuild(wrong_kind).expect_err("checker mutability must match the MIR op");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("no matching canonical MIR Borrow operation")
    }));

    let mut jointly_forged_place = original.clone();
    let start = jointly_forged_place
        .ownership
        .events
        .iter_mut()
        .find(|event| event.kind == MirOwnershipEventKind::BorrowShared)
        .expect("shared borrow receipt");
    start.resource = "forged/nonlocal-resource".into();
    start.value = None;
    start.source = Some("forged.source.place".into());
    for instruction in jointly_forged_place
        .blocks
        .values_mut()
        .flat_map(|block| &mut block.instructions)
    {
        if let MirInstructionKind::Borrow {
            checker_resource,
            checker_source,
            ..
        } = &mut instruction.kind
        {
            *checker_resource = Some("forged/nonlocal-resource".into());
            *checker_source = Some("forged.source.place".into());
        }
    }
    let errors = rebuild(jointly_forged_place).expect_err(
        "coordinated place-string edits must disagree with the private Checker contract",
    );
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("no matching canonical MIR Borrow operation")
    }));

    let mut missing_local_value = original.clone();
    let start = missing_local_value
        .ownership
        .events
        .iter_mut()
        .find(|event| event.kind == MirOwnershipEventKind::BorrowShared)
        .expect("shared borrow receipt");
    start.value = None;
    let errors = rebuild(missing_local_value)
        .expect_err("a local checker resource must retain its source MIR value");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("no matching canonical MIR Borrow operation")
    }));

    let mut mismatched_local_resource = original.clone();
    let start = mismatched_local_resource
        .ownership
        .events
        .iter_mut()
        .find(|event| event.kind == MirOwnershipEventKind::BorrowShared)
        .expect("shared borrow receipt");
    start.resource = "forged/node/local".into();
    let errors = rebuild(mismatched_local_resource)
        .expect_err("local resource and stable value identity must agree");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("no matching canonical MIR Borrow operation")
    }));

    let mut mismatched_source_place = original.clone();
    let start = mismatched_source_place
        .ownership
        .events
        .iter_mut()
        .find(|event| event.kind == MirOwnershipEventKind::BorrowShared)
        .expect("shared borrow receipt");
    start.source = Some("forged.source.place".into());
    let errors = rebuild(mismatched_source_place)
        .expect_err("checker place source must match its canonical Borrow boundary");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("no matching canonical MIR Borrow operation")
    }));

    let mut jointly_forged_identity = original.clone();
    let forged_loan = NodeId("forged/loan-id".into());
    for event in &mut jointly_forged_identity.ownership.events {
        if matches!(
            event.kind,
            MirOwnershipEventKind::BorrowShared
                | MirOwnershipEventKind::BorrowMut
                | MirOwnershipEventKind::BorrowEnd
        ) {
            event.loan = Some(forged_loan.clone());
        }
    }
    for instruction in jointly_forged_identity
        .blocks
        .values_mut()
        .flat_map(|block| &mut block.instructions)
    {
        if let MirInstructionKind::Borrow { loan, .. } = &mut instruction.kind {
            *loan = forged_loan.clone();
        }
    }
    let errors = rebuild(jointly_forged_identity.clone())
        .expect_err("matching forged receipts cannot replace the checker identity rule");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("loan identity is not derived from its resolved point")
    }));
    let operation_errors = super::validate_ownership_event_receipts(&jointly_forged_identity);
    assert!(operation_errors.iter().any(|error| {
        error
            .message
            .contains("canonical MIR Borrow loan identity is not derived")
    }));

    let mut unknown_loan = original.clone();
    let end = unknown_loan
        .ownership
        .events
        .iter_mut()
        .find(|event| event.kind == MirOwnershipEventKind::BorrowEnd)
        .expect("borrow-end receipt");
    end.loan = Some(NodeId("forged/loan".into()));
    let errors = rebuild(unknown_loan).expect_err("borrow end cannot invent a loan");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("names unknown loan 'forged/loan'")));

    let mut wrong_source = original.clone();
    let end = wrong_source
        .ownership
        .events
        .iter_mut()
        .find(|event| event.kind == MirOwnershipEventKind::BorrowEnd)
        .expect("borrow-end receipt");
    end.source = Some("unrelated.place".into());
    let errors = rebuild(wrong_source).expect_err("borrow end source must preserve loan place");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("disagrees with its checker borrow start")
    }));

    let mut missing_end_source = original.clone();
    let end = missing_end_source
        .ownership
        .events
        .iter_mut()
        .find(|event| event.kind == MirOwnershipEventKind::BorrowEnd)
        .expect("borrow-end receipt");
    end.source = None;
    let errors = rebuild(missing_end_source)
        .expect_err("end source omission must disagree with the checker-owned expected site");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("borrow-end receipts do not match the checker-owned loan contract")
    }));

    let mut forged_end_location = original.clone();
    let end = forged_end_location
        .ownership
        .events
        .iter_mut()
        .find(|event| event.kind == MirOwnershipEventKind::BorrowEnd)
        .expect("borrow-end receipt");
    end.checker_block = Some(NodeId("forged/nonexistent-block".into()));
    end.checker_edge = Some(NodeId("forged/nonexistent-edge".into()));
    let errors = rebuild(forged_end_location)
        .expect_err("end CFG location must match the independent checker contract");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("borrow-end receipts do not match the checker-owned loan contract")
    }));

    let mut duplicate_end = original.clone();
    let end = duplicate_end
        .ownership
        .events
        .iter()
        .find(|event| event.kind == MirOwnershipEventKind::BorrowEnd)
        .expect("borrow-end receipt")
        .clone();
    duplicate_end.ownership.events.push(end);
    let errors = rebuild(duplicate_end).expect_err("duplicate edge receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("duplicate checker borrow-end receipt")
    }));

    let mut drifted_operation = original;
    let borrow = drifted_operation
        .blocks
        .values_mut()
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::Borrow { loan, .. } => Some(loan),
            _ => None,
        })
        .expect("MIR borrow operation");
    *borrow = NodeId("forged/operation-loan".into());
    let errors = rebuild(drifted_operation).expect_err("borrow operation loan identity must match");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("no matching canonical MIR Borrow operation")
    }));
}

#[test]
fn checker_generated_borrow_end_sites_are_bound_at_public_function_validation() {
    let checked = checked_program(
        r#"
func read(value: i32) -> i32 {
    println(*(&value))
    value
}
func main() -> i32 { read(7) }
"#,
    );
    let owner = NodeId("function:read".into());
    let analysis = checked
        .resource_analysis(&owner)
        .expect("checker resource analysis");
    let end_actions = analysis
        .actions
        .iter()
        .filter(|action| action.kind == crate::core::CanonicalActionKind::BorrowEnd)
        .collect::<Vec<_>>();
    assert!(
        !end_actions.is_empty(),
        "fixture must exercise checker-generated BorrowEnd actions"
    );
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("checker end sites lower to canonical MIR receipts");
    let function = program.functions().get(&owner).expect("read MIR function");
    assert_eq!(
        function
            .ownership
            .events
            .iter()
            .filter(|event| event.kind == MirOwnershipEventKind::BorrowEnd)
            .count(),
        end_actions.len()
    );
    for action in &end_actions {
        let loan = action.loan.as_ref().expect("checker end loan");
        let expected = super::MirCheckerLoanEndSite {
            location: super::MirCheckerLoanLocation {
                block: action.location.block.0.clone(),
                point: action.location.point.clone(),
                edge: action.location.edge.as_ref().map(|edge| edge.0.clone()),
            },
            resource: action.resource.0 .0.clone(),
            value: action.resource.0 .0.ends_with("/local").then(|| {
                crate::core::mir::MirValueId::new(format!("local:{}", action.resource.0 .0))
                    .expect("derived local value ID")
            }),
            source: action.source.as_ref().map(|place| place.display()),
        };
        assert!(function.ownership.checker_loans[&loan.0]
            .ends
            .contains(&expected));
        assert!(function.ownership.events.iter().any(|event| {
            event.kind == MirOwnershipEventKind::BorrowEnd
                && event.loan.as_ref() == Some(&loan.0)
                && event.checker_block.as_ref() == Some(&expected.location.block)
                && event.point == expected.location.point
                && event.checker_edge == expected.location.edge
                && event.resource == expected.resource
                && event.value == expected.value
                && event.source == expected.source
        }));
    }

    let mut missing_end = function.clone();
    missing_end
        .ownership
        .events
        .retain(|event| event.kind != MirOwnershipEventKind::BorrowEnd);
    let errors = missing_end
        .validate()
        .expect_err("public per-function validation rejects omitted checker end sites");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("borrow-end receipts do not match the checker-owned loan contract")
    }));
}

#[test]
fn consuming_variant_payload_uses_the_canonical_drop_plan() {
    let source = r#"
func make_owned_option() -> Option<string> { Some("owned") }
func main() -> i32 {
    let value = make_owned_option()
    drop(value)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned variant glue must be materialized");
    let main = canonical
        .functions()
        .get(&crate::core::NodeId("function:make_owned_option".into()))
        .expect("option-producing helper MIR");
    let instruction = main
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::ConstructVariantMove {
                result,
                nominal,
                variant,
                fields,
            } => Some((result, nominal, variant, fields)),
            _ => None,
        })
        .expect("move variant construction");
    let result_ty = &main.values.get(instruction.0).expect("result value").ty;
    let field_ids = instruction
        .3
        .iter()
        .map(|(field, _)| field.clone())
        .collect::<Vec<_>>();
    let field_types = instruction
        .3
        .iter()
        .map(|(_, value)| main.values.get(value).expect("payload value").ty.clone())
        .collect::<Vec<_>>();
    canonical
        .type_catalog()
        .validate_variant_move_construct(
            result_ty,
            instruction.1,
            instruction.2,
            &field_ids,
            &field_types,
        )
        .expect("variant payload/drop plan must agree");
}

#[test]
fn consuming_variant_payload_identity_drift_is_rejected_before_consumers() {
    let source = r#"
func make_owned_option() -> Option<string> { Some("owned") }
func main() -> i32 {
    let value = make_owned_option()
    drop(value)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned variant glue must be materialized");
    let owner = crate::core::NodeId("function:make_owned_option".into());
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
                crate::core::mir::MirInstructionKind::ConstructVariantMove { .. }
            )
        })
        .expect("move variant construction");
    let crate::core::mir::MirInstructionKind::ConstructVariantMove { fields, .. } =
        &mut instruction.kind
    else {
        unreachable!();
    };
    fields[0].0 = crate::core::NodeId("builtin:variant:Option::Some/payload:drift".into());
    let errors = validate_variant_move_payloads(&forged, canonical.type_catalog());
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("canonical variant move payload contract failed")
    }));
}

#[test]
fn flow_move_receipt_reaches_the_consuming_transition_argument() {
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(
            include_str!("../../../tests/fixtures/mir_native_flow_transition.mimi"),
        ))
        .expect("silent-local Flow transition must remain admitted");
    let owner = crate::core::NodeId("function:main".into());
    let main = canonical.functions().get(&owner).expect("main MIR");
    assert!(validate_transfer_event_boundaries(main, canonical.transitions()).is_empty());
    let flow = main
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::FlowTransition { .. }
            )
        })
        .expect("FlowTransition instruction");
    let crate::core::mir::MirInstructionKind::FlowTransition { arguments, .. } = &flow.kind else {
        unreachable!();
    };
    assert!(main.values.contains_key(&arguments[0]));
}

#[test]
fn flow_transition_argument_arity_is_checked_against_contract() {
    let source = r#"
flow Counter {
    state Zero { n: i32 }
    state Active { n: i32 }
    transition inc(Zero, amount: i32) -> Active {
        return Active { n: self.n + amount }
    }
}
func main() -> i32 {
    let c = Zero { n: 41 }
    let active = Counter::inc(c, 1)
    active.n
}
"#;
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("parameterized cross-state Flow lowers to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut functions = canonical.functions().clone();
    let main = functions.get_mut(&owner).expect("main MIR");
    let flow_transition = main
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::FlowTransition { .. }))
        .expect("parameterized FlowTransition instruction");
    let MirInstructionKind::FlowTransition { arguments, .. } = &mut flow_transition.kind else {
        unreachable!();
    };
    assert_eq!(arguments.len(), 2, "source state + event parameter");
    arguments.pop();

    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            canonical.type_catalog().clone(),
            canonical.instances().clone(),
            canonical.transitions().clone(),
        )
        .expect_err("canonical validation must reject an omitted Flow event argument");
    assert!(
        errors.iter().any(|error| {
            error
                .message
                .contains("FlowTransition argument arity disagrees with its contract")
        }),
        "{errors:?}"
    );
}

#[test]
fn flow_move_receipt_point_drift_is_rejected_before_consumers() {
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(
            include_str!("../../../tests/fixtures/mir_native_flow_transition.mimi"),
        ))
        .expect("silent-local Flow transition must remain admitted");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("main MIR");
    let event = forged
        .ownership
        .events
        .iter_mut()
        .find(|event| event.kind == MirOwnershipEventKind::Move)
        .expect("Flow move receipt");
    event.point = crate::core::NodeId("function:main/node:forged-flow-point".into());
    let errors = validate_transfer_event_boundaries(&forged, canonical.transitions());
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("has no canonical call/flow boundary at point")
    }));
}

#[test]
fn flow_session_effect_receipt_cannot_be_attached_to_silent_local_transition() {
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(
            include_str!("../../../tests/fixtures/mir_native_flow_transition.mimi"),
        ))
        .expect("silent-local Flow transition must remain admitted");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("main MIR");
    let source_event = forged
        .ownership
        .events
        .iter()
        .find(|event| event.kind == MirOwnershipEventKind::Move)
        .cloned()
        .expect("Flow move receipt");
    forged.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::TransferSession,
        resource: source_event.resource,
        value: source_event.value,
        source: source_event.source,
        target: source_event.target,
        point: source_event.point,
        loan: None,
        checker_block: None,
        checker_edge: None,
    });
    let errors = validate_transfer_event_boundaries(&forged, canonical.transitions());
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("disagrees with SilentLocal FlowTransition effect")
    }));
}

#[test]
fn concrete_owned_call_argument_uses_a_canonical_move_boundary() {
    let source = r#"
func take(value: Option<string>) -> i32 {
    drop(value)
    41
}
func main() -> i32 {
    let value = Some("owned")
    take(value)
}
"#;
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("concrete owned call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let main = canonical.functions().get(&owner).expect("main MIR");
    let call_argument = main
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call { arguments, .. } => arguments.first().cloned(),
            _ => None,
        })
        .expect("direct call argument");
    assert!(main.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Move { result, .. } if result == &call_argument
            )
        })
    }));
    crate::interp::bytecode::compile_mir_program(&canonical)
        .expect("bytecode adapter must consume the admitted call receipt");
}

#[test]
fn borrowed_function_argument_keeps_the_source_outside_the_move_boundary() {
    let source = r#"
func inspect(data: view List<i32>) -> i32 {
    len(data)
}
func main() -> i32 {
    let data = [1, 2]
    let n = inspect(data)
    drop(data)
    n
}
"#;
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("borrowed direct call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let main = canonical.functions().get(&owner).expect("main MIR");
    let (call_argument, call_point) = main
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call { arguments, .. } => Some((
                arguments.first().cloned().expect("call argument"),
                instruction.id.clone(),
            )),
            _ => None,
        })
        .expect("borrowed direct call");
    let has_move = main.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Move { result, .. } if result == &call_argument
            )
        })
    });
    assert!(
        !has_move,
        "view parameter must not consume the source local"
    );
    assert!(call_point.as_str().starts_with("inst:call:"));
}

#[test]
fn owned_call_direction_receipt_rejects_a_forged_clone() {
    let source = r#"
func take(value: Option<string>) -> i32 {
    drop(value)
    41
}
func main() -> i32 {
    let value = Some("owned")
    take(value)
}
"#;
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("concrete owned call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical.functions().clone();
    let call_argument = forged
        .get(&owner)
        .expect("main MIR")
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call { arguments, .. } => arguments.first().cloned(),
            _ => None,
        })
        .expect("direct call argument");
    for block in forged
        .get_mut(&owner)
        .expect("main MIR")
        .blocks
        .values_mut()
    {
        for instruction in &mut block.instructions {
            if let MirInstructionKind::Move { result, source } = &instruction.kind {
                if result == &call_argument {
                    instruction.kind = MirInstructionKind::Clone {
                        result: result.clone(),
                        source: source.clone(),
                    };
                }
            }
        }
    }
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            forged,
            canonical.type_catalog().clone(),
            BTreeMap::new(),
            canonical.transitions().clone(),
        )
        .expect_err("owned call Clone must fail the direction receipt");
    assert!(errors
        .iter()
        .any(|error| { error.message.contains("lacks a canonical Move producer") }));
}

#[test]
fn call_move_receipt_rejects_argument_identity_drift() {
    let source = r#"
func take(value: Option<string>) -> i32 {
    drop(value)
    41
}
func main() -> i32 {
    let value = Some("owned")
    take(value)
}
"#;
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("concrete owned call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("main MIR");
    let (call_instruction, original_argument) = forged
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call { arguments, .. } => Some((
                instruction.id.clone(),
                arguments.first().cloned().expect("call argument"),
            )),
            _ => None,
        })
        .expect("direct call");
    let call_point = call_instruction
        .as_str()
        .split_once(':')
        .and_then(|(_, rest)| rest.split_once(':'))
        .map(|(_, point)| crate::core::NodeId(point.to_owned()))
        .expect("call instruction point");
    let replacement = forged
        .values
        .keys()
        .find(|value| **value != original_argument)
        .cloned()
        .expect("replacement value");
    for block in forged.blocks.values_mut() {
        for instruction in &mut block.instructions {
            if instruction.id == call_instruction {
                let MirInstructionKind::Call { arguments, .. } = &mut instruction.kind else {
                    unreachable!("call instruction identity changed");
                };
                arguments[0] = replacement.clone();
            }
        }
    }
    let source_value = forged
        .values
        .keys()
        .find(|value| {
            value
                .as_str()
                .starts_with("local:function:main/node:pattern.variable@")
        })
        .cloned()
        .expect("source local identity");
    forged.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::Move,
        resource: "function:main/node:pattern.variable:value/local".into(),
        value: Some(source_value),
        source: Some("value".into()),
        target: None,
        point: call_point,
        loan: None,
        checker_block: None,
        checker_edge: None,
    });
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            BTreeMap::from([(owner, forged)]),
            canonical.type_catalog().clone(),
            BTreeMap::new(),
            canonical.transitions().clone(),
        )
        .expect_err("call argument identity drift must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("does not reach a canonical call/flow argument at point")
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
    assert!(canonical.functions().values().any(|function| {
        function
            .canonical_text()
            .contains("list_index=MirListIndexProjectionContract")
    }));
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
    let source = "func main() -> i32 { let values = [10, 20, 30]; let count = len(values); drop(values); count }";
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
    let source = r#"
func make_reversed_values() -> List<i32> {
    let values = [1, 2, 3]
    let reversed = reverse(values)
    drop(values)
    reversed
}
func main() -> i32 {
    let reversed = make_reversed_values()
    drop(reversed)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:make_reversed_values".into());
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
    let source = r#"
func make_reversed_values_method() -> List<i32> {
    let values = [1, 2, 3]
    let reversed = values.reverse()
    drop(values)
    reversed
}
func main() -> i32 {
    let reversed = make_reversed_values_method()
    drop(reversed)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:make_reversed_values_method".into());
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
    let source = r#"
func concatenate_values() -> List<i32> {
    let left = [1, 2]
    let right = [3, 4]
    let joined = left.concat(right)
    joined
}
func main() -> i32 {
    let joined = concatenate_values()
    drop(joined)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:concatenate_values".into());
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
    let source = r#"
func concatenate_values() -> List<i32> {
    let left = [1]
    let right = [2]
    let joined = left.concat(right)
    joined
}
func main() -> i32 {
    let joined = concatenate_values()
    drop(joined)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:concatenate_values".into());
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
    let source = r#"
func aliasing_concat() -> List<i32> {
    let values = [1, 2]
    let joined = values.concat(values)
    joined
}
func main() -> i32 {
    let joined = aliasing_concat()
    drop(joined)
    0
}
"#;
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
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("List operation has no canonical receipt")
    }));

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
fn canonical_nested_concat_receipt_rejects_forged_scalar_mode() {
    let source = include_str!("../../../tests/fixtures/mir_native_nested_list_concat.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical nested List.concat MIR");
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
        .expect("nested List.concat operation");
    let MirInstructionKind::ListOp {
        list_operation_contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!()
    };
    receipt.mode = crate::core::mir::types::MirListOperationMode::Scalar;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        std::collections::BTreeMap::from([(owner, forged)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("nested List.concat scalar mode must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("List operation receipt disagrees with TypeDesc")
    }));
}

#[test]
fn non_copy_record_projection_lowers_to_explicit_move_project() {
    let source = r#"
type Named { name: string, count: i32 }
func project_name() -> string {
    let p = Named { name: "owned", count: 41 }
    p.name
}
func main() -> i32 {
    let name = project_name()
    drop(name)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:project_name".into());
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
fn managed_generic_list_projection_fails_closed_before_consumers() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_list_projection_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("managed generic List projection must fail before consumers");
    let text = format!("{error:?}");
    assert!(
        text.contains("outside scalar contract"),
        "unexpected fail-closed error: {text}"
    );
}

#[test]
fn non_copy_tuple_materializes_field_drop_schedule_before_backend() {
    let source = r#"
func make_owned_pair() -> (string, i32) { ("owned", 41) }
func main() -> i32 {
    let pair = make_owned_pair()
    drop(pair)
    0
}
"#;
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
    let source = r#"
func make_owned_option() -> Option<string> { Some("owned") }
func main() -> i32 {
    let value = make_owned_option()
    drop(value)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned variant glue must be materialized");
    let main = canonical
        .functions()
        .get(&crate::core::NodeId("function:make_owned_option".into()))
        .expect("option-producing helper MIR");
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
    let source = r#"
func make_owned_option() -> Option<string> { Some("owned") }
func main() -> i32 {
    let value = make_owned_option()
    drop(value)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:make_owned_option".into());
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
    let source = r#"
func make_owned_pair() -> (string, i32) { ("owned", 41) }
func main() -> i32 {
    let pair = make_owned_pair()
    drop(pair)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:make_owned_pair".into());
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
    let source = r#"
func make_owned_pair() -> (string, i32) { ("owned", 41) }
func main() -> i32 {
    let pair = make_owned_pair()
    drop(pair)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:make_owned_pair".into());
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
fn direct_move_owned_result_list_calls_materialize_signature_receipts() {
    let source = include_str!("../../../tests/fixtures/mir_result_list_i32_call_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned Result<List<i32>, i32> calls must lower");
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
        let payload = canonical
            .type_catalog()
            .get(&receipt.payload_types[0])
            .expect("List payload TypeDesc");
        assert!(matches!(
            payload.kind,
            crate::core::mir::types::MirTypeKind::List
        ));
        canonical
            .type_catalog()
            .validate_variant_call_abi_receipt(
                &receipt.callee,
                &receipt.type_arguments,
                &receipt.parameter_types,
                &receipt.result_ty,
                &receipt,
            )
            .expect("List move-owned call receipt must be TypeDesc-derived");
    }
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference move-owned Result<List<i32>, i32> call execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(48));
}

#[test]
fn direct_move_owned_result_list_i64_bool_calls_share_the_same_scalar_contract() {
    let source = include_str!("../../../tests/fixtures/mir_result_list_i64_bool_call_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned Result<List<i64|bool>, i32> calls must lower");
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
        assert_eq!(receipt.callee, callee);
        assert_eq!(
            receipt.mode,
            crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
        );
        assert_eq!(
            receipt.return_mode,
            crate::core::mir::types::MirVariantCallReturnMode::OwnershipPathExclusiveMerge
        );
        canonical
            .type_catalog()
            .validate_variant_call_abi_receipt(
                &receipt.callee,
                &receipt.type_arguments,
                &receipt.parameter_types,
                &receipt.result_ty,
                &receipt,
            )
            .expect("List<i64|bool> call receipt must be TypeDesc-derived");
        let payload = canonical
            .type_catalog()
            .get(&receipt.payload_ty)
            .expect("managed List payload TypeDesc");
        assert!(matches!(
            payload.kind,
            crate::core::mir::types::MirTypeKind::List
        ));
    }
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Result<List<i64|bool>, i32> call execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(48));
}

#[test]
fn direct_move_owned_result_list_i64_bool_err_paths_preserve_residual_ownership() {
    let source =
        include_str!("../../../tests/fixtures/mir_result_list_i64_bool_err_call_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned Result<List<i64|bool>, i32> Err paths must lower");
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
    assert_eq!(calls.len(), 4);
    for (callee, receipt) in calls {
        assert_eq!(receipt.callee, callee);
        assert_eq!(
            receipt.mode,
            crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
        );
        assert_eq!(
            receipt.return_mode,
            crate::core::mir::types::MirVariantCallReturnMode::OwnershipPathExclusiveMerge
        );
        canonical
            .type_catalog()
            .validate_variant_call_abi_receipt(
                &receipt.callee,
                &receipt.type_arguments,
                &receipt.parameter_types,
                &receipt.result_ty,
                &receipt,
            )
            .expect("Err-path receipt must be TypeDesc-derived");
    }
    crate::core::mir::validate_managed_result_call_island(&canonical)
        .expect("Err-path managed Result island validation");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Err-path execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(56));
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

#[test]
fn materializes_generic_record_f64_projection_with_float_field_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_record_projection_f64.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Record<T=f64> projection must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarRecordProjection { contract }
                    if contract.arity == 1 && contract.name == "value"
            )
        })
        .expect("generic Record<f64> projection instance");
    let function = program
        .functions()
        .get(&instance.function)
        .expect("generic Record<f64> executable instance");
    let parameter = function.parameters.first().expect("record parameter");
    let parameter_ty = &function
        .values
        .get(parameter)
        .expect("parameter TypeDesc")
        .ty;
    program
        .type_catalog()
        .validate_flat_copy_record_with_float(parameter_ty, true)
        .expect("specialized Record<f64> must carry the float Copy layout");
    let MirGenericInstanceContract::ScalarRecordProjection { contract } = &instance.contract else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.field_ty, function.result);
    assert!(matches!(
        program
            .type_catalog()
            .get(&contract.field_ty)
            .map(|descriptor| descriptor.abi),
        Some(crate::core::mir::types::MirAbiClass::Float { bits: 64 })
    ));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Record<f64> projection execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn materializes_generic_record_owned_list_projection_with_list_glue_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_list_projection.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Record<List<i32>> projection must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::OwnedRecordProjection { contract }
                    if contract.arity == 1 && contract.name == "value"
            )
        })
        .expect("owned generic Record<List<i32>> projection instance");
    let function = program
        .functions()
        .get(&instance.function)
        .expect("owned generic Record<List<i32>> executable instance");
    let parameter = function.parameters.first().expect("record parameter");
    let parameter_ty = &function
        .values
        .get(parameter)
        .expect("record parameter TypeDesc")
        .ty;
    let record = program
        .type_catalog()
        .get(parameter_ty)
        .expect("record TypeDesc");
    let crate::core::mir::types::MirLayout::Record { fields, .. } = &record.layout else {
        panic!("specialized generic record must carry a record layout");
    };
    assert_eq!(fields.len(), 1);
    let field = program
        .type_catalog()
        .get(&fields[0].ty)
        .expect("List field TypeDesc");
    assert_eq!(
        field.glue.move_out,
        crate::core::mir::types::MirGlueKind::List
    );
    assert_eq!(field.glue.clone, crate::core::mir::types::MirGlueKind::List);
    assert_eq!(field.glue.drop, crate::core::mir::types::MirGlueKind::List);
    assert!(matches!(
        field.layout,
        crate::core::mir::types::MirLayout::List { .. }
    ));
    assert!(function.canonical_text().contains("move_project"));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Record<List<i32>> projection execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn materializes_generic_record_owned_list_projection_with_string_residual_drop_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_list_projection_residual.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Record<List<i32>> residual projection must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
                    if contract.projection.arity == 2
                        && contract.projection.name == "value"
                        && contract.residual.len() == 1
                        && contract.residual[0].name == "note"
                        && contract.residual[0].glue
                            == crate::core::mir::types::MirGlueKind::OwnedString
            )
        })
        .expect("owned generic Record<List<i32>> residual projection instance");
    let target = program
        .functions()
        .get(&instance.function)
        .expect("owned generic Record<List<i32>> residual target");
    assert!(target.canonical_text().contains("move_project_drop"));
    let result_desc = program
        .type_catalog()
        .get(&target.result)
        .expect("selected List result TypeDesc");
    assert!(matches!(
        result_desc.layout,
        crate::core::mir::types::MirLayout::List { .. }
    ));
    assert_eq!(
        result_desc.glue,
        crate::core::mir::types::MirGlueContract {
            move_out: crate::core::mir::types::MirGlueKind::List,
            clone: crate::core::mir::types::MirGlueKind::List,
            drop: crate::core::mir::types::MirGlueKind::List,
        }
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Record<List<i32>> residual execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn materializes_generic_record_owned_list_projection_with_list_residual_drop_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_list_projection_list_residual.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Record<List<i32>> List residual projection must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
                    if contract.projection.arity == 2
                        && contract.projection.name == "value"
                        && contract.residual.len() == 1
                        && contract.residual[0].name == "tail"
                        && contract.residual[0].glue
                            == crate::core::mir::types::MirGlueKind::List
            )
        })
        .expect("owned generic Record<List<i32>> List residual projection instance");
    let target = program
        .functions()
        .get(&instance.function)
        .expect("owned generic Record<List<i32>> List residual target");
    assert!(target.canonical_text().contains("move_project_drop"));
    let result_desc = program
        .type_catalog()
        .get(&target.result)
        .expect("selected List result TypeDesc");
    assert!(matches!(
        result_desc.layout,
        crate::core::mir::types::MirLayout::List { .. }
    ));
    assert_eq!(
        result_desc.glue,
        crate::core::mir::types::MirGlueContract {
            move_out: crate::core::mir::types::MirGlueKind::List,
            clone: crate::core::mir::types::MirGlueKind::List,
            drop: crate::core::mir::types::MirGlueKind::List,
        }
    );
    let parameter = target.parameters.first().expect("record parameter");
    let record_ty = &target
        .values
        .get(parameter)
        .expect("record parameter TypeDesc")
        .ty;
    let record_desc = program
        .type_catalog()
        .get(record_ty)
        .expect("specialized record TypeDesc");
    let crate::core::mir::types::MirLayout::Record { fields, .. } = &record_desc.layout else {
        panic!("specialized generic record must carry a record layout");
    };
    let residual_desc = program
        .type_catalog()
        .get(&fields[1].ty)
        .expect("List residual TypeDesc");
    assert!(matches!(
        residual_desc.layout,
        crate::core::mir::types::MirLayout::List { .. }
    ));
    assert_eq!(
        residual_desc.glue,
        crate::core::mir::types::MirGlueContract {
            move_out: crate::core::mir::types::MirGlueKind::List,
            clone: crate::core::mir::types::MirGlueKind::List,
            drop: crate::core::mir::types::MirGlueKind::List,
        }
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Record<List<i32>> List residual execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn materializes_generic_record_owned_list_projection_with_two_list_residual_drop_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_list_projection_two_list_residual.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Record<List<i32>> two-List residual projection must lower to MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
                    if contract.projection.arity == 3
                        && contract.projection.name == "value"
                        && contract.residual.len() == 2
                        && contract.residual[0].name == "spare"
                        && contract.residual[0].glue
                            == crate::core::mir::types::MirGlueKind::List
                        && contract.residual[1].name == "tail"
                        && contract.residual[1].glue
                            == crate::core::mir::types::MirGlueKind::List
            )
        })
        .expect("owned generic Record<List<i32>> two-List residual instance");
    let target = program
        .functions()
        .get(&instance.function)
        .expect("owned generic two-List residual target");
    assert!(target.canonical_text().contains("move_project_drop"));
    let parameter = target.parameters.first().expect("record parameter");
    let record_ty = &target
        .values
        .get(parameter)
        .expect("record parameter TypeDesc")
        .ty;
    let record_desc = program
        .type_catalog()
        .get(record_ty)
        .expect("specialized record TypeDesc");
    let crate::core::mir::types::MirLayout::Record { fields, .. } = &record_desc.layout else {
        panic!("specialized generic record must carry a record layout");
    };
    assert_eq!(fields.len(), 3);
    for field in fields.iter().skip(1) {
        let descriptor = program
            .type_catalog()
            .get(&field.ty)
            .expect("List residual TypeDesc");
        assert!(matches!(
            descriptor.layout,
            crate::core::mir::types::MirLayout::List { .. }
        ));
        assert_eq!(
            descriptor.glue.drop,
            crate::core::mir::types::MirGlueKind::List
        );
    }
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic two-List residual execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_record_list_projection_remains_rejected_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_projection_list_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic Record<List<List<i32>>> projection must remain fail-closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("generic record projection")
            || message.contains("nested")
            || message.contains("Copy scalar/bool"),
        "unexpected generic nested List rejection: {message}"
    );
}

#[test]
fn generic_record_list_residual_projection_remains_rejected_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_list_projection_nested_list_residual_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic Record<T> nested List residual must remain fail-closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("generic record projection")
            || message.contains("nested")
            || message.contains("managed")
            || message.contains("List<Copy scalar>"),
        "unexpected generic nested List residual rejection: {message}"
    );
}

#[test]
fn generic_record_three_list_residual_projection_remains_rejected_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_list_projection_three_list_residual_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic Record<T> three-List residual must remain fail-closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("generic record projection")
            || message.contains("residual")
            || message.contains("managed")
            || message.contains("List<Copy scalar>"),
        "unexpected generic three-List residual rejection: {message}"
    );
}

#[test]
fn materializes_generic_record_owned_list_string_residual_drop_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_list_string_residual.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Record<List<i32>> List/String residual must lower to MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
                    if contract.projection.arity == 3
                        && contract.projection.name == "value"
                        && contract.residual.len() == 2
                        && contract.residual[0].name == "note"
                        && contract.residual[0].glue
                            == crate::core::mir::types::MirGlueKind::OwnedString
                        && contract.residual[1].name == "tail"
                        && contract.residual[1].glue
                            == crate::core::mir::types::MirGlueKind::List
            )
        })
        .expect("owned generic List/String residual instance");
    let target = program
        .functions()
        .get(&instance.function)
        .expect("owned generic List/String residual target");
    assert!(target.canonical_text().contains("move_project_drop"));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic List/String residual execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_record_list_string_string_residual_remains_rejected_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_list_string_string_residual_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic Record<T> List/String/String residual must remain fail-closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("generic record projection")
            || message.contains("residual")
            || message.contains("managed"),
        "unexpected generic List/String/String residual rejection: {message}"
    );
}

#[test]
fn materializes_generic_record_owned_list_two_string_residual_drop_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_list_two_string_residual.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Record<List<i32>> List/two-String residual must lower to MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
                    if contract.projection.arity == 4
                        && contract.projection.name == "value"
                        && contract.residual.len() == 3
                        && contract.residual[0].name == "spare"
                        && contract.residual[0].glue
                            == crate::core::mir::types::MirGlueKind::OwnedString
                        && contract.residual[1].name == "note"
                        && contract.residual[1].glue
                            == crate::core::mir::types::MirGlueKind::OwnedString
                        && contract.residual[2].name == "tail"
                        && contract.residual[2].glue == crate::core::mir::types::MirGlueKind::List
            )
        })
        .expect("owned generic List/two-String residual instance");
    let target = program
        .functions()
        .get(&instance.function)
        .expect("owned generic List/two-String residual target");
    assert!(target.canonical_text().contains("move_project_drop"));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic List/two-String residual execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_record_two_list_string_residual_remains_rejected_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_two_list_string_residual_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic Record<T> two-List/String residual must remain fail-closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("generic record projection")
            || message.contains("residual")
            || message.contains("managed"),
        "unexpected generic two-List/String residual rejection: {message}"
    );
}

#[test]
fn materializes_generic_record_owned_bool_list_two_string_residual_drop_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_bool_list_two_string_residual.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Record<List<bool>> List/two-String residual must lower to MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
                    if contract.projection.arity == 4
                        && contract.projection.name == "value"
                        && contract.residual.len() == 3
                        && contract.residual[0].name == "spare"
                        && contract.residual[0].glue
                            == crate::core::mir::types::MirGlueKind::OwnedString
                        && contract.residual[1].name == "note"
                        && contract.residual[1].glue
                            == crate::core::mir::types::MirGlueKind::OwnedString
                        && contract.residual[2].name == "tail"
                        && contract.residual[2].glue == crate::core::mir::types::MirGlueKind::List
            )
        })
        .expect("owned generic Bool List/two-String residual instance");
    let target = program
        .functions()
        .get(&instance.function)
        .expect("owned generic Bool List/two-String residual target");
    let MirGenericInstanceContract::OwnedRecordProjectionDrop { contract } = &instance.contract
    else {
        unreachable!("instance was selected by the owned record receipt predicate")
    };
    let selected_desc = program
        .type_catalog()
        .get(&contract.result_ty)
        .expect("selected Bool List TypeDesc");
    let crate::core::mir::types::MirLayout::List { element } = &selected_desc.layout else {
        panic!("selected payload must retain canonical List layout")
    };
    let element_desc = program
        .type_catalog()
        .get(element)
        .expect("Bool List element TypeDesc");
    assert!(matches!(
        element_desc.kind,
        crate::core::mir::types::MirTypeKind::Primitive(crate::core::PrimitiveType::Bool)
    ));
    assert_eq!(
        selected_desc.abi,
        crate::core::mir::types::MirAbiClass::OpaqueHandle
    );
    assert_eq!(
        selected_desc.ownership,
        crate::core::mir::types::MirOwnership::Move
    );
    assert_eq!(
        selected_desc.glue.move_out,
        crate::core::mir::types::MirGlueKind::List
    );
    assert!(target.canonical_text().contains("move_project_drop"));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Bool List/two-String residual execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_record_bool_nested_residual_remains_rejected_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_bool_nested_residual_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic Record<T> nested Bool List residual must remain fail-closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("generic record projection")
            || message.contains("nested")
            || message.contains("managed"),
        "unexpected generic nested Bool List rejection: {message}"
    );
}

#[test]
fn materializes_generic_record_owned_set_residual_drop_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_record_owned_set_residual.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Record<Set<i32>> Set residual must lower to MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::OwnedRecordProjectionDrop { contract }
                    if contract.projection.arity == 2
                        && contract.projection.name == "value"
                        && contract.residual.len() == 1
                        && contract.residual[0].name == "tail"
                        && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::Set
            )
        })
        .expect("owned generic Set residual instance");
    let MirGenericInstanceContract::OwnedRecordProjectionDrop { contract } = &instance.contract
    else {
        unreachable!("instance was selected by the owned record receipt predicate")
    };
    let selected_desc = program
        .type_catalog()
        .get(&contract.result_ty)
        .expect("selected Set TypeDesc");
    assert!(matches!(
        selected_desc.layout,
        crate::core::mir::types::MirLayout::Set { .. }
    ));
    assert_eq!(
        selected_desc.abi,
        crate::core::mir::types::MirAbiClass::SetHandle
    );
    assert_eq!(
        selected_desc.ownership,
        crate::core::mir::types::MirOwnership::Move
    );
    assert_eq!(
        selected_desc.glue.move_out,
        crate::core::mir::types::MirGlueKind::Set
    );
    let target = program
        .functions()
        .get(&instance.function)
        .expect("owned generic Set residual target");
    assert!(target.canonical_text().contains("move_project_drop"));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Set residual execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_record_set_string_residual_remains_rejected_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_set_string_residual_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic Record<T> Set/String residual must remain fail-closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("generic record projection")
            || message.contains("residual")
            || message.contains("managed"),
        "unexpected generic Set/String residual rejection: {message}"
    );
}

#[test]
fn materializes_generic_record_owned_set_scalar_family_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_set_scalar_family.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Record<Set<i64|bool>> Set residual must lower to MIR");
    let mut saw_i64 = false;
    let mut saw_bool = false;
    for instance in program.instances().values().filter(|instance| {
        matches!(
            instance.contract,
            MirGenericInstanceContract::OwnedRecordProjectionDrop { .. }
        )
    }) {
        let MirGenericInstanceContract::OwnedRecordProjectionDrop { contract } = &instance.contract
        else {
            unreachable!("instance filter preserves the owned record contract")
        };
        assert_eq!(contract.projection.arity, 2);
        assert_eq!(contract.projection.name, "value");
        assert_eq!(contract.residual.len(), 1);
        assert_eq!(contract.residual[0].name, "tail");
        assert_eq!(contract.residual[0].glue, MirGlueKind::Set);
        let selected = program
            .type_catalog()
            .get(&contract.result_ty)
            .expect("selected Set TypeDesc");
        let MirLayout::Set { element } = &selected.layout else {
            panic!("selected payload must retain its Set layout")
        };
        let element_desc = program
            .type_catalog()
            .get(element)
            .expect("selected Set element TypeDesc");
        saw_i64 |= matches!(
            &element_desc.kind,
            MirTypeKind::Primitive(PrimitiveType::I64)
        );
        saw_bool |= matches!(
            &element_desc.kind,
            MirTypeKind::Primitive(PrimitiveType::Bool)
        );
        assert_eq!(selected.ownership, MirOwnership::Move);
        assert_eq!(selected.glue.move_out, MirGlueKind::Set);
    }
    assert!(saw_i64, "Set<i64> selected TypeDesc was not materialized");
    assert!(saw_bool, "Set<bool> selected TypeDesc was not materialized");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Set scalar-family execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_record_owned_set_float_remains_rejected_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_record_owned_set_float_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic Record<Set<f64>> must remain outside the Set payload island");
    let message = format!("{error:?}");
    assert!(
        message.contains("generic record")
            || message.contains("Set")
            || message.contains("Copy scalar")
            || message.contains("scalar contract"),
        "unexpected generic Set<f64> rejection: {message}"
    );
}

fn reclassify_map_any_owner_as_raw_map_root_for_cross_profile_test(function: &mut MirFunction) {
    let typed_receipts = std::mem::take(&mut function.ownership.checker_map_any_owners);
    assert!(
        !typed_receipts.is_empty(),
        "fixture must carry typed-owner receipts"
    );
    for receipt in typed_receipts {
        assert_eq!(
            receipt.profile_version,
            crate::core::MapAnyOwnerProfileVersion::V1
        );
        let (kind, role) = match receipt.kind {
            crate::core::MapAnyOwnerActionKind::New => {
                (crate::core::MapRootActionKind::New, "map_root_new")
            }
            crate::core::MapAnyOwnerActionKind::Set => {
                (crate::core::MapRootActionKind::Set, "map_root_set")
            }
            crate::core::MapAnyOwnerActionKind::Remove => {
                (crate::core::MapRootActionKind::Remove, "map_root_remove")
            }
            crate::core::MapAnyOwnerActionKind::Size => {
                (crate::core::MapRootActionKind::Size, "map_root_size")
            }
            crate::core::MapAnyOwnerActionKind::Contains => (
                crate::core::MapRootActionKind::Contains,
                "map_root_contains",
            ),
            crate::core::MapAnyOwnerActionKind::Drop => {
                (crate::core::MapRootActionKind::Drop, "map_root_drop")
            }
        };
        let instruction = MirInstructionId::new(format!(
            "inst:{role}:{}:{}",
            receipt.local.0, receipt.point.0
        ))
        .expect("legacy MapRoot instruction identity");
        let mut matches = 0;
        for block in function.blocks.values_mut() {
            for instruction_in_block in &mut block.instructions {
                if instruction_in_block.id == receipt.instruction {
                    instruction_in_block.id = instruction.clone();
                    matches += 1;
                }
            }
        }
        assert_eq!(matches, 1, "each receipt must bind one instruction");
        let root = receipt
            .local
            .as_str()
            .strip_prefix("local:")
            .expect("local identity")
            .to_owned();
        let source = receipt.source.map(|source| {
            source
                .strip_prefix("map-any-owner-v1:")
                .unwrap_or(&source)
                .to_owned()
        });
        function
            .ownership
            .checker_map_roots
            .push(MirCheckerMapRootAction {
                kind,
                point: receipt.point,
                local: receipt.local,
                result: receipt.result,
                root,
                key: receipt.key,
                value: receipt.value,
                value_type: receipt.value_type,
                source_local: receipt.source_local,
                source,
                instruction,
            });
    }
    for value in function.values.values_mut() {
        if value.ty == types::map_any_owner_v1_type_id() {
            value.ty = types::map_root_type_id();
        }
    }
    if function.result == types::map_any_owner_v1_type_id() {
        function.result = types::map_root_type_id();
    }
}

#[test]
fn verifier_and_native_reject_two_complete_but_incompatible_map_profiles() {
    let checked = checked_program(
        r#"
            func legacy_map_profile() -> i32 {
                match map_new() {
                    Ok(root) => {
                        match map_set(root, "legacy", 1) {
                            Ok(updated) => {
                                drop(root)
                                let size = map_size(updated)
                                drop(updated)
                                size
                            },
                            Err(status) => { drop(root); status },
                        }
                    },
                    Err(status) => status,
                }
            }
            func main() -> i32 {
                match map_new() {
                    Ok(root) => {
                        match map_set(root, "typed", 2) {
                            Ok(updated) => {
                                drop(root)
                                let size = map_size(updated)
                                drop(updated)
                                size
                            },
                            Err(status) => { drop(root); status },
                        }
                    },
                    Err(status) => status,
                }
            }
        "#,
    );
    let typed = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("both callables initially lower under the typed v1 profile");
    let legacy_owner = crate::core::NodeId("function:legacy_map_profile".into());
    let mut functions = typed.functions().clone();
    let mut legacy = functions
        .get(&legacy_owner)
        .expect("legacy profile fixture")
        .clone();
    reclassify_map_any_owner_as_raw_map_root_for_cross_profile_test(&mut legacy);
    legacy
        .validate()
        .expect("converted fixture carries a complete raw-owner receipt");
    functions.insert(legacy_owner, legacy);

    // The canonical catalog selects either private owner descriptor based on
    // the checked operation profile. Add a real Checker-produced legacy
    // descriptor from a deliberately non-v1 MapGet callable for this
    // whole-program composition fixture.
    let raw_checked = checked_program(
        r#"
            type Point { x: i32 }
            func main() -> i32 {
                let point = Point { x: 7 }
                match map_new() {
                    Ok(root) => {
                        match map_set(root, "answer", point.x) {
                            Ok(updated) => {
                                drop(root)
                                let size = map_size(updated)
                                let outside_profile = map_get(updated, "unreceipted")
                                drop(updated)
                                size
                            },
                            Err(status) => { drop(root); status },
                        }
                    },
                    Err(status) => status,
                }
            }
        "#,
    );
    let raw_catalog = types::MirTypeCatalog::from_checked_program(&raw_checked)
        .expect("raw MapRoot TypeDesc materializes from its Checker profile");
    let root_ty = types::map_root_type_id();
    let mut catalog = typed.type_catalog().clone();
    catalog.replace_for_test_only(
        root_ty.clone(),
        raw_catalog
            .get(&root_ty)
            .expect("Checker-produced raw MapRoot TypeDesc")
            .clone(),
    );
    let combined = crate::core::mir::reference::MirProgram::with_type_catalog(functions, catalog)
        .expect("both profile receipts and descriptors are internally complete");

    let verifier_errors = crate::verifier::validate_mir_capabilities(&combined)
        .expect_err("verifier rejects whole-program profile mixing");
    assert!(
        verifier_errors.iter().any(|error| {
            error.contains("independent profiles and cannot share one MIR program")
        }),
        "unexpected verifier mixed-profile errors: {verifier_errors:?}"
    );
    let native_errors = crate::codegen::mir::validate_mir_native(&combined)
        .expect_err("native admission rejects the same profile mixing");
    assert!(native_errors.iter().any(|error| {
        error
            .to_string()
            .contains("cannot share one native MIR program")
    }));
}

#[test]
fn ordinary_map_alias_clone_and_automatic_cleanup_are_checker_sealed() {
    let checked = checked_program(
        r#"
func main() -> i32 {
    match map_new() {
        Ok(source) => {
            match map_set(source, "number", 42) {
                Ok(numbers) => {
                    drop(source)
                    match map_set(numbers, "text", "owned") {
                        Ok(mixed) => {
                            drop(numbers)
                            let alias = mixed
                            drop(mixed)
                            let size = map_size(alias)
                            size - 2
                        },
                        Err(status) => { drop(numbers); status },
                    }
                },
                Err(status) => { drop(source); status },
            }
        },
        Err(status) => status,
    }
}
"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("ordinary alias canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("main");
    function.validate().expect("alias and cleanup receipt");
    let clones = function
        .ownership
        .checker_ordinary_map_owner_lifecycle_v1
        .iter()
        .filter(|action| action.kind == crate::core::OrdinaryMapOwnerLifecycleActionKind::Clone)
        .collect::<Vec<_>>();
    assert_eq!(clones.len(), 1);
    let clone_id = clones[0].instruction.clone();
    assert_ne!(clones[0].source_local.as_ref(), Some(&clones[0].local));
    assert_eq!(
        function
            .ownership
            .checker_ordinary_map_owner_lifecycle_v1
            .iter()
            .filter(|action| action.kind == crate::core::OrdinaryMapOwnerLifecycleActionKind::Drop)
            .count(),
        4
    );

    let mut omitted = function.clone();
    omitted
        .ownership
        .checker_ordinary_map_owner_lifecycle_v1
        .retain(|action| action.instruction != clone_id);
    assert!(
        omitted.validate().is_err(),
        "a live clone cannot lose its private Checker receipt"
    );
    let mut forged = function.clone();
    let clone = forged
        .ownership
        .checker_ordinary_map_owner_lifecycle_v1
        .iter_mut()
        .find(|action| action.instruction == clone_id)
        .expect("clone");
    clone.source_local = Some(clone.local.clone());
    assert!(
        forged.validate().is_err(),
        "alias receipt cannot clone its undefined destination"
    );
    let mut leaked = function.clone();
    leaked
        .ownership
        .checker_ordinary_map_owner_lifecycle_v1
        .pop();
    assert!(
        leaked.validate().is_err(),
        "automatic cleanup is a required operation receipt"
    );
}

#[test]
fn ordinary_map_v1_reuses_direct_string_key_without_cloning_or_early_drop() {
    let checked = checked_program(
        r#"
func main() -> i32 {
    let key = "pre" + "fix"
    match map_new() {
        Ok(source) => {
            match map_set(source, key, 7) {
                Ok(updated) => {
                    let removed = map_remove(updated, key)
                    let present = has_key(source, key)
                    drop(source)
                    drop(updated)
                    drop(removed)
                    0
                },
                Err(status) => { drop(source); status },
            }
        },
        Err(status) => status,
    }
}
"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("dynamic reusable String key uses the closed ordinary Map profile");
    let function = program
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main MIR");
    let entry = function.blocks.get(&function.entry).expect("entry block");
    let keyed_operations = entry
        .instructions
        .iter()
        .filter_map(|instruction| {
            let key = match &instruction.kind {
                MirInstructionKind::OrdinaryMapOwnerLifecycleSet { key, .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { key, .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleContains { key, .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { key, .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove { key, .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { key, .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { key, .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { key, .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { key, .. } => key,
                _ => return None,
            };
            Some((instruction, key))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        keyed_operations.len(),
        3,
        "Set, Remove, and Contains all use the key"
    );
    let key = keyed_operations[0].1;
    assert!(
        key.as_str().starts_with("local:"),
        "key is the local value: {key}"
    );
    assert!(
        keyed_operations.iter().all(|(_, used)| *used == key),
        "all three operations borrow the same local String value"
    );
    let last_use = entry
        .instructions
        .iter()
        .rposition(|instruction| {
            keyed_operations
                .iter()
                .any(|(operation, _)| operation.id == instruction.id)
        })
        .expect("last key use");
    assert!(
        !entry.instructions[..=last_use].iter().any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Drop { value } if value == key
            )
        }),
        "the borrowed local remains live through its last Map operation"
    );
    assert!(
        !entry.instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Clone { result, .. } if result == key
            )
        }),
        "Map key uses do not manufacture an alias as a temporary clone"
    );
    assert_eq!(
        reference::MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference execution with borrowed Map keys"),
        reference::MirRuntimeValue::Int(0)
    );
}

#[test]
fn ordinary_map_v1_straight_line_direct_local_key_receipt_is_checker_sealed() {
    let checked = checked_program(
        r#"
func main() -> i32 {
    let key = "pre" + "fix"
    match map_new() {
        Ok(source) => {
            match map_set(source, key, 7) {
                Ok(updated) => {
                    let removed = map_remove(updated, key)
                    let present = has_key(source, key)
                    drop(source)
                    drop(updated)
                    drop(removed)
                    if present { 1 } else { 0 }
                },
                Err(status) => { drop(source); status },
            }
        },
        Err(status) => status,
    }
}
"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("straight-line Map operations borrow a direct String key");
    let function = program
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main MIR");
    function
        .validate()
        .expect("every lifecycle key keeps its private Checker receipt");
    let entry = function.blocks.get(&function.entry).expect("entry block");
    let keyed_operations = entry
        .instructions
        .iter()
        .filter_map(|instruction| {
            let key = match &instruction.kind {
                MirInstructionKind::OrdinaryMapOwnerLifecycleSet { key, .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { key, .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleContains { key, .. } => key,
                _ => return None,
            };
            Some((instruction, key))
        })
        .collect::<Vec<_>>();
    assert_eq!(keyed_operations.len(), 3);
    let key = keyed_operations[0].1;
    assert!(key.as_str().starts_with("local:"));
    assert!(keyed_operations.iter().all(|(_, used)| *used == key));
    assert!(!entry.instructions.iter().any(|instruction| {
        matches!(
            &instruction.kind,
            MirInstructionKind::Clone { result, .. } if result == key
        )
    }));
    let last_keyed_operation = entry
        .instructions
        .iter()
        .rposition(|instruction| {
            keyed_operations
                .iter()
                .any(|(operation, _)| operation.id == instruction.id)
        })
        .expect("last keyed operation");
    assert!(!entry.instructions[..=last_keyed_operation]
        .iter()
        .any(|instruction| matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == key)));

    let has_key_receipt = function
        .ownership
        .checker_ordinary_map_owner_lifecycle_v1
        .iter()
        .position(|receipt| {
            receipt.kind == crate::core::OrdinaryMapOwnerLifecycleActionKind::HasKey
        })
        .expect("HasKey key receipt");
    let mut forged_key_local = function.clone();
    forged_key_local
        .ownership
        .checker_ordinary_map_owner_lifecycle_v1[has_key_receipt]
        .key_local = Some(crate::core::ir::ResolvedLocalId(NodeId(
        "function:main/node:pattern.variable@forged/local".into(),
    )));
    assert!(
        forged_key_local.validate().is_err(),
        "lifecycle must reject a key local that disagrees with Checker source identity"
    );
    assert_eq!(
        reference::MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference execution with borrowed Map keys"),
        reference::MirRuntimeValue::Int(0)
    );
}

#[test]
fn ordinary_map_owned_get_v1_preserves_direct_local_key_receipts() {
    let checked = checked_program(
        r#"
func main() -> i32 {
    let key = "answer"
    match map_new() {
        Ok(source) => {
            match map_set(source, key, 17) {
                Ok(updated) => {
                    drop(source)
                    let integer = map_get_i32(updated, key)
                    drop(updated)
                    drop(integer)
                    0
                },
                Err(status) => { drop(source); status },
            }
        },
        Err(status) => status,
    }
}
"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("typed owned-get accepts its exact borrowed direct String key");
    let function = program
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main MIR");
    function
        .validate()
        .expect("all typed Map operations match their private Checker key receipts");
    let entry = function.blocks.get(&function.entry).expect("entry block");
    let keyed_operations = entry
        .instructions
        .iter()
        .filter_map(|instruction| {
            let key = match &instruction.kind {
                MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { key, .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { key, .. } => key,
                _ => return None,
            };
            Some((instruction, key))
        })
        .collect::<Vec<_>>();
    assert_eq!(keyed_operations.len(), 2);
    let key = keyed_operations[0].1;
    assert!(key.as_str().starts_with("local:"));
    assert!(keyed_operations.iter().all(|(_, used)| *used == key));
    assert!(!entry.instructions.iter().any(|instruction| {
        matches!(
            &instruction.kind,
            MirInstructionKind::Clone { result, .. } if result == key
        )
    }));
    let last_keyed_operation = entry
        .instructions
        .iter()
        .rposition(|instruction| {
            keyed_operations
                .iter()
                .any(|(operation, _)| operation.id == instruction.id)
        })
        .expect("last keyed operation");
    assert!(!entry.instructions[..=last_keyed_operation]
        .iter()
        .any(|instruction| matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == key)));

    let get_receipt = function
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1
        .iter()
        .position(|receipt| {
            receipt.kind == crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::GetI32
        })
        .expect("GetI32 key receipt");
    let mut forged_key_local = function.clone();
    forged_key_local
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1[get_receipt]
        .key_local = Some(crate::core::ir::ResolvedLocalId(NodeId(
        "function:main/node:pattern.variable@forged/local".into(),
    )));
    assert!(
        forged_key_local.validate().is_err(),
        "owned-get must reject a key local that disagrees with its private Checker source"
    );
}

#[test]
fn ordinary_map_owned_get_v1_rejects_coherent_entry_action_reordering() {
    let checked = checked_program(
        r#"
func main() -> i32 {
    match map_new() {
        Ok(first) => {
            let first_value = map_get_i32(first, "first")
            match map_new() {
                Ok(second) => {
                    let second_value = map_get_i32(second, "second")
                    drop(first)
                    drop(second)
                    drop(first_value)
                    drop(second_value)
                    0
                },
                Err(status) => {
                    drop(first)
                    drop(first_value)
                    status
                },
            }
        },
        Err(status) => status,
    }
}
"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("straight-line typed owned-get uses the entry-closed V1 profile");
    let function = program
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main MIR");
    let cfg_receipt = function
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1_cfg_receipt
        .as_ref()
        .expect("owned-get CFG receipt");
    assert_eq!(
        cfg_receipt.profile_version,
        crate::core::OrdinaryMapOwnedGetI32StringV1CfgProfileVersion::EntryClosedTailV1
    );

    let mut reordered = function.clone();
    let drop_instructions = reordered
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1
        .iter()
        .filter(|action| action.kind == crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::Drop)
        .map(|action| action.instruction.clone())
        .collect::<Vec<_>>();
    assert_eq!(drop_instructions.len(), 2, "two independent Map roots");

    let first_index = reordered.blocks[&reordered.entry]
        .instructions
        .iter()
        .position(|instruction| instruction.id == drop_instructions[0])
        .expect("first Map drop instruction");
    let second_index = reordered.blocks[&reordered.entry]
        .instructions
        .iter()
        .position(|instruction| instruction.id == drop_instructions[1])
        .expect("second Map drop instruction");
    assert!(first_index < second_index);
    reordered
        .blocks
        .get_mut(&reordered.entry)
        .expect("entry block")
        .instructions
        .swap(first_index, second_index);

    let instruction_order = reordered.blocks[&reordered.entry]
        .instructions
        .iter()
        .enumerate()
        .map(|(index, instruction)| (instruction.id.clone(), index))
        .collect::<BTreeMap<_, _>>();
    reordered
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1
        .sort_by_key(|action| {
            instruction_order
                .get(&action.instruction)
                .copied()
                .unwrap_or(usize::MAX)
        });

    let errors = reordered
        .validate()
        .expect_err("MIR action order cannot be made authoritative by reordering receipts");
    assert!(
        errors.iter().any(|error| error
            .message
            .contains("entry MIR action order disagrees with Checker CFG point order")),
        "expected Checker point-rank rejection, got {errors:?}"
    );
}

#[test]
fn ordinary_map_owned_any_v2_settled_tail_rejects_resealed_instruction_reordering() {
    let checked = checked_program(
        r#"
func main() -> i32 {
    match map_new() {
        Ok(root) => {
            let pair = map_get(root, "missing")
            let found = pair.0
            drop(pair)
            drop(root)
            let observed = found == true
            if observed { 1 } else { 0 }
        },
        Err(status) => status,
    }
}

"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("Checker-receipted V2 settled branch-tail MIR");
    let function = program
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main MIR");
    let cfg_receipt = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .clone();
    let flow = cfg_receipt
        .as_ref()
        .and_then(|receipt| receipt.flow.as_ref())
        .expect("V2 Checker branch-tail CFG receipt");
    let drop_action = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .find(|action| action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Drop)
        .expect("root Drop action");
    let source_entry = flow
        .reachable_blocks
        .get(&flow.entry)
        .expect("Checker entry block");
    let selector = match &source_entry.terminator {
        crate::core::cfg::Terminator::Branch { condition, .. } => condition,
        other => panic!("settled tail starts with a branch selector, got {other:?}"),
    };
    let drop_source_position = source_entry
        .points
        .iter()
        .position(|(point, _)| point == &drop_action.point)
        .expect("root Drop source point is in the Checker entry");
    let selector_source_position = source_entry
        .points
        .iter()
        .position(|(point, _)| point == selector)
        .expect("branch selector source point is in the Checker entry");
    assert!(drop_source_position < selector_source_position);

    let selector_value =
        MirValueId::new(format!("expr:{}", selector.0)).expect("canonical branch selector value");
    let entry_block = function
        .blocks
        .get(&function.entry)
        .expect("MIR entry block");
    let instruction_origins = checker_managed_any_instruction_origins(function);
    let selector_instruction_index = entry_block
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Clone { result, .. } if result == &selector_value
            )
        })
        .expect("final selector load has a MIR Clone instruction");
    let drop_instruction_index = entry_block
        .instructions
        .iter()
        .position(|instruction| instruction.id == drop_action.instruction)
        .expect("root Drop is physically in the entry block");
    let selector_operand_index = entry_block
        .instructions
        .iter()
        .enumerate()
        .filter(|(_, instruction)| {
            matches!(&instruction.kind, MirInstructionKind::Clone { .. })
                && instruction_origins
                    .get(&instruction.id)
                    .and_then(|origin| {
                        source_entry
                            .points
                            .iter()
                            .position(|(point, _)| point == origin)
                    })
                    .is_some_and(|point_index| {
                        drop_source_position < point_index && point_index < selector_source_position
                    })
        })
        .map(|(index, _)| index)
        .min()
        .expect("selector expression has a source-marked local operand Clone");
    assert!(drop_instruction_index < selector_operand_index);
    assert!(selector_operand_index < selector_instruction_index);

    let mut reordered = function.clone();
    let moved_drop = {
        let entry = reordered
            .blocks
            .get_mut(&reordered.entry)
            .expect("MIR entry block");
        entry.instructions.remove(drop_instruction_index)
    };
    // Removing an earlier instruction shifts this operand left by one, so
    // inserting at its original index places the Drop after the operand but
    // before the remaining selector computation and entry Branch.
    reordered
        .blocks
        .get_mut(&reordered.entry)
        .expect("MIR entry block")
        .instructions
        .insert(selector_operand_index, moved_drop);

    reseal_checker_managed_any_instructions(&mut reordered);
    assert_eq!(
        reordered
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt,
        cfg_receipt,
        "the adversary leaves the Checker CFG receipt untouched"
    );

    let errors = reordered
        .validate()
        .expect_err("physical owner Drop order cannot contradict Checker points");
    assert!(
        errors.iter().any(|error| error
            .message
            .contains("settled-tail V2 owner action follows a branch selector dependency")),
        "expected source-point order rejection, got {errors:?}"
    );
}

fn checker_managed_any_instruction_origins(
    function: &MirFunction,
) -> BTreeMap<MirInstructionId, NodeId> {
    function
        .ownership
        .checker_managed_any
        .as_ref()
        .expect("private Checker ManagedAny body seal")
        .instructions
        .iter()
        .map(|(_, origin, instruction)| (instruction.id.clone(), origin.clone()))
        .collect()
}

fn reseal_checker_managed_any_instructions(function: &mut MirFunction) {
    let instruction_origins = checker_managed_any_instruction_origins(function);
    let resealed_instructions = function
        .blocks
        .values()
        .flat_map(|block| {
            block.instructions.iter().map(|instruction| {
                (
                    block.id.clone(),
                    instruction_origins
                        .get(&instruction.id)
                        .expect("instruction retains its Checker source origin")
                        .clone(),
                    instruction.clone(),
                )
            })
        })
        .collect();
    function
        .ownership
        .checker_managed_any
        .as_mut()
        .expect("private Checker ManagedAny body seal")
        .instructions = resealed_instructions;
}

fn settled_selector_dependency_indices(function: &MirFunction) -> Vec<usize> {
    let entry = function
        .blocks
        .get(&function.entry)
        .expect("MIR entry block");
    let selector = match &entry.terminator {
        MirTerminator::Branch { condition, .. } => condition.clone(),
        other => panic!("settled selector fixture has an entry branch, got {other:?}"),
    };
    let mut definitions = BTreeMap::<MirValueId, usize>::new();
    for (index, instruction) in entry.instructions.iter().enumerate() {
        for result in super::lower::instruction_results(&instruction.kind) {
            definitions.insert(result.clone(), index);
        }
    }

    let mut dependencies = BTreeSet::new();
    let mut visited_values = BTreeSet::new();
    let mut pending_values = vec![selector];
    while let Some(value) = pending_values.pop() {
        if !visited_values.insert(value.clone()) {
            continue;
        }
        let Some(index) = definitions.get(&value).copied() else {
            assert!(
                function.parameters.contains(&value),
                "settled selector dependency has a sealed entry definition: {value}"
            );
            continue;
        };
        if dependencies.insert(index) {
            pending_values.extend(super::mir_instruction_value_uses(
                &entry.instructions[index].kind,
            ));
        }
    }
    dependencies.into_iter().collect()
}

#[test]
fn ordinary_map_owned_any_v2_rejects_owner_drop_moved_before_earlier_selector_dependency() {
    let checked = checked_program(
        r#"
func main() -> i32 {
    match map_new() {
        Ok(root) => {
            let pair = map_get(root, "missing")
            let found = pair.0
            let observed = found == true
            drop(pair)
            drop(root)
            if observed { 1 } else { 0 }
        },
        Err(status) => status,
    }
}
"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("typed Checker-receipted V2 owner and settled branch-tail MIR");
    let function = program
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main MIR");
    let cfg_receipt = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .clone();
    let flow = cfg_receipt
        .as_ref()
        .and_then(|receipt| receipt.flow.as_ref())
        .expect("V2 Checker branch-tail CFG receipt");
    let source_entry = flow
        .reachable_blocks
        .get(&flow.entry)
        .expect("Checker entry block");
    let drop_actions = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|action| action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Drop)
        .collect::<Vec<_>>();
    let [drop_action] = drop_actions.as_slice() else {
        panic!("fixture has exactly one V2 owner Drop action, got {drop_actions:?}");
    };
    let drop_source_position = source_entry
        .points
        .iter()
        .position(|(point, _)| point == &drop_action.point)
        .expect("root Drop source point is in the Checker entry");
    let instruction_origins = checker_managed_any_instruction_origins(function);
    let entry_block = function
        .blocks
        .get(&function.entry)
        .expect("MIR entry block");
    let drop_instruction_index = entry_block
        .instructions
        .iter()
        .position(|instruction| instruction.id == drop_action.instruction)
        .expect("root Drop is physically in the entry block");
    let (dependency_index, dependency_id, dependency_source_position) =
        settled_selector_dependency_indices(function)
            .into_iter()
            .filter_map(|index| {
                let instruction = &entry_block.instructions[index];
                let origin = instruction_origins.get(&instruction.id)?;
                let source_position = source_entry
                    .points
                    .iter()
                    .position(|(point, _)| point == origin)?;
                Some((index, instruction.id.clone(), source_position))
            })
            .filter(|(_, _, source_position)| *source_position < drop_source_position)
            .min_by_key(|(_, _, source_position)| *source_position)
            .expect("observed's earlier definition is in the branch selector dependency cone");
    assert!(
        dependency_source_position < drop_source_position,
        "the selector dependency's Checker point precedes the V2 root Drop"
    );
    assert!(
        dependency_index < drop_instruction_index,
        "the original MIR places the earlier selector dependency before the root Drop"
    );

    let mut reordered = function.clone();
    let moved_drop = reordered
        .blocks
        .get_mut(&reordered.entry)
        .expect("MIR entry block")
        .instructions
        .remove(drop_instruction_index);
    reordered
        .blocks
        .get_mut(&reordered.entry)
        .expect("MIR entry block")
        .instructions
        .insert(dependency_index, moved_drop);
    reseal_checker_managed_any_instructions(&mut reordered);
    assert_eq!(
        reordered
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt,
        cfg_receipt,
        "the adversary leaves the Checker CFG receipt untouched"
    );
    let reordered_entry = reordered
        .blocks
        .get(&reordered.entry)
        .expect("reordered MIR entry block");
    let reordered_drop_index = reordered_entry
        .instructions
        .iter()
        .position(|instruction| instruction.id == drop_action.instruction)
        .expect("reordered root Drop");
    let reordered_dependency_index = reordered_entry
        .instructions
        .iter()
        .position(|instruction| instruction.id == dependency_id)
        .expect("reordered selector dependency");
    assert!(reordered_drop_index < reordered_dependency_index);

    let errors = reordered
        .validate()
        .expect_err("a V2 owner Drop cannot physically precede its earlier selector dependency");
    assert!(
        errors.iter().any(|error| error
            .message
            .contains("settled-tail V2 owner action precedes a branch selector dependency")),
        "expected reverse source/physical order rejection, got {errors:?}"
    );
}

#[test]
fn ordinary_map_owned_any_v2_rejects_result_owner_drop_moved_after_selector_dependency() {
    let checked = checked_program(
        r#"
func main() -> i32 {
    match map_new() {
        Ok(root) => {
            let pair = map_get(root, "missing")
            let found = pair.0
            drop(pair)
            drop(root)
            let observed = found == true
            if observed { 1 } else { 0 }
        },
        Err(status) => status,
    }
}
"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("typed Checker-receipted V2 result owner and settled branch-tail MIR");
    let function = program
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main MIR");
    let cfg_receipt = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .clone();
    let flow = cfg_receipt
        .as_ref()
        .and_then(|receipt| receipt.flow.as_ref())
        .expect("V2 Checker branch-tail CFG receipt");
    let source_entry = flow
        .reachable_blocks
        .get(&flow.entry)
        .expect("Checker entry block");
    let result_owner = cfg_receipt
        .as_ref()
        .and_then(|receipt| receipt.result_receipts.first())
        .expect("GetAny result owner receipt for pair");
    let settlement_point = result_owner
        .settlement_point
        .as_ref()
        .expect("pair has an explicit settlement Drop");
    let settlement_source_position = source_entry
        .points
        .iter()
        .position(|(point, _)| point == settlement_point)
        .expect("pair Drop source point is in the Checker entry");
    let selector_source_position = match &source_entry.terminator {
        crate::core::cfg::Terminator::Branch { condition, .. } => source_entry
            .points
            .iter()
            .position(|(point, _)| point == condition)
            .expect("branch selector source point is in the Checker entry"),
        other => panic!("settled tail starts with a branch selector, got {other:?}"),
    };
    assert!(
        settlement_source_position < selector_source_position,
        "the pair settlement precedes the branch selector in Checker source order"
    );

    let instruction_origins = checker_managed_any_instruction_origins(function);
    let entry_block = function
        .blocks
        .get(&function.entry)
        .expect("MIR entry block");
    let binding_value = MirValueId::new(format!("local:{}", result_owner.binding.0 .0))
        .expect("pair binding has canonical MIR value identity");
    let settlement_drop = function
        .ownership
        .checker_managed_any
        .as_ref()
        .expect("private Checker ManagedAny body seal")
        .instructions
        .iter()
        .find(|(_, origin, instruction)| {
            origin == settlement_point
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::Drop { value } if value == &binding_value
                )
        })
        .map(|(_, _, instruction)| instruction.id.clone())
        .expect("pair settlement is sealed to its exact MIR Drop");
    let settlement_drop_index = entry_block
        .instructions
        .iter()
        .position(|instruction| instruction.id == settlement_drop)
        .expect("pair settlement Drop is physically in the entry block");
    let (dependency_index, dependency_id, dependency_source_position) =
        settled_selector_dependency_indices(function)
            .into_iter()
            .filter_map(|index| {
                let instruction = &entry_block.instructions[index];
                let origin = instruction_origins.get(&instruction.id)?;
                let source_position = source_entry
                    .points
                    .iter()
                    .position(|(point, _)| point == origin)?;
                Some((index, instruction.id.clone(), source_position))
            })
            .filter(|(_, _, source_position)| *source_position > settlement_source_position)
            .min_by_key(|(_, _, source_position)| *source_position)
            .expect("observed's selector dependency follows the pair settlement");
    assert!(
        settlement_source_position < dependency_source_position,
        "the pair settlement's Checker point precedes this selector dependency"
    );
    assert!(
        settlement_drop_index < dependency_index,
        "the original MIR places the pair settlement before the later selector dependency"
    );

    let mut reordered = function.clone();
    let moved_drop = reordered
        .blocks
        .get_mut(&reordered.entry)
        .expect("MIR entry block")
        .instructions
        .remove(settlement_drop_index);
    reordered
        .blocks
        .get_mut(&reordered.entry)
        .expect("MIR entry block")
        .instructions
        .insert(dependency_index, moved_drop);
    reseal_checker_managed_any_instructions(&mut reordered);
    assert_eq!(
        reordered
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt,
        cfg_receipt,
        "the adversary leaves the Checker CFG receipt untouched"
    );
    let reordered_entry = reordered
        .blocks
        .get(&reordered.entry)
        .expect("reordered MIR entry block");
    let reordered_drop_index = reordered_entry
        .instructions
        .iter()
        .position(|instruction| instruction.id == settlement_drop)
        .expect("reordered pair settlement Drop");
    let reordered_dependency_index = reordered_entry
        .instructions
        .iter()
        .position(|instruction| instruction.id == dependency_id)
        .expect("reordered selector dependency");
    assert!(reordered_drop_index > reordered_dependency_index);

    let errors = reordered
        .validate()
        .expect_err("a result-owner settlement Drop cannot follow a later selector dependency");
    assert!(
        errors.iter().any(|error| error
            .message
            .contains("settled-tail V2 owner action follows a branch selector dependency")),
        "expected result-owner settlement order rejection, got {errors:?}"
    );
}

#[test]
fn ordinary_map_owned_get_clone_and_scope_cleanup_keep_their_separate_identity() {
    let checked = checked_program(
        r#"
func main() -> i32 {
    match map_new() {
        Ok(source) => {
            match map_set(source, "text", "kept") {
                Ok(full) => {
                    drop(source)
                    let alias = full
                    drop(full)
                    let snapshot = map_get_string(alias, "text")
                    drop(snapshot)
                    let size = map_size(alias)
                    size - 1
                },
                Err(status) => { drop(source); status },
            }
        },
        Err(status) => status,
    }
}
"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("owned-get alias canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("main");
    function.validate().expect("clone and cleanup receipts");
    assert!(function
        .ownership
        .checker_ordinary_map_owner_lifecycle_v1
        .is_empty());
    let clone = function
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1
        .iter()
        .find(|action| action.kind == crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::Clone)
        .expect("owned-get clone");
    assert_ne!(clone.source_local.as_ref(), Some(&clone.local));
    let mut forged = function.clone();
    let action = forged
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1
        .iter_mut()
        .find(|action| action.kind == crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::Clone)
        .expect("clone");
    action.profile_version = crate::core::OrdinaryMapOwnedGetI32StringV1ProfileVersion::V1;
    action.source = Some("ordinary-map-owner-lifecycle-i32-string-v1:forged".into());
    assert!(
        forged.validate().is_err(),
        "lifecycle root identity cannot satisfy an owned-get clone receipt"
    );
}

#[test]
fn ordinary_map_string_domain_helper_is_reserved_source_ffi() {
    let checked = checked_program(
        r#"
extern "C" { func mimi_mir_ordinary_map_domain_own_string(pointer: i64, length: i64) -> i32; }
func main() -> i32 { mimi_mir_ordinary_map_domain_own_string(0 as i64, 0 as i64) }
"#,
    );
    let error = reference::MirProgram::from_checked_program(&checked)
        .expect_err("source cannot manufacture a canonical String execution-domain owner receipt");
    assert!(
        error.to_diagnostics().iter().any(|diagnostic| diagnostic
            .message
            .contains("reserved Canonical MIR ordinary Map execution-domain runtime helper")),
        "{error:?}"
    );
}

#[test]
fn ordinary_map_owned_get_string_unwrap_preserves_its_result_snapshot() {
    let source = r#"
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "text", "kept") {
                Ok(mixed) => {
                    drop(empty)
                    let snapshot = map_get_string(mixed, "text")
                    let text = snapshot.unwrap()
                    drop(snapshot)
                    drop(text)
                    0
                },
                Err(status) => { drop(empty); status },
            }
        },
        Err(status) => status,
    }
}
"#;
    let checked = checked_program(source);
    let program = super::reference::MirProgram::from_checked_program(&checked)
        .expect("owned getter unwrap uses the exact managed Result projection");
    let main = program
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main MIR");
    let projection = main
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::VariantProjectMove {
                base,
                contract: Some(contract),
                ..
            } => Some((base, contract)),
            _ => None,
        })
        .expect("owned String uses a receipted move projection");
    assert_eq!(
        projection.1.projection.move_out_glue,
        types::MirGlueKind::OwnedString
    );
    let snapshot = main
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Clone { result, source } if result == projection.0 => Some(source),
            _ => None,
        })
        .expect("projection consumes a cloned Result and preserves the source snapshot");
    assert!(main
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == snapshot)
        }));
    let mut forged_main = main.clone();
    for instruction in forged_main
        .blocks
        .values_mut()
        .flat_map(|block| &mut block.instructions)
    {
        if let MirInstructionKind::VariantProjectMove { base, .. } = &mut instruction.kind {
            *base = snapshot.clone();
        }
    }
    let mut forged_functions = program.functions().clone();
    forged_functions.insert(forged_main.owner.clone(), forged_main);
    let errors = super::reference::MirProgram::with_type_catalog(
        forged_functions,
        program.type_catalog().clone(),
    )
    .expect_err("a same-TypeDesc projection cannot consume the original retained Result");
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("use after consuming")),
        "{errors:?}"
    );
    let value = super::reference::MirReferenceInterpreter::new(&program)
        .execute(&NodeId("function:main".into()), &[])
        .expect("the source Result and projected String have independent destruction");
    assert_eq!(value, super::reference::MirRuntimeValue::Int(0));
}

#[test]
fn ordinary_map_owned_any_v2_scalar_tail_binds_source_load_and_return_after_resealing() {
    let checked = checked_program(
        r#"
func main() -> i32 {
    match map_new() {
        Ok(root) => {
            let pair = map_get(root, "missing")
            let found = pair.0
            drop(pair)
            drop(root)
            let alternate = 77
            let alternate_flag = true
            if found { 1 } else { 0 }
        },
        Err(status) => status,
    }
}
"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("entry-settled scalar tail has complete source coverage");
    let function = &program.functions()[&NodeId("function:main".into())];
    let body = checked.resolved_body(&function.owner).unwrap();
    let local = |name: &str| {
        let binding = body
            .locals
            .values()
            .find(|local| local.display_name == name)
            .unwrap();
        MirValueId::new(format!("local:{}", binding.id.0 .0)).unwrap()
    };
    let alternate = local("alternate");
    let alternate_flag = local("alternate_flag");
    let mut wrong_return = function.clone();
    for block in wrong_return.blocks.values_mut() {
        if let MirTerminator::Return { value, .. } = &mut block.terminator {
            *value = Some(alternate.clone());
        }
    }
    reseal_checker_managed_any_instructions(&mut wrong_return);
    wrong_return
        .ownership
        .checker_managed_any
        .as_mut()
        .unwrap()
        .terminators = wrong_return
        .blocks
        .values()
        .map(|block| (block.id.clone(), block.terminator.clone()))
        .collect();
    assert!(wrong_return
        .validate()
        .unwrap_err()
        .iter()
        .any(|error| { error.message.contains("scalar-tail source return") }));

    let mut wrong_load = function.clone();
    let condition = match &wrong_load.blocks[&wrong_load.entry].terminator {
        MirTerminator::Branch { condition, .. } => condition.clone(),
        _ => panic!("the source if is the first selector"),
    };
    let instruction = wrong_load.blocks.get_mut(&wrong_load.entry).unwrap().instructions
        .iter_mut().find(|instruction| {
            matches!(&instruction.kind, MirInstructionKind::Clone { result, .. } if result == &condition)
        }).unwrap();
    if let MirInstructionKind::Clone { source, .. } = &mut instruction.kind {
        *source = alternate_flag;
    }
    reseal_checker_managed_any_instructions(&mut wrong_load);
    assert!(wrong_load
        .validate()
        .unwrap_err()
        .iter()
        .any(|error| { error.message.contains("scalar-tail source expression") }));
}

#[test]
fn ordinary_map_owned_any_v2_scalar_tail_supports_finite_serial_and_nested_selectors() {
    let checked = checked_program(
        r#"
func main() -> i32 {
    match map_new() {
        Ok(root) => {
            let pair = map_get(root, "missing")
            let found = pair.0
            drop(pair)
            drop(root)
            let first = found && false
            let second = first || true
            if second { if found { 1 } else { 2 } } else { 3 }
        },
        Err(status) => status,
    }
}
"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("source-bound serial and nested scalar selectors have a complete receipt");
    let result = super::reference::MirReferenceInterpreter::new(&program)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap();
    assert_eq!(result, super::reference::MirRuntimeValue::Int(2));
}

#[test]
fn ordinary_map_owned_any_v2_scalar_tail_string_drop_requires_exact_mir_settlement() {
    let checked = checked_program(
        r#"
func main() -> i32 {
    let key = "missing"
    match map_new() {
        Ok(root) => {
            let pair = map_get(root, key)
            let same = key == "missing"
            let found = pair.0
            drop(pair)
            drop(root)
            drop(key)
            if found && same { 1 } else { 0 }
        },
        Err(status) => { drop(key); status },
    }
}
"#,
    );
    let program = reference::MirProgram::from_checked_program(&checked).unwrap();
    let function = &program.functions()[&NodeId("function:main".into())];
    let flow = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .unwrap()
        .flow
        .as_ref()
        .unwrap();
    let crate::core::ownership::OrdinaryMapOwnedAnyV2CfgFlowProof::SettledScalarBranchTailV1(tail) =
        &flow.proof
    else {
        panic!("scalar source authority");
    };
    let (binding, drop) = tail.string_drops.first_key_value().unwrap();
    let value = MirValueId::new(format!("local:{}", binding.0 .0)).unwrap();
    let id = format!("inst:drop.{}:{}", drop.place_index, drop.point.0);
    assert!(function.blocks[&function.entry].instructions.iter().any(|instruction| {
        instruction.id.as_str() == id && matches!(&instruction.kind, MirInstructionKind::Drop { value: actual } if actual == &value)
    }));
    assert!(function
        .blocks
        .iter()
        .filter(|(block, _)| *block != &function.entry)
        .all(|(_, block)| !block
            .instructions
            .iter()
            .any(|instruction| matches!(&instruction.kind, MirInstructionKind::Drop { .. }))));
    for mutation in [
        "missing",
        "wrong index",
        "synthetic scope",
        "wrong local",
        "wrong source origin",
        "extra prefix drop",
        "drop before key use",
        "drop before later binary read",
    ] {
        let mut forged = function.clone();
        for block in forged.blocks.values_mut() {
            if mutation == "missing" {
                block
                    .instructions
                    .retain(|instruction| instruction.id.as_str() != id);
            } else if mutation == "extra prefix drop" {
                if let Some(index) = block
                    .instructions
                    .iter()
                    .position(|instruction| instruction.id.as_str() == id)
                {
                    let mut duplicate = block.instructions[index].clone();
                    duplicate.id =
                        MirInstructionId::new(format!("inst:drop.extra-prefix:{}", drop.point.0))
                            .unwrap();
                    block.instructions.insert(index + 1, duplicate);
                }
            } else if matches!(
                mutation,
                "drop before key use" | "drop before later binary read"
            ) {
                if let Some(index) = block
                    .instructions
                    .iter()
                    .position(|instruction| instruction.id.as_str() == id)
                {
                    let instruction = block.instructions.remove(index);
                    let first_key_use = block
                        .instructions
                        .iter()
                        .position(|instruction| {
                            if mutation == "drop before key use" {
                                matches!(&instruction.kind,
                                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { key, .. }
                                        if key == &value)
                            } else {
                                matches!(&instruction.kind,
                                    MirInstructionKind::Binary { left, right, .. }
                                        if left == &value || right == &value)
                            }
                        })
                        .unwrap();
                    block.instructions.insert(first_key_use, instruction);
                }
            } else if mutation != "wrong source origin" {
                for instruction in &mut block.instructions {
                    if instruction.id.as_str() == id {
                        match mutation {
                            "wrong index" => {
                                instruction.id = MirInstructionId::new(format!(
                                    "inst:drop.{}:{}",
                                    drop.place_index + 1,
                                    drop.point.0
                                ))
                                .unwrap()
                            }
                            "synthetic scope" => {
                                instruction.id = MirInstructionId::new(format!(
                                    "inst:drop.managed_scope:{}",
                                    drop.point.0
                                ))
                                .unwrap()
                            }
                            _ => {
                                instruction.kind = MirInstructionKind::Drop {
                                    value: function.blocks[&function.entry]
                                        .instructions
                                        .iter()
                                        .find_map(|instruction| match &instruction.kind {
                                            MirInstructionKind::OrdinaryMapOwnedAnyV2New {
                                                result,
                                            } => Some(result.clone()),
                                            _ => None,
                                        })
                                        .unwrap(),
                                }
                            }
                        }
                    }
                }
            }
        }
        // Reseal every changed instruction while retaining the independent
        // typed source commitment and its original exact Drop identity.
        let origins = checker_managed_any_instruction_origins(function);
        forged
            .ownership
            .checker_managed_any
            .as_mut()
            .unwrap()
            .instructions = forged
            .blocks
            .values()
            .flat_map(|block| {
                block.instructions.iter().map(|instruction| {
                    (
                        block.id.clone(),
                        if mutation == "wrong source origin" && instruction.id.as_str() == id {
                            drop.loads.first().unwrap().0.clone()
                        } else {
                            origins
                                .get(&instruction.id)
                                .cloned()
                                .unwrap_or_else(|| drop.point.clone())
                        },
                        instruction.clone(),
                    )
                })
            })
            .collect();
        assert!(
            forged
                .validate()
                .unwrap_err()
                .iter()
                .any(|error| error.message.contains("scalar-tail source String Drop")),
            "{mutation}"
        );
    }
}

const ORDINARY_MAP_NEW_RESULT_L1_SOURCE: &str = r#"
    func main() -> i32 {
        let outcome = map_new()
        match outcome {
            Ok(root) => {
                let size = map_size(root)
                drop(root)
                size
            },
            Err(status) => status
        }
    }
"#;

const ORDINARY_MAP_NEW_RESULT_L2_SOURCE: &str = r#"
    func main() -> i32 {
        requires: true
        ensures: result == 0 || result == 3
        let outcome = map_new()
        match outcome {
            Ok(root) => {
                let size = map_size(root)
                drop(root)
                size
            },
            Err(status) => status
        }
    }
"#;

fn ordinary_map_new_result_l1_program() -> reference::MirProgram {
    let checked = checked_program(ORDINARY_MAP_NEW_RESULT_L1_SOURCE);
    reference::MirProgram::from_checked_program_map_new_result(
        &checked,
        &crate::core::NodeId("function:main".into()),
    )
    .expect("exact Checker NewResult source lowers through pending direct L1 constructor")
}

fn ordinary_map_new_result_l2_pair(
    source: &str,
) -> (crate::core::CheckedProgram, reference::MirProgram) {
    let checked = checked_program(source);
    let owner = crate::core::NodeId("function:main".into());
    let program =
        reference::MirProgram::from_checked_program_map_new_result_for_verifier(&checked, &owner)
            .expect(
            "exact Checker NewResult source lowers through the isolated L2 verifier constructor",
        );
    (checked, program)
}

#[test]
fn ordinary_map_new_result_l2_replays_checked_receipts_contracts_and_recoverable_err3() {
    if !crate::verifier::is_z3_available() {
        return;
    }
    let (checked, program) = ordinary_map_new_result_l2_pair(ORDINARY_MAP_NEW_RESULT_L2_SOURCE);
    let receipt = validate_ordinary_map_new_result_l2_verifier_route(&checked, &program)
        .expect("L2 binds the exact CheckedProgram, graph, source census, contracts and worklist");
    assert_eq!(receipt.profile, ORDINARY_MAP_NEW_RESULT_L2_VERIFIER_PROFILE);
    let main = &program.functions()[&crate::core::NodeId("function:main".into())];
    assert_eq!(
        main.contracts.len(),
        2,
        "requires and ensures remain attached"
    );
    assert!(
        main.proof_worklist.is_none(),
        "direct contracts need no body worklist"
    );
    program
        .validate_owner_proof_worklist(&main.owner)
        .expect("the independent checked-source census validates direct contract coverage");

    let results = crate::verifier::verify_mir_with_ordinary_map_new_result_l2_verifier_route(
        &checked,
        &program,
        &receipt,
        "ordinary-map-new-result-l2-source".into(),
    )
    .expect("the dedicated Checker-aware L2 verifier accepts the exact graph");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        crate::verifier::VerifStatus::Proven,
        "{}",
        results[0].message
    );
    assert!(!results[0].message.contains("E0800"));

    for (ensures, expected) in [
        (
            "result == 0",
            "the capacity Err(3) path is recoverable and disproves success-only",
        ),
        (
            "result == 3",
            "the successful empty root path disproves capacity-only",
        ),
    ] {
        let source =
            ORDINARY_MAP_NEW_RESULT_L2_SOURCE.replace("result == 0 || result == 3", ensures);
        let (checked, program) = ordinary_map_new_result_l2_pair(&source);
        let receipt = validate_ordinary_map_new_result_l2_verifier_route(&checked, &program)
            .expect("the exact source contract change keeps L2 route authority");
        let results = crate::verifier::verify_mir_with_ordinary_map_new_result_l2_verifier_route(
            &checked,
            &program,
            &receipt,
            "ordinary-map-new-result-l2-branch-check".into(),
        )
        .expect("both NewResult branches are normal symbolic outcomes");
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].status,
            crate::verifier::VerifStatus::Disproven,
            "{expected}: {}",
            results[0].message
        );
        assert!(
            !results[0].message.contains("E0800"),
            "{}",
            results[0].message
        );
    }
}

#[test]
fn ordinary_map_new_result_l2_bridges_new_and_size_calls_without_clearing_proof_census() {
    if !crate::verifier::is_z3_available() {
        return;
    }
    let source = ORDINARY_MAP_NEW_RESULT_L2_SOURCE.replace(
        "ensures: result == 0 || result == 3",
        "ensures: result == 0 || result == 3\n        math: { 1 + 1 == 2; };",
    );
    let (checked, program) = ordinary_map_new_result_l2_pair(&source);
    let receipt = validate_ordinary_map_new_result_l2_verifier_route(&checked, &program)
        .expect("Checker call bridge retains the exact proof boundary/source census");
    let main = &program.functions()[&crate::core::NodeId("function:main".into())];
    let source_receipt = main.ownership.ordinary_map_new_result_receipt().unwrap();
    let worklist = main
        .proof_worklist
        .as_ref()
        .expect("direct Math obligation retains an executable worklist");
    assert_eq!(worklist.expected_calls.len(), 2);
    assert_eq!(
        worklist.expected_scoped_boundaries,
        std::slice::from_ref(&source_receipt.match_node)
    );
    assert_eq!(
        worklist.boundaries.len(),
        1,
        "the exact match boundary remains in the census"
    );
    fn collect_calls<'a>(block: &'a MirProofBlock, calls: &mut Vec<&'a MirProofCallPoint>) {
        for operation in &block.operations {
            match operation {
                MirProofOperation::Call(call) => calls.push(call),
                MirProofOperation::Branch {
                    then_block,
                    else_block,
                    ..
                } => {
                    collect_calls(then_block, calls);
                    collect_calls(else_block, calls);
                }
                MirProofOperation::Scope(inner) => collect_calls(inner, calls),
                MirProofOperation::Loop {
                    condition_calls,
                    body,
                    ..
                } => {
                    calls.extend(condition_calls.iter());
                    collect_calls(body, calls);
                }
                MirProofOperation::Bind { .. }
                | MirProofOperation::Eval { .. }
                | MirProofOperation::Assign { .. }
                | MirProofOperation::Math(_)
                | MirProofOperation::Break { .. }
                | MirProofOperation::Continue { .. }
                | MirProofOperation::Return { .. }
                | MirProofOperation::Exit => {}
            }
        }
    }
    let mut calls = Vec::new();
    collect_calls(&worklist.root, &mut calls);
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().any(|call| {
        call.call_node == source_receipt.operation
            && call.instruction.as_str()
                == format!(
                    "inst:ordinary_map_new_result:new:{}",
                    source_receipt.operation.0
                )
    }));
    assert!(calls.iter().any(|call| {
        call.call_node == source_receipt.size_operation
            && call.instruction.as_str()
                == format!(
                    "inst:ordinary_map_new_result:size:{}",
                    source_receipt.size_operation.0
                )
            && call.match_path
                == checked
                    .callable(&source_receipt.owner)
                    .unwrap()
                    .proof_point_worklist()
                    .calls
                    .iter()
                    .find(|point| point.call_node == source_receipt.size_operation)
                    .unwrap()
                    .match_path
    }));
    assert!(
        calls[0].order < calls[1].order,
        "source call evaluation order is preserved"
    );

    let results = crate::verifier::verify_mir_with_ordinary_map_new_result_l2_verifier_route(
        &checked,
        &program,
        &receipt,
        "ordinary-map-new-result-l2-math-census".into(),
    )
    .expect("canonical New/Size operations and proof worklist verify together");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        crate::verifier::VerifStatus::Proven,
        "{}",
        results[0].message
    );
}

#[test]
fn ordinary_map_new_result_l2_rejects_forged_graph_receipt_and_route_digest() {
    let (checked, program) = ordinary_map_new_result_l2_pair(ORDINARY_MAP_NEW_RESULT_L2_SOURCE);
    let owner = crate::core::NodeId("function:main".into());
    let receipt = validate_ordinary_map_new_result_l2_verifier_route(&checked, &program)
        .expect("the base L2 route is exact");

    let mut forged_receipt = program.clone();
    let function = forged_receipt.function_mut_for_test(&owner).unwrap();
    function
        .ownership
        .checker_ordinary_map_owned_any_v2_new_result
        .as_mut()
        .unwrap()
        .status_type = crate::core::ir::primitive_type_id(PrimitiveType::I64);
    assert!(validate_ordinary_map_new_result_l2_verifier_route(&checked, &forged_receipt).is_err());
    assert!(receipt.validate_against_program(&forged_receipt).is_err());

    let mut forged_digest = receipt.clone();
    forged_digest.ownership_digest = "0".repeat(64);
    assert!(forged_digest.validate_against_program(&program).is_err());

    let source = program.functions()[&owner]
        .ownership
        .ordinary_map_new_result_receipt()
        .unwrap()
        .clone();
    let mut missing_drop = program.clone();
    missing_drop
        .function_mut_for_test(&owner)
        .unwrap()
        .blocks
        .get_mut(&MirBlockId::new(source.ok_block.0 .0.clone()).unwrap())
        .unwrap()
        .instructions
        .pop();
    assert!(validate_ordinary_map_new_result_l2_verifier_route(&checked, &missing_drop).is_err());
    assert!(receipt.validate_against_program(&missing_drop).is_err());

    let mut bad_status = program.clone();
    bad_status
        .function_mut_for_test(&owner)
        .unwrap()
        .blocks
        .get_mut(&MirBlockId::new(source.err_block.0 .0.clone()).unwrap())
        .unwrap()
        .terminator = MirTerminator::Goto {
        edge: MirEdgeId::new(source.err_join_edge.0 .0.clone()).unwrap(),
        target: MirBlockId::new(source.join.0 .0.clone()).unwrap(),
        arguments: vec![MirValueId::new(format!("local:{}", source.root_local.0 .0)).unwrap()],
    };
    assert!(validate_ordinary_map_new_result_l2_verifier_route(&checked, &bad_status).is_err());
}

#[test]
fn ordinary_map_new_result_l1_core_route_receipt_is_exact_and_revalidates() {
    let program = ordinary_map_new_result_l1_program();
    let receipt = validate_ordinary_map_new_result_l1_core_mir_route(&program)
        .expect("the pending Checker receipt derives an independent Core MIR route receipt");
    assert_eq!(receipt.profile, ORDINARY_MAP_NEW_RESULT_L1_CORE_PROFILE);
    receipt
        .validate_against_program(&program)
        .expect("the route receipt revalidates its exact New/Ok/Err/settlement MIR graph");
    let main = &program.functions()[&crate::core::NodeId("function:main".into())];
    assert!(
        main.contracts.is_empty(),
        "the source has no verifier obligations"
    );
    let verifier_error =
        crate::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new())
            .expect_err("a valid Core audit receipt does not grant verifier capability");
    assert!(verifier_error.contains("audit-only and has no verifier capability"));
    let raw_error = crate::verifier::verify_mir(&program, String::new())
        .expect_err("raw verify_mir cannot accept a NewResult graph without its Checker authority");
    assert!(
        raw_error.contains("not admitted by default MIR verification")
            && raw_error.contains("dedicated Checker-aware L2 verifier route"),
        "raw verifier rejection remains specific: {raw_error}"
    );
    for profile in ["verifier-mir-v1", "arbitrary-profile"] {
        let relabeled = program.route_receipt(profile);
        let error =
            crate::verifier::verify_mir_with_route_receipt(&program, &relabeled, String::new())
                .expect_err(
                    "NewResult operation/descriptor signals survive route profile relabeling",
                );
        assert!(error.contains("exact CheckedProgram-aware L2 verifier route"));
    }

    let mut forged_receipt = receipt.clone();
    forged_receipt.ownership_digest = "0".repeat(64);
    assert!(
        forged_receipt.validate_against_program(&program).is_err(),
        "a syntactically valid digest cannot replace the ownership identity"
    );
}

#[test]
fn ordinary_map_new_result_l1_core_route_rejects_graph_and_scope_forgery() {
    let program = ordinary_map_new_result_l1_program();
    let route_receipt = validate_ordinary_map_new_result_l1_core_mir_route(&program)
        .expect("exact source has an independent Core route receipt");
    let owner = crate::core::NodeId("function:main".into());
    let function = program.functions()[&owner].clone();
    let source = function
        .ownership
        .ordinary_map_new_result_receipt()
        .expect("the Checker source receipt is embedded in MIR")
        .clone();
    let err = MirBlockId::new(source.err_block.0 .0.clone()).unwrap();
    let ok = MirBlockId::new(source.ok_block.0 .0.clone()).unwrap();

    let mut missing_drop = program.clone();
    missing_drop
        .function_mut_for_test(&owner)
        .unwrap()
        .blocks
        .get_mut(&ok)
        .unwrap()
        .instructions
        .pop();
    assert!(validate_ordinary_map_new_result_l1_core_mir_route(&missing_drop).is_err());
    assert!(route_receipt
        .validate_against_program(&missing_drop)
        .is_err());

    let mut extra_size = program.clone();
    {
        let forged = extra_size.function_mut_for_test(&owner).unwrap();
        let value = MirValueId::new("forged:new-result:extra-size").unwrap();
        forged.values.insert(
            value.clone(),
            MirValue {
                id: value.clone(),
                ty: crate::core::ir::primitive_type_id(PrimitiveType::I32),
            },
        );
        forged.blocks.get_mut(&ok).unwrap().instructions.insert(
            0,
            MirInstruction {
                id: MirInstructionId::new("forged:new-result:extra-size-op").unwrap(),
                kind: MirInstructionKind::OrdinaryMapOwnedAnyV2Size {
                    result: value,
                    root: MirValueId::new(format!("local:{}", source.root_local.0 .0)).unwrap(),
                },
            },
        );
    }
    assert!(validate_ordinary_map_new_result_l1_core_mir_route(&extra_size).is_err());
    assert!(route_receipt.validate_against_program(&extra_size).is_err());

    let mut rewritten_err = program.clone();
    let status = MirValueId::new(format!("local:{}", source.status_local.0 .0)).unwrap();
    let size = MirValueId::new(format!("local:{}", source.size_local.0 .0)).unwrap();
    assert_ne!(status, size);
    rewritten_err
        .function_mut_for_test(&owner)
        .unwrap()
        .blocks
        .get_mut(&err)
        .unwrap()
        .terminator = MirTerminator::Goto {
        edge: MirEdgeId::new(source.err_join_edge.0 .0.clone()).unwrap(),
        target: MirBlockId::new(source.join.0 .0.clone()).unwrap(),
        arguments: vec![size],
    };
    assert!(validate_ordinary_map_new_result_l1_core_mir_route(&rewritten_err).is_err());
    assert!(route_receipt
        .validate_against_program(&rewritten_err)
        .is_err());

    let helper = crate::core::NodeId("function:helper".into());
    let mut extra_callable = program.clone();
    let mut helper_function = function;
    helper_function.owner = helper;
    extra_callable.replace_function_for_test_only(helper_function);
    assert!(validate_ordinary_map_new_result_l1_core_mir_route(&extra_callable).is_err());
    assert!(route_receipt
        .validate_against_program(&extra_callable)
        .is_err());
}

#[test]
fn ordinary_map_new_result_l1_ok_reference_matches_bytecode() {
    let program = ordinary_map_new_result_l1_program();
    let owner = crate::core::NodeId("function:main".into());
    let function = &program.functions()[&owner];
    let receipt = function
        .ownership
        .ordinary_map_new_result_receipt()
        .unwrap();
    assert!(function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .is_empty());
    assert!(function
        .ownership
        .ordinary_map_owned_any_v2_cfg_receipt()
        .is_none());
    assert_eq!(function.blocks.len(), 4);
    let result = MirValueId::new(format!("local:{}", receipt.result_local.0 .0)).unwrap();
    assert_eq!(
        function.values[&result].ty,
        types::ordinary_map_owned_any_v2_root_result_type_id()
    );
    let err = MirBlockId::new(receipt.err_block.0 .0.clone()).unwrap();
    assert!(function.blocks[&err].instructions.is_empty());
    assert!(function.blocks[&err]
        .parameters
        .iter()
        .all(|parameter| function.values[&parameter.value].ty
            == crate::core::ir::primitive_type_id(PrimitiveType::I32)));
    let reference = reference::MirReferenceInterpreter::new(&program)
        .execute(&owner, &[])
        .unwrap();
    assert_eq!(reference, reference::MirRuntimeValue::Int(0));
    let bytecode = crate::interp::bytecode::mir::compile_mir_map_new_result(&program).unwrap();
    assert!(bytecode.functions[0].code.iter().any(|op| matches!(
        op,
        crate::interp::bytecode::Op::MirOrdinaryMapOwnedAnyV2NewResult { .. }
    )));
    let before_bytecode = crate::runtime::ordinary_map_domain_live_counts_for_test()
        .expect("ownership domains before bytecode NewResult route");
    let mut vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
    let actual = vm.run_value_for_ordinary_map_failure_model_test().unwrap();
    assert!(matches!(actual, crate::interp::Value::Int(0)));
    assert_eq!(
        crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains after bytecode NewResult route"),
        before_bytecode,
        "bytecode New, real Size, and exact Drop leave no root behind"
    );
    assert_eq!(
        vm.ordinary_map_owned_any_v2_live_owner_occurrences_for_test(),
        0
    );
    assert_eq!(
        vm.ordinary_map_new_result_live_runtime_root_occurrences_for_test(),
        0
    );
}

#[test]
fn ordinary_map_new_result_l1_capacity_is_err3_without_root_or_trap() {
    reference::with_ordinary_map_new_result_status_for_test(3, || {
        let raw = reference::ordinary_map_new_result_reference_value().unwrap();
        assert!(
            matches!(raw, reference::MirRuntimeValue::Variant { variant, payload, .. }
            if variant.0 == "builtin:variant:Result::Err" && payload == vec![reference::MirRuntimeValue::Int(3)])
        );
        let program = ordinary_map_new_result_l1_program();
        let result = reference::MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .unwrap();
        assert_eq!(result, reference::MirRuntimeValue::Int(3));
    });
    // Bytecode uses the real New ABI and turns only its exact Capacity result
    // into Result::Err(3).
    let before_runtime_capacity = crate::runtime::ordinary_map_domain_live_counts_for_test()
        .expect("ownership domains before runtime New Capacity");
    crate::runtime::with_ordinary_map_owned_any_v2_root_publish_failure_for_test(0, || {
        let program = ordinary_map_new_result_l1_program();
        let bytecode = crate::interp::bytecode::mir::compile_mir_map_new_result(&program).unwrap();
        let mut vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
        assert!(matches!(
            vm.run_value_for_ordinary_map_failure_model_test().unwrap(),
            crate::interp::Value::Int(3)
        ));
        assert_eq!(
            vm.ordinary_map_new_result_live_runtime_root_occurrences_for_test(),
            0
        );
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after real New Capacity"),
            before_runtime_capacity,
            "the real New ABI returns exactly Capacity with no published handle"
        );
    });
}

#[test]
fn ordinary_map_new_result_l1_invalid_invariant_statuses_fail_closed() {
    let program = ordinary_map_new_result_l1_program();
    for status in [-1, 1, 2, 4, i32::MAX] {
        reference::with_ordinary_map_new_result_status_for_test(status, || {
            let error = reference::MirReferenceInterpreter::new(&program)
                .execute(&crate::core::NodeId("function:main".into()), &[])
                .unwrap_err();
            assert!(error
                .to_string()
                .contains("[E0800] ordinary Map NewResult invalid"));
        });
    }
}

#[test]
fn ordinary_map_new_result_l1_bytecode_size_error_drops_only_its_exact_handle() {
    let before = crate::runtime::ordinary_map_domain_live_counts_for_test()
        .expect("ownership domains before unrelated owner setup");
    let unrelated = crate::runtime::OwnedAnyV2Root::try_new()
        .expect("an unrelated runtime owner remains live during bytecode unwind");
    let unrelated_handle = unrelated.handle();
    let with_unrelated = crate::runtime::ordinary_map_domain_live_counts_for_test()
        .expect("ownership domains with one unrelated root");
    assert_eq!(with_unrelated.roots, before.roots + 1);

    let program = ordinary_map_new_result_l1_program();
    let bytecode = crate::interp::bytecode::mir::compile_mir_map_new_result(&program).unwrap();
    let mut vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
    let error =
        crate::interp::bytecode::vm::with_ordinary_map_new_result_size_failure_for_test(|| {
            vm.run_value_for_ordinary_map_failure_model_test()
        })
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("ordinary Map NewResult Size failure injected"));
    assert_eq!(
        vm.ordinary_map_new_result_live_runtime_root_occurrences_for_test(),
        0,
        "error cleanup clears the exact owner before recycling registers"
    );
    assert_eq!(
        crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains after NewResult Size failure"),
        with_unrelated,
        "unwind retires only the root created by this NewResult"
    );
    assert_eq!(unrelated.handle(), unrelated_handle);
    assert_eq!(unrelated.size(), Ok(0));
    assert!(unrelated.try_drop().is_ok());
    assert_eq!(
        crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains after retiring the unrelated root"),
        before
    );
}

#[test]
fn ordinary_map_new_result_l1_rejects_forged_receipt_edges_and_generic_clone() {
    let program = ordinary_map_new_result_l1_program();
    let owner = crate::core::NodeId("function:main".into());
    let original = program.functions()[&owner].clone();
    let receipt = original
        .ownership
        .ordinary_map_new_result_receipt()
        .unwrap()
        .clone();
    let err_block = MirBlockId::new(receipt.err_block.0 .0.clone()).unwrap();
    for mutation in 0..5 {
        let mut forged = original.clone();
        match mutation {
            0 => {
                forged
                    .ownership
                    .checker_ordinary_map_owned_any_v2_new_result
                    .as_mut()
                    .unwrap()
                    .root_local = receipt.status_local.clone()
            }
            1 => {
                let MirTerminator::SwitchMove { arms, .. } =
                    &mut forged.blocks.get_mut(&forged.entry).unwrap().terminator
                else {
                    unreachable!()
                };
                arms.swap(0, 1);
                let target = arms[0].target.clone();
                arms[0].target = arms[1].target.clone();
                arms[1].target = target;
            }
            2 => {
                let root = MirValueId::new(format!("local:{}", receipt.root_local.0 .0)).unwrap();
                forged.blocks.get_mut(&err_block).unwrap().parameters[0].value = root;
            }
            3 => {
                let ok = MirBlockId::new(receipt.ok_block.0 .0.clone()).unwrap();
                forged.blocks.get_mut(&ok).unwrap().instructions.pop();
            }
            _ => {
                let source =
                    MirValueId::new(format!("local:{}", receipt.result_local.0 .0)).unwrap();
                let result = MirValueId::new("forged:clone:root-result").unwrap();
                forged.values.insert(
                    result.clone(),
                    MirValue {
                        id: result.clone(),
                        ty: types::ordinary_map_owned_any_v2_root_result_type_id(),
                    },
                );
                forged
                    .blocks
                    .get_mut(&forged.entry)
                    .unwrap()
                    .instructions
                    .push(MirInstruction {
                        id: MirInstructionId::new("forged:generic:clone").unwrap(),
                        kind: MirInstructionKind::Clone { result, source },
                    });
            }
        }
        assert!(forged.validate().is_err(), "mutation {mutation}");
        assert!(reference::MirProgram::with_type_catalog(
            BTreeMap::from([(owner.clone(), forged)]),
            program.type_catalog().clone()
        )
        .is_err());
    }
    let result_ty = types::ordinary_map_owned_any_v2_root_result_type_id();
    assert!(
        !program
            .type_catalog()
            .get(&result_ty)
            .unwrap()
            .needs_clone_glue
    );
    assert!(program
        .type_catalog()
        .validate_glue(&result_ty, types::MirGlueOperation::Clone)
        .is_err());
}

#[test]
fn ordinary_map_new_result_l1_rejects_old_routes_bytecode_forgery_and_production_run() {
    use crate::interp::bytecode::instr::CanonicalMapAnyOwnerV1BytecodeReceipt;
    let program = ordinary_map_new_result_l1_program();
    let core_route = validate_ordinary_map_new_result_l1_core_mir_route(&program)
        .expect("the isolated Core MIR receipt validates without opening consumers");
    assert!(crate::interp::bytecode::mir::compile_mir_program(&program).is_err());
    assert!(
        crate::interp::bytecode::mir::compile_mir_program_with_route_receipt(&program, &core_route)
            .is_err()
    );
    let old = program.route_receipt(CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2.as_str());
    assert!(
        crate::interp::bytecode::mir::compile_mir_program_with_route_receipt(&program, &old)
            .is_err()
    );
    assert!(crate::codegen::mir::validate_mir_native(&program).is_err());
    assert!(crate::verifier::validate_mir_capabilities(&program).is_err());
    let bytecode = crate::interp::bytecode::mir::compile_mir_map_new_result(&program).unwrap();
    assert_eq!(
        crate::interp::bytecode::BytecodeVM::new(bytecode.clone())
            .run_value()
            .expect("the exact NewResult bytecode route executes in production"),
        crate::interp::Value::Int(0)
    );
    for mutation in 0..4 {
        let mut forged = (*bytecode).clone();
        match mutation {
            0 => forged.canonical_map_any_owner_v1_receipt = None,
            1 => forged.canonical_map_any_owner_v1_receipt = Some(
                CanonicalMapAnyOwnerV1BytecodeReceipt::capture_with_ordinary_map_owner_lifecycle_i32_string_v1(forged.entry, &forged.functions)),
            2 => {
                let op = forged.functions[0].code.iter_mut().find(|op|
                    matches!(op, crate::interp::bytecode::Op::MirOrdinaryMapOwnedAnyV2NewResult { .. })).unwrap();
                let crate::interp::bytecode::Op::MirOrdinaryMapOwnedAnyV2NewResult { rd } = *op else { unreachable!() };
                *op = crate::interp::bytecode::Op::MirOrdinaryMapOwnedAnyV2New { rd };
            }
            _ => {
                let root_move = forged.functions[0]
                    .code
                    .iter()
                    .find(|op| matches!(op, crate::interp::bytecode::Op::Move { .. }))
                    .copied()
                    .expect("the exact source emits one root payload move");
                forged.functions[0].code.push(root_move);
            }
        }
        assert!(
            crate::interp::bytecode::BytecodeVM::new(std::sync::Arc::new(forged))
                .run_value_for_ordinary_map_failure_model_test()
                .is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
fn ordinary_map_new_result_l1_supported_and_unsupported_sources_keep_default_route_closed() {
    for source in [
        ORDINARY_MAP_NEW_RESULT_L1_SOURCE.to_owned(),
        ORDINARY_MAP_NEW_RESULT_L1_SOURCE.replace("Err(status) => status", "Err(status) => 7"),
        ORDINARY_MAP_NEW_RESULT_L1_SOURCE.replace("Err(status) => status", "_ => 7"),
        ORDINARY_MAP_NEW_RESULT_L1_SOURCE.replace("drop(root)", ""),
    ] {
        let checked = checked_program(&source);
        let admission = classify_canonical_mir_route_admission(&checked);
        assert_eq!(
            admission.ordinary_map_owned_any_v2,
            OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage
        );
        assert!(materialize_canonical_mir_route(&checked, None).is_err());
    }
    const {
        assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
}

#[test]
fn ordinary_map_new_result_l1_rejects_forged_descriptor_and_err_root_glue() {
    let program = ordinary_map_new_result_l1_program();
    let result_ty = types::ordinary_map_owned_any_v2_root_result_type_id();
    for mutation in 0..3 {
        let mut catalog = program.type_catalog().clone();
        let mut descriptor = catalog.get(&result_ty).unwrap().clone();
        match mutation {
            0 => {
                descriptor.needs_clone_glue = true;
                descriptor.glue.clone = MirGlueKind::Aggregate;
            }
            1 => {
                descriptor.variant_drop_plan.as_mut().unwrap()[1].fields[0].ty =
                    types::ordinary_map_owned_any_v2_type_id()
            }
            _ => descriptor.ownership = MirOwnership::Copy,
        }
        catalog.replace_for_test_only(result_ty.clone(), descriptor);
        assert!(
            reference::MirProgram::with_type_catalog(program.functions().clone(), catalog).is_err(),
            "mutation {mutation}"
        );
    }
}

fn ordinary_map_set_get_i32_fixture() -> (crate::core::CheckedProgram, reference::MirProgram) {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_i32_lineage.mimi"
    ));
    let owner = crate::core::NodeId("function:main".into());
    let receipt = checked
        .callable(&owner)
        .and_then(|callable| {
            callable
                .resources
                .ordinary_map_owned_any_v2_set_get_i32_lineage_receipt
                .as_ref()
        })
        .expect("exact fixture yields a source-bound Set/Get-i32 receipt");
    crate::core::cfg::validate_ordinary_map_owned_any_v2_set_get_i32_lineage_receipt(
        receipt,
        &checked.callable(&owner).unwrap().cfg,
        &checked.callable(&owner).unwrap().body,
        &checked.callable(&owner).unwrap().signature,
        checked.resolved_types(),
    )
    .expect("the exact Checker receipt replays against its source artifacts");
    let program =
        reference::MirProgram::from_checked_program_map_set_get_i32_result(&checked, &owner)
            .expect("exact fixture materializes source-bound Set/Get-i32 MIR");
    (checked, program)
}

const ORDINARY_MAP_SET_GET_STRING_SOURCE: &str = include_str!(
    "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_string_lineage.mimi"
);

fn ordinary_map_set_get_string_fixture() -> (crate::core::CheckedProgram, reference::MirProgram) {
    let checked = checked_program(ORDINARY_MAP_SET_GET_STRING_SOURCE);
    let owner = crate::core::NodeId("function:main".into());
    let receipt = checked
        .callable(&owner)
        .and_then(|callable| {
            callable
                .resources
                .ordinary_map_owned_any_v2_set_get_string_lineage_receipt
                .as_ref()
        })
        .expect("exact source yields its independent String Set/Get receipt");
    crate::core::cfg::validate_ordinary_map_owned_any_v2_set_get_string_lineage_receipt(
        receipt,
        &checked.callable(&owner).unwrap().cfg,
        &checked.callable(&owner).unwrap().body,
        &checked.callable(&owner).unwrap().signature,
        checked.resolved_types(),
    )
    .expect("the String Checker receipt replays against its exact source CFG");
    let program =
        reference::MirProgram::from_checked_program_map_set_get_string_result(&checked, &owner)
            .expect("exact source materializes dedicated SetString/GetString MIR");
    (checked, program)
}

fn ordinary_map_set_get_string_l2_checked_fixture() -> crate::core::CheckedProgram {
    let source = ORDINARY_MAP_SET_GET_STRING_SOURCE.replacen(
        "func main() -> i32 {",
        "func main() -> i32 {\n    ensures: result == 42 || result == 2 || result == 3",
        1,
    );
    checked_program(&source)
}

fn ordinary_map_set_get_string_false_claim_l2_checked_fixture() -> crate::core::CheckedProgram {
    let source = ORDINARY_MAP_SET_GET_STRING_SOURCE.replacen(
        "func main() -> i32 {",
        "func main() -> i32 {\n    ensures: result == 42 || result == 3",
        1,
    );
    checked_program(&source)
}

#[test]
fn ordinary_map_set_get_string_receipt_routes_and_executes_independently() {
    let owner = crate::core::NodeId("function:main".into());
    let (checked, expected) = ordinary_map_set_get_string_fixture();
    assert!(has_ordinary_map_set_get_string_route_candidate(&checked));
    let program = materialize_ordinary_map_set_get_string_route(&checked)
        .expect("exact Checker receipt materializes and passes L1 Core replay");
    assert_eq!(program, expected);
    let route = validate_ordinary_map_set_get_string_l1_core_mir_route(&program)
        .expect("String lineage graph carries its dedicated L1 receipt");
    assert_eq!(route.profile, ORDINARY_MAP_SET_GET_STRING_L1_CORE_PROFILE);
    assert_ne!(route.profile, ORDINARY_MAP_SET_GET_I32_L1_CORE_PROFILE);
    route
        .validate_against_program(&program)
        .expect("the String profile receipt replays against its graph");

    assert!(
        materialize_ordinary_map_set_get_string_verifier_route(&checked).is_err(),
        "contract-free source must not materialize the String L2 verifier profile"
    );
    let verifier_checked = ordinary_map_set_get_string_l2_checked_fixture();
    let verifier = materialize_ordinary_map_set_get_string_verifier_route(&verifier_checked)
        .expect("exact direct Ensures source materializes the isolated L2 String profile");
    let verifier_receipt =
        validate_ordinary_map_set_get_string_l2_verifier_route(&verifier_checked, &verifier)
            .expect("the L2 route is bound to the complete CheckedProgram");
    assert_eq!(
        verifier_receipt.profile,
        ORDINARY_MAP_SET_GET_STRING_L2_VERIFIER_PROFILE
    );

    let false_claim_checked = ordinary_map_set_get_string_false_claim_l2_checked_fixture();
    assert!(
        !has_ordinary_map_set_get_string_route_candidate(&false_claim_checked),
        "a verifier-only false claim must not select the contract-free L1 execution route"
    );
    assert!(materialize_ordinary_map_set_get_string_route(&false_claim_checked).is_err());
    let false_claim_verifier =
        materialize_ordinary_map_set_get_string_verifier_route(&false_claim_checked)
            .expect("the exact false claim reaches the L2 verifier profile for Disproven");
    let false_claim_receipt = validate_ordinary_map_set_get_string_l2_verifier_route(
        &false_claim_checked,
        &false_claim_verifier,
    )
    .expect("the false claim retains exact Checked contracts and the Checker-rooted worklist");
    assert_eq!(
        false_claim_receipt.profile,
        ORDINARY_MAP_SET_GET_STRING_L2_VERIFIER_PROFILE
    );
    assert_eq!(
        false_claim_verifier.functions()[&owner]
            .proof_worklist
            .as_ref()
            .expect("false claim retains all three Checker-rooted calls")
            .expected_calls
            .len(),
        3
    );

    let verifier_function = &verifier.functions()[&owner];
    assert_eq!(verifier_function.contracts.len(), 1);
    assert_eq!(
        verifier_function.contracts[0].kind,
        MirContractKind::Ensures
    );
    assert_eq!(
        verifier_function
            .proof_worklist
            .as_ref()
            .expect("String L2 retains the exact Checker-rooted call worklist")
            .expected_calls
            .len(),
        3
    );
    let function = &program.functions()[&owner];
    assert_eq!(function.blocks.len(), 13);
    assert_eq!(
        function.ownership.checker_ordinary_map_owned_any_v2.len(),
        0
    );
    assert_eq!(
        function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(
                instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2SetStringResult { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(
                instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2GetStringResult { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        reference::MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("Reference settles the source and detached String owners"),
        reference::MirRuntimeValue::Int(42)
    );

    {
        let status = 2;
        reference::with_ordinary_map_set_string_status_for_test(status, || {
            assert_eq!(
                reference::MirReferenceInterpreter::new(&program)
                    .execute(&owner, &[])
                    .expect("only the exact Set String capacity status propagates through Err"),
                reference::MirRuntimeValue::Int(i64::from(status))
            );
        });
    }
    for status in [1, 18] {
        reference::with_ordinary_map_set_string_status_for_test(status, || {
            let error = reference::MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect_err("InvalidInput and TypeMismatch remain fail-closed");
            assert!(
                error.message.contains(&format!("status {status}")),
                "{error}"
            );
        });
    }
    assert_eq!(
        reference::ordinary_map_set_string_status_for_test(),
        None,
        "Set status injection must be restored before testing Get status edges"
    );
    for status in 1..=3 {
        reference::with_ordinary_map_get_string_status_for_test(status, || {
            assert_eq!(
                reference::MirReferenceInterpreter::new(&program)
                    .execute(&owner, &[])
                    .expect("documented GetStringResult status propagates through Err"),
                reference::MirRuntimeValue::Int(i64::from(status))
            );
        });
    }
    assert_eq!(
        reference::ordinary_map_get_string_status_for_test(),
        None,
        "Get status injection must be restored before testing invalid Get status"
    );
    reference::with_ordinary_map_get_string_status_for_test(4, || {
        let error = reference::MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect_err("GetStringResult internal status is not source Err");
        assert!(error.message.contains("internal status"), "{error}");
    });
    reference::with_ordinary_map_set_string_status_for_test(17, || {
        let error = reference::MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect_err("original nonrecoverable Set status remains an internal failure");
        assert!(error.message.contains("status 17"), "{error}");
    });

    let changed_key = r#"
func main() -> i32 {
    let created = map_new()
    match created {
        Ok(root) => {
            let text = "payload"
            let changed = map_set(root, "answer", text)
            drop(text)
            match changed {
                Ok(updated) => {
                    drop(root)
                    let lookup = map_get_string(updated, "other")
                    match lookup {
                        Ok(value) => { drop(updated); drop(value); 0 },
                        Err(status) => { drop(updated); status },
                    }
                },
                Err(status) => { drop(root); status },
            }
        },
        Err(status) => status,
    }
}
"#;
    let near_miss = checked_program(changed_key);
    assert!(!has_ordinary_map_set_get_string_route_candidate(&near_miss));
    assert!(materialize_ordinary_map_set_get_string_route(&near_miss).is_err());

    for near_miss_source in [
        ORDINARY_MAP_SET_GET_STRING_SOURCE.replace("value == \"payload\"", "value != \"payload\""),
        ORDINARY_MAP_SET_GET_STRING_SOURCE
            .replace("value == \"payload\"", "value == \"different\""),
        ORDINARY_MAP_SET_GET_STRING_SOURCE.replace(
            "if matches { 42 } else { 0 }",
            "if matches { 0 } else { 42 }",
        ),
        ORDINARY_MAP_SET_GET_STRING_SOURCE.replace(
            "drop(updated)\n                            drop(value)",
            "drop(value)\n                            drop(updated)",
        ),
    ] {
        let near_miss = checked_program(&near_miss_source);
        assert!(
            !has_ordinary_map_set_get_string_route_candidate(&near_miss),
            "payload, result-branch, and cleanup near-misses must not acquire the String receipt"
        );
        assert!(materialize_ordinary_map_set_get_string_route(&near_miss).is_err());
    }

    let exact_l2 = ORDINARY_MAP_SET_GET_STRING_SOURCE.replacen(
        "func main() -> i32 {",
        "func main() -> i32 {\n    ensures: result == 42 || result == 2 || result == 3",
        1,
    );
    let nested_ensure = ORDINARY_MAP_SET_GET_STRING_SOURCE.replacen(
        "            let lookup = map_get_string(updated, \"answer\")",
        "            ensures: result == 42 || result == 2 || result == 3\n            let lookup = map_get_string(updated, \"answer\")",
        1,
    );
    let interleaved_ensure = exact_l2
        .replacen(
            "    ensures: result == 42 || result == 2 || result == 3\n",
            "",
            1,
        )
        .replacen(
            "    let created = map_new()",
            "    let created = map_new()\n    ensures: result == 42 || result == 2 || result == 3",
            1,
        );
    for near_miss_source in [
        exact_l2.replace(
            "result == 42 || result == 2 || result == 3",
            "result == 42 || result == 2 || result == 4",
        ),
        exact_l2.replace(
            "result == 42 || result == 2 || result == 3",
            "result == 42 || result == 3 || result == 2",
        ),
        exact_l2.replacen(
            "    ensures: result == 42 || result == 2 || result == 3",
            "    requires: true\n    ensures: result == 42 || result == 2 || result == 3",
            1,
        ),
        exact_l2.replacen(
            "    ensures: result == 42 || result == 2 || result == 3",
            "    ensures: result == 42 || result == 2 || result == 3\n    ensures: true",
            1,
        ),
        interleaved_ensure,
        nested_ensure,
    ] {
        if let Ok(near_miss) = std::panic::catch_unwind(|| checked_program(&near_miss_source)) {
            assert!(
                materialize_ordinary_map_set_get_string_verifier_route(&near_miss).is_err(),
                "wrong, extra, interleaved, or nested String proof roots must fail closed"
            );
        }
    }
}

#[test]
fn ordinary_map_set_get_i32_route_replays_the_exact_checker_receipt() {
    let (checked, expected) = ordinary_map_set_get_i32_fixture();
    assert!(has_ordinary_map_set_get_i32_route_candidate(&checked));
    let program = materialize_ordinary_map_set_get_i32_route(&checked)
        .expect("exact Checker receipt materializes and passes independent Core L1 replay");
    assert_eq!(program, expected);
    let route = validate_ordinary_map_set_get_i32_l1_core_mir_route(&program)
        .expect("exact Set/Get-i32 MIR graph carries its L1 Core route receipt");
    assert_eq!(route.profile, ORDINARY_MAP_SET_GET_I32_L1_CORE_PROFILE);
    assert_ne!(
        route.profile, ORDINARY_MAP_SET_RESULT_L1_CORE_PROFILE,
        "SetResult cannot authorize the later typed Get lineage"
    );
    route
        .validate_against_program(&program)
        .expect("the profile receipt replays against the unchanged graph");

    let owner = crate::core::NodeId("function:main".into());
    let mut forged = program.clone();
    forged
        .function_mut_for_test(&owner)
        .unwrap()
        .ownership
        .checker_ordinary_map_owned_any_v2_set_get_i32_lineage_commitment =
        Some("forged-route-lineage".into());
    assert!(validate_ordinary_map_set_get_i32_l1_core_mir_route(&forged).is_err());
    assert!(route.validate_against_program(&forged).is_err());

    let wrong_key = include_str!(
        "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_i32_lineage.mimi"
    )
    .replacen(
        "map_get_i32(updated, \"answer\")",
        "map_get_i32(updated, \"different\")",
        1,
    );
    let near_miss = checked_program(&wrong_key);
    assert!(!has_ordinary_map_set_get_i32_route_candidate(&near_miss));
    assert!(materialize_ordinary_map_set_get_i32_route(&near_miss).is_err());
}

#[test]
fn ordinary_map_set_get_i32_result_materializes_and_executes_exact_source() {
    let (_checked, program) = ordinary_map_set_get_i32_fixture();
    let owner = crate::core::NodeId("function:main".into());
    let function = &program.functions()[&owner];
    assert_eq!(function.blocks.len(), 10);
    assert_eq!(
        function.ownership.checker_ordinary_map_owned_any_v2.len(),
        0
    );
    assert_eq!(
        function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(
                instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { .. }
            ))
            .count(),
        1
    );
    let get_block = function
        .blocks
        .values()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { .. }
                )
            })
        })
        .expect("Set-success block has the dedicated GetI32Result instruction");
    let get_index = get_block
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { .. }
            )
        })
        .unwrap();
    let get_key = match &get_block.instructions[get_index].kind {
        MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { key, .. } => key,
        _ => unreachable!(),
    };
    let updated_root = match &get_block.instructions[get_index].kind {
        MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { root, .. } => root,
        _ => unreachable!(),
    };
    assert!(
        matches!(
            get_block.instructions.get(get_index + 1).map(|instruction| &instruction.kind),
            Some(MirInstructionKind::Drop { value }) if value == get_key
        ),
        "Get key is dropped immediately after the borrowed lookup"
    );
    let updated_root_drops = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root }
                    if root == updated_root
            )
        })
        .count();
    assert_eq!(
        updated_root_drops, 2,
        "both Get Result arms settle the updated root"
    );

    assert_eq!(
        reference::MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("Reference executes both Result matches and all root drops"),
        reference::MirRuntimeValue::Int(42)
    );
}

#[test]
fn ordinary_map_set_get_i32_result_propagates_status_and_rejects_forgery() {
    let (_checked, program) = ordinary_map_set_get_i32_fixture();
    let owner = crate::core::NodeId("function:main".into());
    let updated_root = program.functions()[&owner]
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { root, .. } => {
                Some(root.clone())
            }
            _ => None,
        })
        .expect("the source-bound GetI32Result borrows the updated root");
    for status in 1..=3 {
        reference::with_ordinary_map_set_get_i32_status_for_test(status, || {
            assert_eq!(
                reference::MirReferenceInterpreter::new(&program)
                    .execute(&owner, &[])
                    .expect("documented Get status is the exact Result::Err payload"),
                reference::MirRuntimeValue::Int(i64::from(status))
            );
        });
    }
    reference::with_ordinary_map_set_get_i32_status_for_test(4, || {
        let error = reference::MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect_err("unknown Get status is an internal invariant error");
        assert!(
            error.message.contains("invalid invariant status"),
            "{error}"
        );
    });
    reference::with_ordinary_map_set_result_status_for_test(2, || {
        assert_eq!(
            reference::MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("Set-capacity status propagates unchanged"),
            reference::MirRuntimeValue::Int(2)
        );
    });
    reference::with_ordinary_map_new_result_status_for_test(3, || {
        assert_eq!(
            reference::MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("outer New status propagates unchanged"),
            reference::MirRuntimeValue::Int(3)
        );
    });

    let mut forged = program.clone();
    forged
        .function_mut_for_test(&owner)
        .unwrap()
        .ownership
        .checker_ordinary_map_owned_any_v2_set_get_i32_lineage_commitment =
        Some("forged-set-get-i32-commitment".into());
    assert!(forged.functions()[&owner].validate().is_err());

    let mut missing_get_drop = program.clone();
    let function = missing_get_drop.function_mut_for_test(&owner).unwrap();
    let block = function
        .blocks
        .values_mut()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { .. }
                )
            })
        })
        .unwrap();
    let index = block
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { .. }
            )
        })
        .unwrap();
    block.instructions.remove(index + 1);
    assert!(
        function.validate().is_err(),
        "the Get key Drop is mandatory"
    );

    let mut missing_get_root_drop = program.clone();
    let function = missing_get_root_drop.function_mut_for_test(&owner).unwrap();
    let block = function
        .blocks
        .values_mut()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root }
                        if root == &updated_root
                )
            })
        })
        .unwrap();
    let index = block
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root }
                        if root == &updated_root
            )
        })
        .unwrap();
    block.instructions.remove(index);
    assert!(
        function.validate().is_err(),
        "both Get Result arms must drop the updated root"
    );

    let source_without_exact_key = include_str!(
        "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_i32_lineage.mimi"
    )
    .replacen(
        "map_get_i32(updated, \"answer\")",
        "map_get_i32(updated, \"different\")",
        1,
    );
    let changed = checked_program(&source_without_exact_key);
    assert!(
        changed
            .callable(&owner)
            .unwrap()
            .resources
            .ordinary_map_owned_any_v2_set_get_i32_lineage_receipt
            .is_none(),
        "a changed Get key withholds the independent source receipt"
    );
    assert!(
        reference::MirProgram::from_checked_program_map_set_get_i32_result(&changed, &owner)
            .is_err()
    );
}

fn ordinary_map_set_result_l1_fixture() -> (crate::core::CheckedProgram, reference::MirProgram) {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_result_lineage.mimi"
    ));
    let owner = crate::core::NodeId("function:main".into());
    let program = reference::MirProgram::from_checked_program_map_set_result(&checked, &owner)
        .expect("exact fixture materializes its Checker-bound SetResult MIR");
    (checked, program)
}

#[test]
fn ordinary_map_set_result_l1_executes_exact_fixture_in_reference_and_bytecode() {
    let (checked, program) = ordinary_map_set_result_l1_fixture();
    let owner = crate::core::NodeId("function:main".into());
    let route = validate_ordinary_map_set_result_l1_core_mir_route(&program)
        .expect("exact Checker-bound SetResult graph validates in Core");
    assert_eq!(route.profile, ORDINARY_MAP_SET_RESULT_L1_CORE_PROFILE);
    let function = &program.functions()[&owner];
    assert_eq!(function.blocks.len(), 7);
    assert_eq!(
        function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(
                instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
            ))
            .count(),
        1
    );
    let key = super::MirValueId::new("literal:ordinary_map_set_lineage:answer").unwrap();
    let set_block = function
        .blocks
        .values()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { key: observed, .. }
                        if observed == &key
                )
            })
        })
        .expect("SetResult and its key cleanup share the exact block");
    let set_index = set_block
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { key: observed, .. }
                    if observed == &key
            )
        })
        .expect("the exact SetResult instruction is present");
    assert!(
        matches!(
            set_block
                .instructions
                .get(set_index + 1)
                .map(|instruction| &instruction.kind),
            Some(MirInstructionKind::Drop { value }) if value == &key
        ),
        "the owned SetResult key is retired on both Result branches before branching"
    );

    let reference = reference::MirReferenceInterpreter::new(&program)
        .execute(&owner, &[])
        .expect("Reference executes both Result branches and ownership settlement");
    assert_eq!(reference, reference::MirRuntimeValue::Int(1));

    let before = crate::runtime::ordinary_map_domain_live_counts_for_test()
        .expect("runtime ownership baseline before SetResult Bytecode");
    let bytecode = crate::interp::bytecode::mir::compile_mir_program_with_map_set_result_route(
        &checked, &program,
    )
    .expect("CheckedProgram-aware SetResult adapter compiles the exact route");
    assert!(bytecode.functions[0].code.iter().any(|op| matches!(
        op,
        crate::interp::bytecode::Op::MirOrdinaryMapOwnedAnyV2SetResult { .. }
    )));
    let mut vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value()
            .expect("Bytecode runs New, SetResult, Size, and exact Drops"),
        crate::interp::Value::Int(1)
    );
    assert_eq!(
        crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("runtime ownership after SetResult Bytecode"),
        before,
        "success must retire both the original and updated roots"
    );
    assert_eq!(
        vm.ordinary_map_new_result_live_runtime_root_occurrences_for_test(),
        0
    );
}

#[test]
fn ordinary_map_set_result_l1_propagates_exact_set_and_outer_new_statuses() {
    let (checked, program) = ordinary_map_set_result_l1_fixture();
    let owner = crate::core::NodeId("function:main".into());
    reference::with_ordinary_map_set_result_status_for_test(2, || {
        assert_eq!(
            reference::MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("Set capacity is a recoverable Err(2)"),
            reference::MirRuntimeValue::Int(2)
        );
    });
    reference::with_ordinary_map_new_result_status_for_test(3, || {
        assert_eq!(
            reference::MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("outer New capacity propagates its exact Err(3)"),
            reference::MirRuntimeValue::Int(3)
        );
    });

    let bytecode = crate::interp::bytecode::mir::compile_mir_program_with_map_set_result_route(
        &checked, &program,
    )
    .expect("CheckedProgram-aware SetResult adapter compiles the exact route");
    let before = crate::runtime::ordinary_map_domain_live_counts_for_test()
        .expect("runtime ownership baseline before Set capacity");
    let actual =
        crate::runtime::with_ordinary_map_owned_any_v2_root_publish_failure_for_test(1, || {
            let mut vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
            let result = vm.run_value();
            (
                result,
                vm.ordinary_map_new_result_live_runtime_root_occurrences_for_test(),
            )
        });
    let (result, live_result_roots) = actual;
    assert_eq!(
        result.expect("Set publication capacity is a Result value"),
        crate::interp::Value::Int(2)
    );
    assert_eq!(live_result_roots, 0);
    assert_eq!(
        crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("runtime ownership after Set capacity"),
        before,
        "the old root remains owned until its branch Drop, with no published Set root"
    );
}

#[test]
fn ordinary_map_set_result_l2_proves_finite_error_domain_and_rejects_forgery() {
    let (checked, program) = ordinary_map_set_result_l1_fixture();
    let owner = crate::core::NodeId("function:main".into());
    let route = validate_ordinary_map_set_result_l1_core_mir_route(&program)
        .expect("valid SetResult L1 route");

    let mut forged = program.clone();
    let function = forged.function_mut_for_test(&owner).unwrap();
    function
        .ownership
        .checker_ordinary_map_owned_any_v2_set_lineage_commitment =
        Some("forged-set-lineage-commitment".into());
    assert!(validate_ordinary_map_set_result_l1_core_mir_route(&forged).is_err());
    assert!(route.validate_against_program(&forged).is_err());
    assert!(
        crate::interp::bytecode::mir::compile_mir_program_with_map_set_result_route(
            &checked, &forged
        )
        .is_err()
    );

    let mut wrong_operand = program.clone();
    let function = wrong_operand.function_mut_for_test(&owner).unwrap();
    let set = function
        .blocks
        .values_mut()
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
            )
        })
        .expect("the exact SetResult operation exists");
    let MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { key, value, .. } = &mut set.kind
    else {
        unreachable!()
    };
    std::mem::swap(key, value);
    assert!(validate_ordinary_map_set_result_l1_core_mir_route(&wrong_operand).is_err());

    let mut missing_key_drop = program.clone();
    let function = missing_key_drop.function_mut_for_test(&owner).unwrap();
    let set_block = function
        .blocks
        .values_mut()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
                )
            })
        })
        .expect("the exact SetResult block exists");
    let set_index = set_block
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
            )
        })
        .expect("the exact SetResult instruction exists");
    assert!(matches!(
        set_block
            .instructions
            .get(set_index + 1)
            .map(|instruction| &instruction.kind),
        Some(MirInstructionKind::Drop { .. })
    ));
    let key_drop = set_block.instructions[set_index + 1].clone();
    set_block.instructions.remove(set_index + 1);
    assert!(
        validate_ordinary_map_set_result_l1_core_mir_route(&missing_key_drop).is_err(),
        "the exact SetResult route rejects omission of its owned key cleanup"
    );

    let mut wrong_key_drop = program.clone();
    let function = wrong_key_drop.function_mut_for_test(&owner).unwrap();
    let drop = function
        .blocks
        .values_mut()
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::Drop { .. }))
        .expect("the exact key Drop exists");
    let MirInstructionKind::Drop { value } = &mut drop.kind else {
        unreachable!()
    };
    *value = super::MirValueId::new("literal:ordinary_map_set_lineage:42").unwrap();
    assert!(validate_ordinary_map_set_result_l1_core_mir_route(&wrong_key_drop).is_err());

    let mut duplicate_key_drop = program.clone();
    let function = duplicate_key_drop.function_mut_for_test(&owner).unwrap();
    let set_block = function
        .blocks
        .values_mut()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
                )
            })
        })
        .unwrap();
    let set_index = set_block
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
            )
        })
        .unwrap();
    set_block
        .instructions
        .insert(set_index + 2, key_drop.clone());
    assert!(validate_ordinary_map_set_result_l1_core_mir_route(&duplicate_key_drop).is_err());

    let mut displaced_key_drop = program.clone();
    let function = displaced_key_drop.function_mut_for_test(&owner).unwrap();
    let set_block = function
        .blocks
        .values_mut()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
                )
            })
        })
        .unwrap();
    let set_index = set_block
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
            )
        })
        .unwrap();
    set_block.instructions.swap(set_index, set_index + 1);
    assert!(validate_ordinary_map_set_result_l1_core_mir_route(&displaced_key_drop).is_err());

    if !crate::verifier::is_z3_available() {
        return;
    }
    // These Err branches return normally from main: a successful Set yields
    // size 1, Set capacity failure yields 2, and New capacity failure yields 3.
    // The exact postcondition is therefore the finite set {1, 2, 3}.
    let fixture = include_str!(
        "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_result_lineage.mimi"
    );
    let finite_domain_source = fixture.replacen(
        "func main() -> i32 {",
        "func main() -> i32 {\n    ensures: result == 1 || result == 2 || result == 3",
        1,
    );
    let source_hash = blake3::hash(finite_domain_source.as_bytes())
        .to_hex()
        .to_string();
    let checked_with_contract = checked_program(&finite_domain_source);
    let proven_program = materialize_ordinary_map_set_result_verifier_route(&checked_with_contract)
        .expect("exact Checker receipt admits the proof-bearing SetResult route");
    let receipt =
        validate_ordinary_map_set_result_l2_verifier_route(&checked_with_contract, &proven_program)
            .expect("the exact SetResult L2 route binds source, census, and MIR");
    assert_eq!(receipt.profile, ORDINARY_MAP_SET_RESULT_L2_VERIFIER_PROFILE);
    let results = crate::verifier::verify_mir_with_ordinary_map_set_result_l2_verifier_route(
        &checked_with_contract,
        &proven_program,
        &receipt,
        source_hash.clone(),
    )
    .expect("the verifier discharges the finite {1,2,3} result contract");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].func_name, "function:main");
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Proven);
    assert!(results[0].constraint_count > 0);
    let artifact = results[0]
        .artifact
        .as_ref()
        .expect("a real SetResult proof carries its provenance artifact");
    assert_eq!(artifact.source_hash, source_hash);
    assert_eq!(artifact.mir_hash, receipt.mir_digest);
    assert_eq!(artifact.mir_route_receipt.as_ref(), Some(&receipt));

    let wrong_profile = proven_program.route_receipt("verifier-mir-v1");
    let error = crate::verifier::verify_mir_with_ordinary_map_set_result_l2_verifier_route(
        &checked_with_contract,
        &proven_program,
        &wrong_profile,
        "wrong-set-result-profile".into(),
    )
    .expect_err("a broad verifier receipt cannot authorize the SetResult L2 route");
    assert!(error.contains("requires exact route profile"), "{error}");

    let mut missing_err_branch = proven_program.clone();
    let function = missing_err_branch.function_mut_for_test(&owner).unwrap();
    let removed_err_arm = function.blocks.values_mut().find_map(|block| {
        let MirTerminator::SwitchMove { arms, .. } = &mut block.terminator else {
            return None;
        };
        let before = arms.len();
        arms.retain(|arm| {
            !matches!(
                &arm.case,
                MirSwitchCase::Variant(variant)
                    if variant.0 == "builtin:variant:Result::Err"
            )
        });
        Some(before != arms.len())
    });
    assert_eq!(removed_err_arm, Some(true));
    assert!(
        validate_ordinary_map_set_result_l2_verifier_route(
            &checked_with_contract,
            &missing_err_branch,
        )
        .is_err(),
        "the exact L2 route rejects a graph with its Result::Err arm removed"
    );

    for (label, ensures, counterexample) in [
        (
            "omitted-set-err-2",
            "result == 1 || result == 3",
            "counterexample: result = 2",
        ),
        (
            "omitted-new-err-3",
            "result == 1 || result == 2",
            "counterexample: result = 3",
        ),
    ] {
        let source = fixture.replacen(
            "func main() -> i32 {",
            &format!("func main() -> i32 {{\n    ensures: {ensures}"),
            1,
        );
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let checked = checked_program(&source);
        let program = materialize_ordinary_map_set_result_verifier_route(&checked)
            .unwrap_or_else(|error| panic!("{label} exact route: {error}"));
        let receipt = validate_ordinary_map_set_result_l2_verifier_route(&checked, &program)
            .unwrap_or_else(|error| panic!("{label} exact receipt: {error}"));
        let results = crate::verifier::verify_mir_with_ordinary_map_set_result_l2_verifier_route(
            &checked,
            &program,
            &receipt,
            source_hash,
        )
        .unwrap_or_else(|error| panic!("{label} verifier route: {error}"));
        assert_eq!(results.len(), 1, "{label}");
        assert_eq!(
            results[0].status,
            crate::verifier::VerifStatus::Disproven,
            "{label}"
        );
        assert!(results[0].constraint_count > 0, "{label}");
        assert!(
            results[0].message.contains(counterexample),
            "{label}: {}",
            results[0].message
        );
    }
}

#[test]
fn ordinary_map_set_result_l2_is_neutral_and_checker_bound() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_result_lineage.mimi"
    ));
    let program = materialize_ordinary_map_set_result_verifier_route(&checked)
        .expect("contract-free exact SetResult graph has an L2 no-obligation projection");
    let receipt = validate_ordinary_map_set_result_l2_verifier_route(&checked, &program)
        .expect("CheckedProgram source receipt and full census validate");
    assert_eq!(receipt.profile, ORDINARY_MAP_SET_RESULT_L2_VERIFIER_PROFILE);
    let results = crate::verifier::verify_mir_with_ordinary_map_set_result_l2_verifier_route(
        &checked,
        &program,
        &receipt,
        "ordinary-map-set-result-no-obligations".into(),
    )
    .expect("dedicated L2 route returns neutral status without symbolic proof");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].func_name, "function:main");
    assert_eq!(
        results[0].status,
        crate::verifier::VerifStatus::NoObligations
    );
    assert_eq!(results[0].constraint_count, 0);
    assert!(results[0].artifact.is_none());
    assert!(results[0].message.contains("were not proven"));

    assert!(crate::verifier::verify_mir_with_route_receipt(
        &program,
        &receipt,
        "generic-must-reject-set-result".into(),
    )
    .expect_err("generic verifier entry stays closed for SetResult")
    .contains("exact CheckedProgram-aware L2 verifier route"));

    let mut forged = program.clone();
    let owner = crate::core::NodeId("function:main".into());
    forged
        .function_mut_for_test(&owner)
        .unwrap()
        .ownership
        .checker_ordinary_map_owned_any_v2_set_lineage_commitment =
        Some("forged-l2-set-lineage".into());
    assert!(validate_ordinary_map_set_result_l2_verifier_route(&checked, &forged).is_err());
    assert!(
        crate::verifier::verify_mir_with_ordinary_map_set_result_l2_verifier_route(
            &checked,
            &forged,
            &receipt,
            "forged-set-result-route".into(),
        )
        .is_err()
    );
}
