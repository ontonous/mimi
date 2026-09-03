use std::collections::BTreeMap;

use crate::core::ir::{PrimitiveType, ResolvedTypeId};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{
    MirLayout, MirRecordMoveProjectionDropContract, MirTypeKind, MirVariantProjectionTrapContract,
};
use crate::core::mir::{
    MirBlock, MirBlockId, MirFunction, MirInstruction, MirInstructionId, MirInstructionKind,
    MirOwnershipSummary, MirSwitchArm, MirSwitchBinding, MirSwitchCase, MirTerminator, MirValue,
    MirValueId,
};
use crate::core::NodeId;
use crate::lexer::Lexer;
use crate::parser::Parser;

/// A small canonical program used by backend tests for the direct variant
/// projection island.  The source only seeds the checker-owned Option<i32>
/// TypeDesc; the test replaces one already-canonical function body with the
/// hand-built MIR node so the source language does not gain an accidental new
/// projection syntax as a side effect of this architecture slice.
#[derive(Debug, Clone)]
pub(crate) struct DirectVariantProjectionFixture {
    pub(crate) program: MirProgram,
    pub(crate) function: NodeId,
    pub(crate) source_ty: ResolvedTypeId,
    pub(crate) result_ty: ResolvedTypeId,
    pub(crate) some: NodeId,
    pub(crate) none: NodeId,
    pub(crate) field: NodeId,
    pub(crate) receipt: MirVariantProjectionTrapContract,
}

pub(crate) fn direct_variant_projection_fixture() -> DirectVariantProjectionFixture {
    let source = "func project(value: Option<i32>) -> i32 {\n    ensures: result >= 0\n    0\n}\nfunc main() -> i32 { 0 }";
    let tokens = Lexer::new(source)
        .tokenize()
        .expect("direct projection lex");
    let file = Parser::new(tokens)
        .parse_file()
        .expect("direct projection parse");
    let checked = crate::core::check_program(&file).expect("direct projection check");
    let canonical = MirProgram::from_checked_program(&checked).expect("direct projection MIR");
    let catalog = canonical.type_catalog().clone();
    let result_ty = catalog
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == MirTypeKind::Primitive(PrimitiveType::I32)).then(|| id.clone())
        })
        .expect("i32 TypeDesc");
    let (source_ty, variants) = catalog
        .iter()
        .find_map(|(id, descriptor)| match &descriptor.layout {
            MirLayout::Option { inner, variants } if inner == &result_ty => {
                Some((id.clone(), variants.clone()))
            }
            _ => None,
        })
        .expect("Option<i32> TypeDesc");
    let some = variants
        .iter()
        .find(|variant| variant.name == "Some")
        .expect("Option Some TypeDesc")
        .id
        .clone();
    let none = variants
        .iter()
        .find(|variant| variant.name == "None")
        .expect("Option None TypeDesc")
        .id
        .clone();
    let field = variants
        .iter()
        .find(|variant| variant.id == some)
        .and_then(|variant| variant.fields.first())
        .expect("Option Some payload TypeDesc")
        .id
        .clone();
    let receipt = catalog
        .validated_variant_projection_trap_contract(&source_ty, &some, &field, &result_ty)
        .expect("direct variant projection receipt");
    let contracts = canonical
        .functions()
        .get(&NodeId("function:project".into()))
        .expect("project source function")
        .contracts
        .clone();

    let input = MirValueId::new("v.input").expect("input value id");
    let result = MirValueId::new("v.result").expect("result value id");
    let entry = MirBlockId::new("bb.entry").expect("entry block id");
    let mut functions = canonical.functions().clone();
    functions.insert(
        NodeId("function:project".into()),
        MirFunction {
            owner: NodeId("function:project".into()),
            parameters: vec![input.clone()],
            result: result_ty.clone(),
            entry: entry.clone(),
            values: BTreeMap::from([
                (
                    input.clone(),
                    MirValue {
                        id: input.clone(),
                        ty: source_ty.clone(),
                    },
                ),
                (
                    result.clone(),
                    MirValue {
                        id: result.clone(),
                        ty: result_ty.clone(),
                    },
                ),
            ]),
            blocks: BTreeMap::from([(
                entry.clone(),
                MirBlock {
                    id: entry,
                    parameters: Vec::new(),
                    instructions: vec![MirInstruction {
                        id: MirInstructionId::new("i.variant-project")
                            .expect("variant projection instruction id"),
                        kind: MirInstructionKind::VariantProject {
                            result: result.clone(),
                            base: input,
                            contract: Some(receipt.clone()),
                        },
                    }],
                    terminator: MirTerminator::Return {
                        value: Some(result),
                    },
                },
            )]),
            contracts,
            ownership: MirOwnershipSummary::default(),
        },
    );
    let program = MirProgram::with_type_catalog(functions, catalog)
        .expect("direct variant projection program validation");
    DirectVariantProjectionFixture {
        program,
        function: NodeId("function:project".into()),
        source_ty,
        result_ty,
        some,
        none,
        field,
        receipt,
    }
}

/// Canonical fixture for the consuming variant projection slice.  As with the
/// read-only fixture above, the source only asks the checker to materialize
/// the Option<string> TypeDesc; the consuming node is introduced directly in
/// canonical MIR so no surface projection syntax is implied.
#[derive(Debug, Clone)]
pub(crate) struct DirectVariantMoveProjectionFixture {
    pub(crate) program: MirProgram,
    pub(crate) function: NodeId,
    pub(crate) source_ty: ResolvedTypeId,
    pub(crate) result_ty: ResolvedTypeId,
    pub(crate) some: NodeId,
    pub(crate) none: NodeId,
    pub(crate) field: NodeId,
    pub(crate) receipt: MirVariantProjectionTrapContract,
}

pub(crate) fn direct_variant_move_projection_fixture() -> DirectVariantMoveProjectionFixture {
    let source = "func project(value: Option<string>) -> string {\n    ensures: true\n    \"seed\"\n}\nfunc main() -> i32 { 0 }";
    let tokens = Lexer::new(source)
        .tokenize()
        .expect("direct move projection lex");
    let file = Parser::new(tokens)
        .parse_file()
        .expect("direct move projection parse");
    let checked = crate::core::check_program(&file).expect("direct move projection check");
    let canonical = MirProgram::from_checked_program(&checked).expect("direct move projection MIR");
    let catalog = canonical.type_catalog().clone();
    let result_ty = catalog
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == MirTypeKind::Primitive(PrimitiveType::String)).then(|| id.clone())
        })
        .expect("string TypeDesc");
    let (source_ty, variants) = catalog
        .iter()
        .find_map(|(id, descriptor)| match &descriptor.layout {
            MirLayout::Option { inner, variants } if inner == &result_ty => {
                Some((id.clone(), variants.clone()))
            }
            _ => None,
        })
        .expect("Option<string> TypeDesc");
    let some = variants
        .iter()
        .find(|variant| variant.name == "Some")
        .expect("Option Some TypeDesc")
        .id
        .clone();
    let none = variants
        .iter()
        .find(|variant| variant.name == "None")
        .expect("Option None TypeDesc")
        .id
        .clone();
    let field = variants
        .iter()
        .find(|variant| variant.id == some)
        .and_then(|variant| variant.fields.first())
        .expect("Option Some payload TypeDesc")
        .id
        .clone();
    let receipt = catalog
        .validated_variant_move_projection_trap_contract(&source_ty, &some, &field, &result_ty)
        .expect("direct variant move projection receipt");
    let contracts = canonical
        .functions()
        .get(&NodeId("function:project".into()))
        .expect("project source function")
        .contracts
        .clone();

    let input = MirValueId::new("v.input").expect("input value id");
    let result = MirValueId::new("v.result").expect("result value id");
    let entry = MirBlockId::new("bb.entry").expect("entry block id");
    let mut functions = canonical.functions().clone();
    functions.insert(
        NodeId("function:project".into()),
        MirFunction {
            owner: NodeId("function:project".into()),
            parameters: vec![input.clone()],
            result: result_ty.clone(),
            entry: entry.clone(),
            values: BTreeMap::from([
                (
                    input.clone(),
                    MirValue {
                        id: input.clone(),
                        ty: source_ty.clone(),
                    },
                ),
                (
                    result.clone(),
                    MirValue {
                        id: result.clone(),
                        ty: result_ty.clone(),
                    },
                ),
            ]),
            blocks: BTreeMap::from([(
                entry.clone(),
                MirBlock {
                    id: entry,
                    parameters: Vec::new(),
                    instructions: vec![MirInstruction {
                        id: MirInstructionId::new("i.variant-project-move")
                            .expect("variant move projection instruction id"),
                        kind: MirInstructionKind::VariantProjectMove {
                            result: result.clone(),
                            base: input,
                            contract: Some(receipt.clone()),
                        },
                    }],
                    terminator: MirTerminator::Return {
                        value: Some(result),
                    },
                },
            )]),
            contracts,
            ownership: MirOwnershipSummary::default(),
        },
    );
    let program = MirProgram::with_type_catalog(functions, catalog)
        .expect("direct variant move projection program validation");
    DirectVariantMoveProjectionFixture {
        program,
        function: NodeId("function:project".into()),
        source_ty,
        result_ty,
        some,
        none,
        field,
        receipt,
    }
}

/// Canonical fixture for full-consumption record projection.  A `Pair` has two
/// owned String fields, so moving `left` is only valid when the residual `right`
/// field is explicitly dropped by the MIR node's TypeDesc receipt.
#[derive(Debug, Clone)]
pub(crate) struct DirectRecordMoveDropFixture {
    pub(crate) program: MirProgram,
    pub(crate) function: NodeId,
    pub(crate) source_ty: ResolvedTypeId,
    pub(crate) result_ty: ResolvedTypeId,
    pub(crate) selected_field: NodeId,
    pub(crate) receipt: MirRecordMoveProjectionDropContract,
}

pub(crate) fn direct_record_move_drop_fixture() -> DirectRecordMoveDropFixture {
    let source = "type Pair { left: string, right: string }\nfunc project(value: Pair) -> string {\n    ensures: true\n    \"seed\"\n}\nfunc main() -> i32 { 0 }";
    let tokens = Lexer::new(source).tokenize().expect("record move/drop lex");
    let file = Parser::new(tokens)
        .parse_file()
        .expect("record move/drop parse");
    let checked = crate::core::check_program(&file).expect("record move/drop check");
    let canonical = MirProgram::from_checked_program(&checked).expect("record move/drop MIR");
    let catalog = canonical.type_catalog().clone();
    let result_ty = catalog
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == MirTypeKind::Primitive(PrimitiveType::String)).then(|| id.clone())
        })
        .expect("String TypeDesc");
    let (source_ty, selected_field) = catalog
        .iter()
        .find_map(|(id, descriptor)| {
            let MirLayout::Record { fields, .. } = &descriptor.layout else {
                return None;
            };
            let selected = fields
                .iter()
                .find(|field| field.name == "left" && field.ty == result_ty)?;
            (fields.len() == 2).then(|| (id.clone(), selected.id.clone()))
        })
        .expect("Pair record TypeDesc");
    let receipt = catalog
        .validated_record_move_projection_drop_contract(&source_ty, &selected_field, &result_ty)
        .expect("record move/drop projection receipt");
    let contracts = canonical
        .functions()
        .get(&NodeId("function:project".into()))
        .expect("project source function")
        .contracts
        .clone();

    let input = MirValueId::new("v.input").expect("input value id");
    let result = MirValueId::new("v.result").expect("result value id");
    let entry = MirBlockId::new("bb.entry").expect("entry block id");
    let mut functions = canonical.functions().clone();
    functions.insert(
        NodeId("function:project".into()),
        MirFunction {
            owner: NodeId("function:project".into()),
            parameters: vec![input.clone()],
            result: result_ty.clone(),
            entry: entry.clone(),
            values: BTreeMap::from([
                (
                    input.clone(),
                    MirValue {
                        id: input.clone(),
                        ty: source_ty.clone(),
                    },
                ),
                (
                    result.clone(),
                    MirValue {
                        id: result.clone(),
                        ty: result_ty.clone(),
                    },
                ),
            ]),
            blocks: BTreeMap::from([(
                entry.clone(),
                MirBlock {
                    id: entry,
                    parameters: Vec::new(),
                    instructions: vec![MirInstruction {
                        id: MirInstructionId::new("i.record-move-drop")
                            .expect("record move/drop instruction id"),
                        kind: MirInstructionKind::MoveProjectDrop {
                            result: result.clone(),
                            base: input,
                            projection: crate::core::mir::MirProjection::Field(
                                selected_field.clone(),
                            ),
                            contract: Some(receipt.clone()),
                        },
                    }],
                    terminator: MirTerminator::Return {
                        value: Some(result),
                    },
                },
            )]),
            contracts,
            ownership: MirOwnershipSummary::default(),
        },
    );
    let program = MirProgram::with_type_catalog(functions, catalog)
        .expect("record move/drop program validation");
    DirectRecordMoveDropFixture {
        program,
        function: NodeId("function:project".into()),
        source_ty,
        result_ty,
        selected_field,
        receipt,
    }
}

/// Canonical fixture for an aggregate user-enum `SwitchMove`.  The source
/// only asks the checker to materialize the enum schema; the switch itself is
/// assembled directly as canonical MIR so this slice cannot accidentally
/// widen surface constructor lowering or the default native route.
#[derive(Debug, Clone)]
pub(crate) struct DirectEnumSwitchMoveFixture {
    pub(crate) program: MirProgram,
    pub(crate) function: NodeId,
    pub(crate) source_ty: ResolvedTypeId,
    pub(crate) nominal: crate::core::ir::NominalTypeId,
    pub(crate) keep: NodeId,
}

pub(crate) fn direct_enum_switch_move_fixture() -> DirectEnumSwitchMoveFixture {
    let source = include_str!("../../../tests/fixtures/mir_custom_enum_residual_drop.mimi");
    let tokens = Lexer::new(source).tokenize().expect("enum switch-move lex");
    let file = Parser::new(tokens)
        .parse_file()
        .expect("enum switch-move parse");
    let checked = crate::core::check_program(&file).expect("enum switch-move check");
    let canonical = MirProgram::from_checked_program(&checked).expect("enum switch-move MIR");
    let catalog = canonical.type_catalog().clone();
    let result_ty = catalog
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == MirTypeKind::Primitive(PrimitiveType::String)).then(|| id.clone())
        })
        .expect("String TypeDesc");
    let (source_ty, nominal, variants) = catalog
        .iter()
        .find_map(|(id, descriptor)| match &descriptor.layout {
            MirLayout::Enum { nominal, variants } if nominal.as_str().ends_with("Choice") => {
                Some((id.clone(), nominal.clone(), variants.clone()))
            }
            _ => None,
        })
        .expect("Choice enum TypeDesc");
    let keep_desc = variants
        .iter()
        .find(|variant| variant.name == "Keep")
        .expect("Keep variant TypeDesc");
    let empty_desc = variants
        .iter()
        .find(|variant| variant.name == "Empty")
        .expect("Empty variant TypeDesc");
    let selected = keep_desc
        .fields
        .first()
        .expect("Keep first payload TypeDesc")
        .id
        .clone();
    let projection = catalog
        .validated_variant_payload_projection_contract(
            &source_ty,
            &keep_desc.id,
            &selected,
            &result_ty,
        )
        .expect("enum payload projection receipt");
    let contracts = canonical
        .functions()
        .get(&NodeId("function:take".into()))
        .expect("take source function")
        .contracts
        .clone();

    let input = MirValueId::new("v.input").expect("enum input value id");
    let kept = MirValueId::new("v.kept").expect("enum kept value id");
    let empty_result = MirValueId::new("v.empty").expect("enum empty result value id");
    let entry = MirBlockId::new("bb.entry").expect("enum entry block id");
    let keep_block = MirBlockId::new("bb.keep").expect("enum keep block id");
    let empty_block = MirBlockId::new("bb.empty").expect("enum empty block id");
    let keep_edge = crate::core::mir::MirEdgeId::new("e.keep").expect("enum keep edge id");
    let empty_edge = crate::core::mir::MirEdgeId::new("e.empty").expect("enum empty edge id");
    let mut functions = canonical.functions().clone();
    functions.insert(
        NodeId("function:take".into()),
        MirFunction {
            owner: NodeId("function:take".into()),
            parameters: vec![input.clone()],
            result: result_ty.clone(),
            entry: entry.clone(),
            values: BTreeMap::from([
                (
                    input.clone(),
                    MirValue {
                        id: input.clone(),
                        ty: source_ty.clone(),
                    },
                ),
                (
                    kept.clone(),
                    MirValue {
                        id: kept.clone(),
                        ty: result_ty.clone(),
                    },
                ),
                (
                    empty_result.clone(),
                    MirValue {
                        id: empty_result.clone(),
                        ty: result_ty.clone(),
                    },
                ),
            ]),
            blocks: BTreeMap::from([
                (
                    entry.clone(),
                    MirBlock {
                        id: entry,
                        parameters: Vec::new(),
                        instructions: Vec::new(),
                        terminator: MirTerminator::SwitchMove {
                            scrutinee: input,
                            arms: vec![
                                MirSwitchArm {
                                    edge: keep_edge,
                                    target: keep_block.clone(),
                                    arguments: Vec::new(),
                                    bindings: vec![MirSwitchBinding {
                                        parameter: kept.clone(),
                                        projection: projection.clone(),
                                    }],
                                    case: MirSwitchCase::Variant(keep_desc.id.clone()),
                                },
                                MirSwitchArm {
                                    edge: empty_edge,
                                    target: empty_block.clone(),
                                    arguments: Vec::new(),
                                    bindings: Vec::new(),
                                    case: MirSwitchCase::Variant(empty_desc.id.clone()),
                                },
                            ],
                        },
                    },
                ),
                (
                    keep_block.clone(),
                    MirBlock {
                        id: keep_block,
                        parameters: vec![crate::core::mir::MirBlockParameter {
                            value: kept.clone(),
                        }],
                        instructions: Vec::new(),
                        terminator: MirTerminator::Return { value: Some(kept) },
                    },
                ),
                (
                    empty_block.clone(),
                    MirBlock {
                        id: empty_block,
                        parameters: Vec::new(),
                        instructions: vec![MirInstruction {
                            id: MirInstructionId::new("i.empty-string")
                                .expect("enum empty-string instruction id"),
                            kind: MirInstructionKind::Const {
                                result: empty_result.clone(),
                                literal: crate::core::ir::ResolvedLiteral::String("empty".into()),
                            },
                        }],
                        terminator: MirTerminator::Return {
                            value: Some(empty_result),
                        },
                    },
                ),
            ]),
            contracts,
            ownership: MirOwnershipSummary::default(),
        },
    );
    let program = MirProgram::with_type_catalog(functions, catalog)
        .expect("enum switch-move program validation");
    DirectEnumSwitchMoveFixture {
        program,
        function: NodeId("function:take".into()),
        source_ty,
        nominal,
        keep: keep_desc.id.clone(),
    }
}

/// Canonical fixture for the narrow native flat-Copy user-enum ABI.  The
/// checker supplies the enum TypeDesc; the Switch body is assembled directly
/// as MIR so the test does not widen surface constructor lowering.
#[derive(Debug, Clone)]
pub(crate) struct DirectFlatCopyEnumSwitchFixture {
    pub(crate) program: MirProgram,
    pub(crate) function: NodeId,
    pub(crate) source_ty: ResolvedTypeId,
    pub(crate) nominal: crate::core::ir::NominalTypeId,
    pub(crate) number: NodeId,
}

pub(crate) fn direct_flat_copy_enum_switch_fixture() -> DirectFlatCopyEnumSwitchFixture {
    let source = include_str!("../../../tests/fixtures/mir_custom_enum_flat_copy.mimi");
    let tokens = Lexer::new(source).tokenize().expect("flat enum lex");
    let file = Parser::new(tokens).parse_file().expect("flat enum parse");
    let checked = crate::core::check_program(&file).expect("flat enum check");
    let canonical = MirProgram::from_checked_program(&checked).expect("flat enum MIR");
    let catalog = canonical.type_catalog().clone();
    let result_ty = catalog
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == MirTypeKind::Primitive(PrimitiveType::I32)).then(|| id.clone())
        })
        .expect("i32 TypeDesc");
    let (source_ty, nominal, variants) = catalog
        .iter()
        .find_map(|(id, descriptor)| match &descriptor.layout {
            MirLayout::Enum { nominal, variants } if nominal.as_str().ends_with("Signal") => {
                Some((id.clone(), nominal.clone(), variants.clone()))
            }
            _ => None,
        })
        .expect("Signal enum TypeDesc");
    let number_desc = variants
        .iter()
        .find(|variant| variant.name == "Number")
        .expect("Number variant TypeDesc");
    let empty_desc = variants
        .iter()
        .find(|variant| variant.name == "Empty")
        .expect("Empty variant TypeDesc");
    let number_field = number_desc
        .fields
        .first()
        .expect("Number payload TypeDesc")
        .id
        .clone();
    let projection = catalog
        .validated_variant_payload_projection_contract(
            &source_ty,
            &number_desc.id,
            &number_field,
            &result_ty,
        )
        .expect("flat enum payload projection receipt");
    let contracts = canonical
        .functions()
        .get(&NodeId("function:take_signal".into()))
        .expect("take_signal source function")
        .contracts
        .clone();

    let input = MirValueId::new("v.input").expect("flat enum input value id");
    let number = MirValueId::new("v.number").expect("flat enum number value id");
    let empty_result = MirValueId::new("v.empty").expect("flat enum empty value id");
    let entry = MirBlockId::new("bb.entry").expect("flat enum entry block id");
    let number_block = MirBlockId::new("bb.number").expect("flat enum number block id");
    let empty_block = MirBlockId::new("bb.empty").expect("flat enum empty block id");
    let number_edge = crate::core::mir::MirEdgeId::new("e.number").expect("flat enum number edge");
    let empty_edge = crate::core::mir::MirEdgeId::new("e.empty").expect("flat enum empty edge");
    let mut functions = canonical.functions().clone();
    functions.insert(
        NodeId("function:take_signal".into()),
        MirFunction {
            owner: NodeId("function:take_signal".into()),
            parameters: vec![input.clone()],
            result: result_ty.clone(),
            entry: entry.clone(),
            values: BTreeMap::from([
                (
                    input.clone(),
                    MirValue {
                        id: input.clone(),
                        ty: source_ty.clone(),
                    },
                ),
                (
                    number.clone(),
                    MirValue {
                        id: number.clone(),
                        ty: result_ty.clone(),
                    },
                ),
                (
                    empty_result.clone(),
                    MirValue {
                        id: empty_result.clone(),
                        ty: result_ty.clone(),
                    },
                ),
            ]),
            blocks: BTreeMap::from([
                (
                    entry.clone(),
                    MirBlock {
                        id: entry,
                        parameters: Vec::new(),
                        instructions: Vec::new(),
                        terminator: MirTerminator::Switch {
                            scrutinee: input,
                            arms: vec![
                                MirSwitchArm {
                                    edge: number_edge,
                                    target: number_block.clone(),
                                    arguments: Vec::new(),
                                    bindings: vec![MirSwitchBinding {
                                        parameter: number.clone(),
                                        projection: projection.clone(),
                                    }],
                                    case: MirSwitchCase::Variant(number_desc.id.clone()),
                                },
                                MirSwitchArm {
                                    edge: empty_edge,
                                    target: empty_block.clone(),
                                    arguments: Vec::new(),
                                    bindings: Vec::new(),
                                    case: MirSwitchCase::Variant(empty_desc.id.clone()),
                                },
                            ],
                        },
                    },
                ),
                (
                    number_block.clone(),
                    MirBlock {
                        id: number_block,
                        parameters: vec![crate::core::mir::MirBlockParameter {
                            value: number.clone(),
                        }],
                        instructions: Vec::new(),
                        terminator: MirTerminator::Return {
                            value: Some(number),
                        },
                    },
                ),
                (
                    empty_block.clone(),
                    MirBlock {
                        id: empty_block,
                        parameters: Vec::new(),
                        instructions: vec![MirInstruction {
                            id: MirInstructionId::new("i.empty-int")
                                .expect("flat enum empty instruction id"),
                            kind: MirInstructionKind::Const {
                                result: empty_result.clone(),
                                literal: crate::core::ir::ResolvedLiteral::Int(0),
                            },
                        }],
                        terminator: MirTerminator::Return {
                            value: Some(empty_result),
                        },
                    },
                ),
            ]),
            contracts,
            ownership: MirOwnershipSummary::default(),
        },
    );
    let program = MirProgram::with_type_catalog(functions, catalog)
        .expect("flat enum switch program validation");
    DirectFlatCopyEnumSwitchFixture {
        program,
        function: NodeId("function:take_signal".into()),
        source_ty,
        nominal,
        number: number_desc.id.clone(),
    }
}

/// Canonical fixture for flat Copy user-enum construction.  The function body
/// is assembled directly as MIR because the source fixture intentionally only
/// materializes the checker-owned enum declaration.
#[derive(Debug, Clone)]
pub(crate) struct DirectFlatCopyEnumConstructFixture {
    pub(crate) program: MirProgram,
    pub(crate) function: NodeId,
    pub(crate) source_ty: ResolvedTypeId,
    pub(crate) nominal: crate::core::ir::NominalTypeId,
    pub(crate) number: NodeId,
    pub(crate) number_field: NodeId,
}

pub(crate) fn direct_flat_copy_enum_construct_fixture() -> DirectFlatCopyEnumConstructFixture {
    let switch = direct_flat_copy_enum_switch_fixture();
    let catalog = switch.program.type_catalog().clone();
    let variants = match &catalog
        .get(&switch.source_ty)
        .expect("Signal TypeDesc")
        .layout
    {
        MirLayout::Enum { variants, .. } => variants.clone(),
        layout => panic!("Signal has unexpected layout {layout:?}"),
    };
    let number_desc = variants
        .iter()
        .find(|variant| variant.id == switch.number)
        .expect("Number variant TypeDesc");
    let number_field = number_desc
        .fields
        .first()
        .expect("Number payload TypeDesc")
        .id
        .clone();
    let owner = NodeId("function:construct_signal".into());
    let payload = MirValueId::new("v.payload").expect("construct payload value id");
    let result = MirValueId::new("v.result").expect("construct result value id");
    let entry = MirBlockId::new("bb.entry").expect("construct entry block id");
    let mut functions = switch.program.functions().clone();
    functions.insert(
        owner.clone(),
        MirFunction {
            owner: owner.clone(),
            parameters: Vec::new(),
            result: switch.source_ty.clone(),
            entry: entry.clone(),
            values: BTreeMap::from([
                (
                    payload.clone(),
                    MirValue {
                        id: payload.clone(),
                        ty: number_desc
                            .fields
                            .first()
                            .expect("Number payload field")
                            .ty
                            .clone(),
                    },
                ),
                (
                    result.clone(),
                    MirValue {
                        id: result.clone(),
                        ty: switch.source_ty.clone(),
                    },
                ),
            ]),
            blocks: BTreeMap::from([(
                entry.clone(),
                MirBlock {
                    id: entry,
                    parameters: Vec::new(),
                    instructions: vec![
                        MirInstruction {
                            id: MirInstructionId::new("i.payload")
                                .expect("construct payload instruction id"),
                            kind: MirInstructionKind::Const {
                                result: payload.clone(),
                                literal: crate::core::ir::ResolvedLiteral::Int(7),
                            },
                        },
                        MirInstruction {
                            id: MirInstructionId::new("i.construct")
                                .expect("construct variant instruction id"),
                            kind: MirInstructionKind::ConstructVariant {
                                result: result.clone(),
                                nominal: switch.nominal.clone(),
                                variant: switch.number.clone(),
                                fields: vec![(number_field.clone(), payload.clone())],
                            },
                        },
                    ],
                    terminator: MirTerminator::Return {
                        value: Some(result),
                    },
                },
            )]),
            contracts: Vec::new(),
            ownership: MirOwnershipSummary::default(),
        },
    );
    let program = MirProgram::with_type_catalog(functions, catalog)
        .expect("flat Copy user-enum construction MIR");
    DirectFlatCopyEnumConstructFixture {
        program,
        function: owner,
        source_ty: switch.source_ty,
        nominal: switch.nominal,
        number: switch.number,
        number_field,
    }
}
