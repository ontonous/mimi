//! One closed production profile; the broad Map/Any readiness bit stays closed.
use super::reference::MirProgram;
use super::{
    MirInstruction, MirInstructionId, MirInstructionKind as I, MirListOperation, MirTerminator,
    MirValueId,
};
use crate::core::ir::ResolvedLiteral;
use crate::core::ownership::MapEmptyRootRootQueriesV1CheckerReceipt;
use crate::core::{CheckedProgram, NodeId};

pub const MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE: &str = "map-empty-root-root-queries-v1";

/// Immutable, independently rederived proof. Private fields prevent a caller
/// from turning a generic route digest into source authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapEmptyRootRootQueriesV1MirReceipt {
    checker: MapEmptyRootRootQueriesV1CheckerReceipt,
    instructions: Vec<MirInstruction>,
}

pub(crate) fn checker_receipt(
    checked: &CheckedProgram,
) -> Option<&MapEmptyRootRootQueriesV1CheckerReceipt> {
    // Keep the profile closed over exactly the declarations it proves.
    // Looking only at executable bodies would miss dormant externs, actors,
    // exported declarations, and other items that MIR lowering might not
    // materialize into `functions()`.
    if checked.has_imports()
        || !checked.actors().is_empty()
        || !checked.sessions().is_empty()
        || !checked.ffi_declarations().is_empty()
    {
        return None;
    }
    let source_items = checked
        .items()
        .values()
        .filter(|item| {
            !checked
                .source_registry()
                .key(item.origin.user_span().source_id)
                .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi")
        })
        .collect::<Vec<_>>();
    if source_items.len() != 4 {
        return None;
    }
    let main_items = source_items
        .iter()
        .filter(|item| {
            item.node_id.0 == "function:main"
                && item.qualified_name == "main"
                && item.kind == crate::core::ResolvedItemKind::Function
                && matches!(&item.origin, crate::core::Origin::User(_))
        })
        .count();
    if main_items != 1 {
        return None;
    }
    // The parser synthesizes one runtime `Main` Flow for the user's main
    // function, and the checker derives exactly its StateId/EventId nominal
    // types. Admit only those two generated type items with the matching
    // source span; dormant user declarations remain outside this closed set.
    let generated_main_flow = source_items.iter().find(|item| {
        item.kind == crate::core::ResolvedItemKind::Flow
            && item.node_id.0 == "flow:Main"
            && item.qualified_name == "Main"
            && matches!(
                &item.origin,
                crate::core::Origin::RuntimeSystem { parent, rule, .. }
                    if parent.0 == "function:main" && rule == "progressive.main"
            )
    });
    let generated_main_flow = generated_main_flow?;
    let generated_flow_span = generated_main_flow.origin.user_span();
    let generated_fault_nominal_count = |name: &str| {
        source_items
            .iter()
            .filter(|item| {
                item.kind == crate::core::ResolvedItemKind::Type
                    && item.node_id.0 == format!("type:flow::Main::{name}")
                    && item.qualified_name == format!("flow::Main::{name}")
                    && matches!(
                        &item.origin,
                        crate::core::Origin::Desugared { parent, rule, span }
                            if parent.0 == "compilation:root"
                                && rule == "flow_matrix.fault_nominal"
                                && *span == generated_flow_span
                    )
            })
            .count()
    };
    if generated_fault_nominal_count("EventId") != 1
        || generated_fault_nominal_count("StateId") != 1
    {
        return None;
    }
    let main_function = checked.functions().get(&NodeId("function:main".into()))?;
    if main_function.pub_
        || main_function.is_comptime
        || main_function.is_async
        || main_function.extern_abi.is_some()
        || !main_function.generics.is_empty()
        || !main_function.effects.is_empty()
    {
        return None;
    }

    let mut bodies = checked.resolved_bodies().values().filter(|body| {
        !checked
            .source_registry()
            .key(body.root.origin.user_span().source_id)
            .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi")
    });
    let body = bodies.next()?;
    if body.owner.0 != "function:main" || bodies.next().is_some() {
        return None;
    }
    checked
        .resource_analysis(&body.owner)?
        .ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()?
        .empty_root_root_queries_v1
        .as_ref()
}

pub fn validate_map_empty_root_root_queries_v1_route(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<MapEmptyRootRootQueriesV1MirReceipt, String> {
    let source = checker_receipt(checked)
        .ok_or("MapEmptyRootRootQueriesV1 has no exact whole-program Checker receipt")?;
    let proof = validate_map_empty_root_root_queries_v1_mir_route(program)?;
    if proof.checker != *source {
        return Err("MapEmptyRootRootQueriesV1 belongs to another Checker source".into());
    }
    Ok(proof)
}

/// Replays the complete ordered executable graph against private Checker
/// identities. A freshly computed public MIR digest never replaces this proof.
pub fn validate_map_empty_root_root_queries_v1_mir_route(
    program: &MirProgram,
) -> Result<MapEmptyRootRootQueriesV1MirReceipt, String> {
    program.validate_proof_source_census().map_err(|error| {
        format!("MapEmptyRootRootQueriesV1 source census is invalid: {error:?}")
    })?;
    if !program
        .proof_source_census()
        .admits_map_empty_root_root_queries_v1_source()
    {
        return Err(
            "MapEmptyRootRootQueriesV1 requires an exact CheckedProgram declaration census".into(),
        );
    }
    if program.functions().len() != 1
        || program.purpose() != super::reference::MirProgramPurpose::Executable
        || !program.transitions().is_empty()
        || !program.ffi_calls().is_empty()
        || !program.instances().is_empty()
        || !program.actors().is_empty()
        || !program.export_declarations().is_empty()
    {
        return Err(
            "MapEmptyRootRootQueriesV1 requires only main and no transitions, instances, actors, exports, or FFI"
                .into(),
        );
    }
    let function = program
        .functions()
        .get(&NodeId("function:main".into()))
        .ok_or("MapEmptyRootRootQueriesV1 requires main")?;
    function
        .validate()
        .map_err(|e| format!("MapEmptyRootRootQueriesV1 invalid MIR: {e:?}"))?;
    if function.blocks.len() != 1
        || !function.parameters.is_empty()
        || !function.contracts.is_empty()
    {
        return Err(
            "MapEmptyRootRootQueriesV1 requires a parameterless, contract-free straight-line main"
                .into(),
        );
    }
    let cfg = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .ok_or("missing Checker V2 CFG receipt")?;
    let c = cfg
        .empty_root_root_queries_v1
        .as_ref()
        .ok_or("missing private MapEmptyRootRootQueriesV1 Checker receipt")?;
    if c.owner != function.owner
        || c.actions != cfg.actions
        || c.attempts != cfg.attempts
        || cfg.actions.len() != 4
        || cfg.attempts.len() != 4
    {
        return Err("MapEmptyRootRootQueriesV1 Checker census changed".into());
    }
    let b = &c.bindings;
    let local =
        |i: usize| MirValueId::new(format!("local:{}", b[i].local.0 .0)).map_err(|e| e.to_string());
    let expr =
        |node: &NodeId| MirValueId::new(format!("expr:{}", node.0)).map_err(|e| e.to_string());
    let root = local(0)?;
    let pair = expr(&b[1].operation)?;
    let snapshot = expr(&b[2].operation)?;
    let count = expr(&b[3].operation)?;
    let key = expr(&c.key)?;
    let list = expr(b[3].arguments.first().ok_or("missing len source")?)?;
    let ret = expr(&c.return_value)?;
    let block = function
        .blocks
        .get(&function.entry)
        .ok_or("missing main entry")?;
    if block.instructions.len() != 16
        || block.terminator
            != (MirTerminator::Return {
                value: Some(ret.clone()),
            })
    {
        return Err("MapEmptyRootRootQueriesV1 requires its exact sixteen instructions and literal-zero return".into());
    }
    let contract = match &block.instructions[8].kind {
        I::ListOp {
            result,
            operation: MirListOperation::Len,
            list: source,
            argument: None,
            list_operation_contract: Some(contract),
        } if result == &count && source == &list => Some(contract.clone()),
        _ => {
            return Err(
                "MapEmptyRootRootQueriesV1 len does not consume its exact Values snapshot".into(),
            );
        }
    };
    let mut expected = Vec::new();
    let mut push = |role: &str, node: &NodeId, kind| -> Result<(), String> {
        expected.push(MirInstruction {
            id: MirInstructionId::new(format!("inst:{role}:{}", node.0))
                .map_err(|e| e.to_string())?,
            kind,
        });
        Ok(())
    };
    push(
        &format!("ordinary_map_owned_any_v2_new:{}", root.as_str()),
        &b[0].operation,
        I::OrdinaryMapOwnedAnyV2New {
            result: root.clone(),
        },
    )?;
    push(
        "const",
        &c.key,
        I::Const {
            result: key.clone(),
            literal: ResolvedLiteral::String(c.key_text.clone()),
        },
    )?;
    push(
        &format!("ordinary_map_owned_any_v2_get_any:{}", root.as_str()),
        &b[1].operation,
        I::OrdinaryMapOwnedAnyV2GetAny {
            result: pair.clone(),
            root: root.clone(),
            key: key.clone(),
        },
    )?;
    push(
        "drop.ordinary-map-key",
        &b[1].operation,
        I::Drop { value: key },
    )?;
    push(
        "bind",
        &b[1].statement,
        I::Move {
            result: local(1)?,
            source: pair,
        },
    )?;
    push(
        &format!("ordinary_map_owned_any_v2_values:{}", root.as_str()),
        &b[2].operation,
        I::OrdinaryMapOwnedAnyV2Values {
            result: snapshot.clone(),
            root: root.clone(),
        },
    )?;
    push(
        "bind",
        &b[2].statement,
        I::Move {
            result: local(2)?,
            source: snapshot,
        },
    )?;
    push(
        "clone",
        &b[3].arguments[0],
        I::Clone {
            result: list.clone(),
            source: local(2)?,
        },
    )?;
    push(
        "list_op",
        &b[3].operation,
        I::ListOp {
            result: count.clone(),
            operation: MirListOperation::Len,
            list: list.clone(),
            argument: None,
            list_operation_contract: contract,
        },
    )?;
    push("drop.list-borrow", &b[3].operation, I::Drop { value: list })?;
    push(
        "bind",
        &b[3].statement,
        I::Move {
            result: local(3)?,
            source: count,
        },
    )?;
    for (drop_index, binding_index) in [1usize, 3, 2].into_iter().enumerate() {
        push(
            "drop.0",
            &c.drops[drop_index],
            I::Drop {
                value: local(binding_index)?,
            },
        )?;
    }
    push(
        &format!("ordinary_map_owned_any_v2_drop:{}", root.as_str()),
        &c.drops[3],
        I::OrdinaryMapOwnedAnyV2Drop { root },
    )?;
    push(
        "const",
        &c.return_value,
        I::Const {
            result: ret,
            literal: ResolvedLiteral::Int(0),
        },
    )?;
    if block.instructions != expected {
        return Err(
            "MapEmptyRootRootQueriesV1 ordered source/MIR instruction census differs".into(),
        );
    }
    let selection = program
        .ordinary_map_owned_any_v2_payload_selection_receipt()?
        .ok_or("missing empty payload selection")?;
    selection.validate_against_program(program)?;
    if !selection.selected_types.is_empty()
        || program
            .ordinary_map_owned_any_v2_get_any_result_receipts()?
            .len()
            != 1
        || program
            .ordinary_map_owned_any_v2_values_result_receipts()?
            .len()
            != 1
    {
        return Err("MapEmptyRootRootQueriesV1 requires one absent Get, one empty Values and explicit result settlement".into());
    }
    Ok(MapEmptyRootRootQueriesV1MirReceipt {
        checker: c.clone(),
        instructions: expected,
    })
}
