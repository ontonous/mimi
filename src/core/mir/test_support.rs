use std::collections::BTreeMap;

use crate::core::ir::{PrimitiveType, ResolvedTypeId};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{
    MirLayout, MirRecordMoveProjectionDropContract, MirTypeKind, MirVariantProjectionTrapContract,
};
use crate::core::mir::{
    MirBlock, MirBlockId, MirFunction, MirInstruction, MirInstructionId, MirInstructionKind,
    MirOwnershipSummary, MirTerminator, MirValue, MirValueId,
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
