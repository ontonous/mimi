//! Exact Checker source/CFG authority for one fallible ordinary Map owner
//! lineage ending in generic GetAny and a checked i32 unpack.
//!
//! This is intentionally independent of both `new_result` and the direct
//! `map_get_i32` lineage profile. It binds the New and Set Result matches,
//! the full generic Get pair and its two projections, one `unpack_i32`, and
//! every owner settlement point in the one admitted source/CFG shape.

use super::*;
use crate::core::cfg::{BasicBlockId, CfgPointKind, EdgeId};
use crate::core::ir::{CheckedConversionKind, PrimitiveType, ResolvedProjection};
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2Attempt,
    OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
    OrdinaryMapOwnedAnyV2SetGetAnyI32LineageProfileVersion as Version,
    OrdinaryMapOwnedAnyV2SetGetAnyI32LineageReceipt as Receipt,
};
use crate::core::{LocalId, Place, ResourceId};

fn local_name(body: &ResolvedBody, local: &ResolvedLocalId) -> String {
    body.locals
        .get(local)
        .map(|binding| binding.display_name.clone())
        .unwrap_or_else(|| local.0 .0.clone())
}

fn root_place(body: &ResolvedBody, local: &ResolvedLocalId) -> Place {
    Place::root(LocalId(local.0.clone()), local_name(body, local))
}

fn resource_identity(local: &ResolvedLocalId) -> ResourceId {
    ResourceId(NodeId(format!(
        "ordinary-map-owned-any-v2-set-get-any-i32-lineage-v1:{}",
        local.0 .0
    )))
}

fn immutable_binding<'a>(
    statement: &'a ResolvedStmt,
    body: &ResolvedBody,
) -> Option<(&'a ResolvedLocalId, &'a ResolvedExpr)> {
    let ResolvedStmtKind::Bind {
        pattern,
        initializer: Some(expression),
    } = &statement.kind
    else {
        return None;
    };
    let ResolvedPatternKind::Binding {
        local,
        by_reference: None,
    } = &pattern.kind
    else {
        return None;
    };
    let binding = body.locals.get(local)?;
    (!binding.mutable && binding.ty == pattern.ty && binding.ty == expression.ty)
        .then_some((local, expression))
}

fn builtin_call<'a>(
    expression: &'a ResolvedExpr,
    name: &str,
    argument_count: usize,
) -> Option<&'a ResolvedCall> {
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return None;
    };
    (matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == name)
        && call.arguments.len() == argument_count
        && call.result == expression.ty
        && call.type_arguments.is_empty()
        && call.permission.is_none()
        && call.session.is_empty())
    .then_some(call)
}

fn load(expression: &ResolvedExpr, local: &ResolvedLocalId, ty: &ResolvedTypeId) -> bool {
    expression.ty == *ty
        && matches!(&expression.kind, ResolvedExprKind::Load(place)
            if place.base == *local && place.projections.is_empty())
}

fn tuple_projection(
    expression: &ResolvedExpr,
    local: &ResolvedLocalId,
    index: usize,
    ty: &ResolvedTypeId,
) -> bool {
    expression.ty == *ty
        && matches!(&expression.kind, ResolvedExprKind::Load(place)
            if place.base == *local
                && matches!(place.projections.as_slice(), [ResolvedProjection::Tuple {
                    index: actual_index,
                    ty: actual_type,
                }] if *actual_index == index && actual_type == ty))
}

fn variant_arm<'a>(arms: &'a [MatchArm], name: &str) -> Option<&'a MatchArm> {
    let expected = format!("builtin:variant:Result::{name}");
    let mut found = arms.iter().filter(|arm| {
        matches!(&arm.pattern.kind, ResolvedPatternKind::Constructor { variant, .. }
            if variant.0 == expected)
    });
    let arm = found.next()?;
    found.next().is_none().then_some(arm)
}

fn payload<'a>(
    arm: &'a MatchArm,
    variant_name: &str,
    result_type: &ResolvedTypeId,
    payload_type: &ResolvedTypeId,
    body: &ResolvedBody,
) -> Option<(&'a ResolvedLocalId, &'a ResolvedPattern)> {
    let ResolvedPatternKind::Constructor { variant, fields } = &arm.pattern.kind else {
        return None;
    };
    let [(field, pattern)] = fields.as_slice() else {
        return None;
    };
    let ResolvedPatternKind::Binding {
        local,
        by_reference: None,
    } = &pattern.kind
    else {
        return None;
    };
    let binding = body.locals.get(local)?;
    (variant.0 == format!("builtin:variant:Result::{variant_name}")
        && field.0 == format!("builtin:variant:Result::{variant_name}/payload:0")
        && arm.pattern.ty == *result_type
        && pattern.ty == *payload_type
        && binding.ty == *payload_type
        && !binding.mutable)
        .then_some((local, pattern))
}

fn result_parts<'a>(
    types: &'a ResolvedTypeTable,
    ty: &'a ResolvedTypeId,
) -> Option<(&'a ResolvedTypeId, &'a ResolvedTypeId)> {
    match types.get(ty)? {
        ResolvedType::Result { ok, error } => Some((ok, error)),
        ResolvedType::Nominal {
            item, arguments, ..
        } if item.as_str() == "builtin:type:Result" && arguments.len() == 2 => {
            Some((&arguments[0], &arguments[1]))
        }
        _ => None,
    }
}

fn record_type(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(types.get(ty), Some(ResolvedType::Nominal {
        item, arguments, is_linear: false,
    }) if item.as_str() == "builtin:type:Record" && arguments.is_empty())
}

fn drop_local(statement: &ResolvedStmt, local: &ResolvedLocalId) -> bool {
    matches!(&statement.kind, ResolvedStmtKind::Drop(places)
        if places.len() == 1
            && places[0].base == *local
            && places[0].projections.is_empty())
}

fn direct_arm_result(arm: &MatchArm) -> Option<&ResolvedExpr> {
    match &arm.body.kind {
        ResolvedExprKind::Block(block) => block.result.as_deref(),
        _ => Some(&arm.body),
    }
}

fn edge_for_arm<'a>(
    cfg: &'a CallableCfg,
    dispatch: &BasicBlockId,
    arm: &MatchArm,
) -> Option<(&'a EdgeId, &'a BasicBlockId)> {
    let Terminator::Match { arms, .. } = &cfg.blocks.get(dispatch)?.terminator else {
        return None;
    };
    let mut found = arms.iter().filter_map(|edge_id| {
        let edge = cfg.edges.get(edge_id)?;
        (edge.from == *dispatch
            && edge.kind == EdgeKind::MatchArm
            && edge.source.node == arm.node_id)
            .then_some((edge_id, &edge.to))
    });
    let edge = found.next()?;
    found.next().is_none().then_some(edge)
}

fn goto_edge<'a>(
    cfg: &'a CallableCfg,
    block: &BasicBlockId,
) -> Option<(&'a EdgeId, &'a BasicBlockId)> {
    let Terminator::Goto { edge } = &cfg.blocks.get(block)?.terminator else {
        return None;
    };
    let edge = cfg.edges.get(edge)?;
    (edge.from == *block && edge.kind == EdgeKind::Fallthrough).then_some((&edge.id, &edge.to))
}

fn branch_edge<'a>(
    cfg: &'a CallableCfg,
    edge_id: &EdgeId,
    source: &BasicBlockId,
    kind: EdgeKind,
) -> Option<&'a crate::core::cfg::CfgEdge> {
    let edge = cfg.edges.get(edge_id)?;
    (edge.from == *source && edge.kind == kind).then_some(edge)
}

fn has_point(cfg: &CallableCfg, block: &BasicBlockId, node: &NodeId, kind: CfgPointKind) -> bool {
    let Some(block) = cfg.blocks.get(block) else {
        return false;
    };
    let mut found = block
        .points
        .iter()
        .filter(|point| point.source.node == *node);
    let Some(point) = found.next() else {
        return false;
    };
    point.kind == kind && found.next().is_none()
}

fn has_local_load_point(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    block: &BasicBlockId,
    expression: &ResolvedExpr,
    local: &ResolvedLocalId,
    kind: CfgPointKind,
) -> bool {
    let Some(basic_block) = cfg.blocks.get(block) else {
        return false;
    };
    let expected_place = root_place(body, local);
    let mut found = basic_block
        .points
        .iter()
        .filter(|point| point.source.node == expression.node_id);
    let Some(point) = found.next() else {
        return false;
    };
    point.kind == kind
        && point.uses == [expected_place.base_name.clone()]
        && point.defs.is_empty()
        && point.read_places == [expected_place.clone()]
        && point.write_places.is_empty()
        && found.next().is_none()
}

fn has_drop_point(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    block: &BasicBlockId,
    statement: &ResolvedStmt,
    local: &ResolvedLocalId,
) -> bool {
    cfg.blocks.get(block).is_some_and(|basic_block| {
        let mut points = basic_block
            .points
            .iter()
            .filter(|point| point.source.node == statement.node_id);
        points.next().is_some_and(|point| {
            point.kind == CfgPointKind::ResourceAction
                && point.read_places == [root_place(body, local)]
                && point.write_places.is_empty()
                && points.next().is_none()
        })
    })
}

pub(super) fn collect(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
    operations: &BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
) -> Option<Receipt> {
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    if cfg.owner != body.owner
        || signature.owner != body.owner
        || signature.result != i32_type
        || body.root.ty != i32_type
        || !signature.generic_parameters.is_empty()
        || !signature.parameters.is_empty()
        || !body.parameters.is_empty()
        || !body.captures.is_empty()
        || !body.place_inputs.is_empty()
        || !body.default_values.is_empty()
    {
        return None;
    }

    let mut executable = Vec::new();
    let mut executable_started = false;
    for statement in &body.root.statements {
        match &statement.kind {
            ResolvedStmtKind::Contract { kind, .. } if !executable_started => {
                if !matches!(
                    kind,
                    crate::core::ir::ContractKind::Requires
                        | crate::core::ir::ContractKind::Ensures
                ) {
                    return None;
                }
            }
            ResolvedStmtKind::Contract { .. } => return None,
            _ => {
                executable_started = true;
                executable.push(statement);
            }
        }
    }
    let [new_binding] = executable.as_slice() else {
        return None;
    };
    let (new_result_local, new_expression) = immutable_binding(new_binding, body)?;
    builtin_call(new_expression, "map_new", 0)?;
    let (initial_root_type, new_error_type) = result_parts(types, &new_expression.ty)?;
    if new_error_type != &i32_type || !record_type(types, initial_root_type) {
        return None;
    }
    let outer_match = body.root.result.as_deref()?;
    let ResolvedExprKind::Match {
        scrutinee: outer_scrutinee,
        arms: outer_arms,
    } = &outer_match.kind
    else {
        return None;
    };
    if outer_match.ty != i32_type
        || !load(outer_scrutinee, new_result_local, &new_expression.ty)
        || outer_arms.len() != 2
        || outer_arms
            .iter()
            .any(|arm| arm.guard.is_some() || arm.body.ty != i32_type)
    {
        return None;
    }
    let outer_ok_arm = variant_arm(outer_arms, "Ok")?;
    let outer_err_arm = variant_arm(outer_arms, "Err")?;
    let (initial_root_local, initial_root_pattern) = payload(
        outer_ok_arm,
        "Ok",
        &new_expression.ty,
        initial_root_type,
        body,
    )?;
    let (outer_status_local, outer_status_pattern) =
        payload(outer_err_arm, "Err", &new_expression.ty, &i32_type, body)?;
    if BTreeSet::from([
        new_result_local.clone(),
        initial_root_local.clone(),
        outer_status_local.clone(),
    ])
    .len()
        != 3
    {
        return None;
    }

    let ResolvedExprKind::Block(outer_ok_body) = &outer_ok_arm.body.kind else {
        return None;
    };
    let [set_binding] = outer_ok_body.statements.as_slice() else {
        return None;
    };
    let (set_result_local, set_expression) = immutable_binding(set_binding, body)?;
    let set_call = builtin_call(set_expression, "map_set", 3)?;
    let [set_receiver, set_key, set_value] = set_call.arguments.as_slice() else {
        return None;
    };
    if !load(&set_receiver.value, initial_root_local, initial_root_type)
        || set_receiver.conversion.kind != CheckedConversionKind::Identity
        || set_receiver.conversion.from != *initial_root_type
        || set_receiver.conversion.to != *initial_root_type
        || !matches!(
            &set_key.value.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::String(_))
        )
        || set_key.value.ty != string_type
        || set_key.conversion.kind != CheckedConversionKind::Identity
        || set_key.conversion.from != string_type
        || set_key.conversion.to != string_type
        || !matches!(
            &set_value.value.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(_))
        )
        || set_value.value.ty != i32_type
        || set_value.conversion.kind != CheckedConversionKind::Identity
        || set_value.conversion.from != i32_type
        || set_value.conversion.to != i32_type
    {
        return None;
    }
    let ResolvedExprKind::Literal(ResolvedLiteral::String(set_key_value)) = &set_key.value.kind
    else {
        return None;
    };
    let ResolvedExprKind::Literal(ResolvedLiteral::Int(set_i32_value)) = &set_value.value.kind
    else {
        return None;
    };
    let (updated_root_type, set_error_type) = result_parts(types, &set_expression.ty)?;
    if set_error_type != &i32_type || updated_root_type != initial_root_type {
        return None;
    }
    let set_match = outer_ok_body.result.as_deref()?;
    let ResolvedExprKind::Match {
        scrutinee: set_scrutinee,
        arms: set_arms,
    } = &set_match.kind
    else {
        return None;
    };
    if set_match.ty != i32_type
        || !load(set_scrutinee, set_result_local, &set_expression.ty)
        || set_arms.len() != 2
        || set_arms
            .iter()
            .any(|arm| arm.guard.is_some() || arm.body.ty != i32_type)
    {
        return None;
    }
    let set_ok_arm = variant_arm(set_arms, "Ok")?;
    let set_err_arm = variant_arm(set_arms, "Err")?;
    let (updated_root_local, updated_root_pattern) = payload(
        set_ok_arm,
        "Ok",
        &set_expression.ty,
        updated_root_type,
        body,
    )?;
    let (set_error_local, set_error_pattern) =
        payload(set_err_arm, "Err", &set_expression.ty, &i32_type, body)?;
    if updated_root_local == initial_root_local
        || updated_root_local == set_error_local
        || BTreeSet::from([
            set_result_local.clone(),
            updated_root_local.clone(),
            set_error_local.clone(),
        ])
        .len()
            != 3
    {
        return None;
    }

    let ResolvedExprKind::Block(set_ok_body) = &set_ok_arm.body.kind else {
        return None;
    };
    let [old_root_set_ok_drop, get_binding, found_binding, unpack_binding, pair_drop, updated_root_get_drop] =
        set_ok_body.statements.as_slice()
    else {
        return None;
    };
    if !drop_local(old_root_set_ok_drop, initial_root_local)
        || !drop_local(pair_drop, immutable_binding(get_binding, body)?.0)
        || !drop_local(updated_root_get_drop, updated_root_local)
    {
        return None;
    }
    let (get_result_local, get_expression) = immutable_binding(get_binding, body)?;
    let get_call = builtin_call(get_expression, "map_get", 2)?;
    let [get_receiver, get_key] = get_call.arguments.as_slice() else {
        return None;
    };
    if !load(&get_receiver.value, updated_root_local, updated_root_type)
        || get_receiver.conversion.kind != CheckedConversionKind::Identity
        || get_receiver.conversion.from != *updated_root_type
        || get_receiver.conversion.to != *updated_root_type
        || !matches!(
            &get_key.value.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::String(_))
        )
        || get_key.value.ty != string_type
        || get_key.conversion.kind != CheckedConversionKind::Identity
        || get_key.conversion.from != string_type
        || get_key.conversion.to != string_type
    {
        return None;
    }
    let ResolvedExprKind::Literal(ResolvedLiteral::String(get_key_value)) = &get_key.value.kind
    else {
        return None;
    };
    let ResolvedType::Tuple(tuple_types) = types.get(&get_expression.ty)? else {
        return None;
    };
    let [found_type, any_type] = tuple_types.as_slice() else {
        return None;
    };
    if found_type != &bool_type
        || !matches!(types.get(any_type), Some(ResolvedType::DynamicAny { .. }))
        || body
            .locals
            .get(get_result_local)
            .is_none_or(|binding| binding.ty != get_expression.ty || binding.mutable)
    {
        return None;
    }

    let (found_local, found_expression) = immutable_binding(found_binding, body)?;
    if !tuple_projection(found_expression, get_result_local, 0, &bool_type)
        || body
            .locals
            .get(found_local)
            .is_none_or(|binding| binding.ty != bool_type || binding.mutable)
    {
        return None;
    }
    let (unpack_result_local, unpack_expression) = immutable_binding(unpack_binding, body)?;
    let ResolvedExprKind::Cast {
        value: unpack_argument,
        conversion: unpack_conversion,
    } = &unpack_expression.kind
    else {
        return None;
    };
    if unpack_expression.ty != i32_type
        || unpack_conversion.kind != CheckedConversionKind::DynamicAnyUnpack
        || unpack_conversion.from != *any_type
        || unpack_conversion.to != i32_type
        || !tuple_projection(unpack_argument, get_result_local, 1, any_type)
        || unpack_argument.ty != *any_type
        || body
            .locals
            .get(unpack_result_local)
            .is_none_or(|binding| binding.ty != i32_type || binding.mutable)
    {
        return None;
    }
    if BTreeSet::from([
        get_result_local.clone(),
        found_local.clone(),
        unpack_result_local.clone(),
    ])
    .len()
        != 3
    {
        return None;
    }

    let found_result = set_ok_body.result.as_deref()?;
    let ResolvedExprKind::If {
        condition,
        then_block,
        else_block,
    } = &found_result.kind
    else {
        return None;
    };
    if !matches!(
        &else_block.result.as_deref()?.kind,
        ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
    ) {
        return None;
    }
    if found_result.ty != i32_type
        || then_block.ty != i32_type
        || else_block.ty != i32_type
        || !then_block.statements.is_empty()
        || !else_block.statements.is_empty()
        || !load(condition, found_local, &bool_type)
        || !load(
            then_block.result.as_deref()?,
            unpack_result_local,
            &i32_type,
        )
    {
        return None;
    }

    let ResolvedExprKind::Block(set_err_body) = &set_err_arm.body.kind else {
        return None;
    };
    let [old_root_set_err_drop] = set_err_body.statements.as_slice() else {
        return None;
    };
    if !drop_local(old_root_set_err_drop, initial_root_local)
        || !load(direct_arm_result(set_err_arm)?, set_error_local, &i32_type)
    {
        return None;
    }
    if matches!(&outer_err_arm.body.kind, ResolvedExprKind::Block(block)
        if !block.statements.is_empty())
    {
        return None;
    }
    if !load(
        direct_arm_result(outer_err_arm)?,
        outer_status_local,
        &i32_type,
    ) {
        return None;
    }

    // Bind the whole callable-local owner/operation census. These are the
    // only three Map builtin attempts in this profile; in particular a
    // second New/Set/Get cannot hide behind a subset receipt.
    let expected_operations = BTreeSet::from([
        OrdinaryMapOwnedAnyV2Attempt {
            kind: AttemptKind::New,
            point: new_expression.node_id.clone(),
            arguments: Vec::new(),
            receiver_local: None,
            result_local: Some(new_result_local.clone()),
        },
        OrdinaryMapOwnedAnyV2Attempt {
            kind: AttemptKind::Set,
            point: set_expression.node_id.clone(),
            arguments: set_call
                .arguments
                .iter()
                .map(|argument| argument.value.node_id.clone())
                .collect(),
            receiver_local: Some(initial_root_local.clone()),
            result_local: Some(set_result_local.clone()),
        },
        OrdinaryMapOwnedAnyV2Attempt {
            kind: AttemptKind::GetAny,
            point: get_expression.node_id.clone(),
            arguments: get_call
                .arguments
                .iter()
                .map(|argument| argument.value.node_id.clone())
                .collect(),
            receiver_local: Some(updated_root_local.clone()),
            result_local: Some(get_result_local.clone()),
        },
    ]);
    if operations != &expected_operations {
        return None;
    }

    let mut expected_locals = BTreeSet::from([
        new_result_local.clone(),
        initial_root_local.clone(),
        outer_status_local.clone(),
        set_result_local.clone(),
        updated_root_local.clone(),
        set_error_local.clone(),
        get_result_local.clone(),
        found_local.clone(),
        unpack_result_local.clone(),
    ]);
    let contract_result =
        ResolvedLocalId(NodeId(format!("{}/contract-result/local", body.owner.0)));
    let has_ensures = body.root.statements.iter().any(|statement| {
        matches!(
            &statement.kind,
            ResolvedStmtKind::Contract {
                kind: crate::core::ir::ContractKind::Ensures,
                ..
            }
        )
    });
    if has_ensures {
        let result_local = body.locals.get(&contract_result)?;
        if result_local.ty != i32_type || result_local.mutable {
            return None;
        }
        expected_locals.insert(contract_result);
    } else if body.locals.contains_key(&contract_result) {
        return None;
    }
    if expected_locals.len() != 9 + usize::from(has_ensures)
        || body.locals.keys().cloned().collect::<BTreeSet<_>>() != expected_locals
        || cfg.reachable.len() != cfg.blocks.len()
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != cfg.reachable
        || crate::core::cfg::validate_cfg(cfg).is_err()
    {
        return None;
    }

    // Resolve every branch by its Checker-owned source node and arm identity.
    // The expected graph has the outer New Result match, nested Set Result
    // match, and a final found/miss `if`, with no additional CFG blocks.
    let mut outer_dispatches = cfg.blocks.iter().filter_map(|(block, basic_block)| {
        matches!(
            &basic_block.terminator,
            Terminator::Match { scrutinee, .. } if scrutinee == &outer_scrutinee.node_id
        )
        .then_some(block.clone())
    });
    let outer_dispatch = outer_dispatches.next()?;
    if outer_dispatches.next().is_some() {
        return None;
    }
    let Terminator::Match {
        scrutinee: cfg_outer_scrutinee,
        ..
    } = &cfg.blocks.get(&outer_dispatch)?.terminator
    else {
        return None;
    };
    if cfg_outer_scrutinee != &outer_scrutinee.node_id {
        return None;
    }
    let (outer_ok_edge, outer_ok_block) = edge_for_arm(cfg, &outer_dispatch, outer_ok_arm)?;
    let (outer_err_edge, outer_err_block) = edge_for_arm(cfg, &outer_dispatch, outer_err_arm)?;
    if outer_ok_block == outer_err_block {
        return None;
    }
    let set_dispatch = outer_ok_block;
    let Terminator::Match {
        scrutinee: cfg_set_scrutinee,
        ..
    } = &cfg.blocks.get(set_dispatch)?.terminator
    else {
        return None;
    };
    if cfg_set_scrutinee != &set_scrutinee.node_id {
        return None;
    }
    let (set_ok_edge, set_ok_block) = edge_for_arm(cfg, set_dispatch, set_ok_arm)?;
    let (set_err_edge, set_err_block) = edge_for_arm(cfg, set_dispatch, set_err_arm)?;
    if set_ok_block == set_err_block {
        return None;
    }
    let found_dispatch = set_ok_block;
    let Terminator::Branch {
        condition: cfg_found_condition,
        then_edge: found_true_edge_id,
        else_edge: found_false_edge_id,
    } = &cfg.blocks.get(found_dispatch)?.terminator
    else {
        return None;
    };
    if cfg_found_condition != &condition.node_id {
        return None;
    }
    let found_true_edge = branch_edge(cfg, found_true_edge_id, found_dispatch, EdgeKind::Then)?;
    let found_false_edge = branch_edge(cfg, found_false_edge_id, found_dispatch, EdgeKind::Else)?;
    let found_true_block = found_true_edge.to.clone();
    let found_false_block = found_false_edge.to.clone();
    if found_true_block == found_false_block {
        return None;
    }
    let (found_true_join_edge, found_join) = goto_edge(cfg, &found_true_block)?;
    let (found_false_join_edge, found_false_join) = goto_edge(cfg, &found_false_block)?;
    if found_join != found_false_join {
        return None;
    }
    let (set_err_join_edge, set_join_from_err) = goto_edge(cfg, set_err_block)?;
    let (found_to_set_join_edge, set_join) = goto_edge(cfg, found_join)?;
    if set_join_from_err != set_join || set_join == found_join {
        return None;
    }
    let (set_to_outer_join_edge, outer_join) = goto_edge(cfg, set_join)?;
    let (outer_err_join_edge, outer_join_from_err) = goto_edge(cfg, outer_err_block)?;
    if outer_join_from_err != outer_join
        || cfg.blocks.get(outer_join)?.terminator
            != (Terminator::Return {
                value: Some(outer_match.node_id.clone()),
                implicit: true,
            })
    {
        return None;
    }

    let expected_blocks = BTreeSet::from([
        outer_dispatch.clone(),
        set_dispatch.clone(),
        outer_err_block.clone(),
        set_ok_block.clone(),
        set_err_block.clone(),
        found_true_block.clone(),
        found_false_block.clone(),
        found_join.clone(),
        set_join.clone(),
        outer_join.clone(),
    ]);
    if expected_blocks.len() != 10
        || !expected_blocks.is_subset(&cfg.blocks.keys().cloned().collect::<BTreeSet<_>>())
    {
        return None;
    }
    let expected_edges = BTreeSet::from([
        outer_ok_edge.clone(),
        outer_err_edge.clone(),
        set_ok_edge.clone(),
        set_err_edge.clone(),
        found_true_edge_id.clone(),
        found_false_edge_id.clone(),
        found_true_join_edge.clone(),
        found_false_join_edge.clone(),
        found_to_set_join_edge.clone(),
        set_err_join_edge.clone(),
        set_to_outer_join_edge.clone(),
        outer_err_join_edge.clone(),
    ]);
    let core_edges = cfg
        .edges
        .values()
        .filter(|edge| expected_blocks.contains(&edge.from))
        .map(|edge| edge.id.clone())
        .collect::<BTreeSet<_>>();
    if core_edges != expected_edges
        || cfg
            .blocks
            .iter()
            .filter(|(block, _)| expected_blocks.contains(*block))
            .flat_map(|(_, block)| block.terminator.outgoing_edges())
            .cloned()
            .collect::<BTreeSet<_>>()
            != expected_edges
    {
        return None;
    }

    let contract_nodes = super::new_result::direct_contract_expression_nodes(body);
    if !super::new_result::proof_prelude_is_closed(
        cfg,
        &expected_blocks,
        &outer_dispatch,
        &contract_nodes,
    ) || !super::new_result::proof_nodes_have_exact_point_multiplicity(
        cfg,
        &contract_nodes,
        &contract_nodes,
    ) {
        return None;
    }

    // Exact point-node census: every reachable source point belongs to the
    // checked expression tree above, every point identity is unique, and all
    // operation/binding/drop anchors occur once in their expected block.
    let mut source_nodes = outer_match
        .eager_source_nodes()
        .into_iter()
        .collect::<BTreeSet<_>>();
    source_nodes.extend(new_expression.eager_source_nodes());
    source_nodes.extend(set_expression.eager_source_nodes());
    source_nodes.extend(set_match.eager_source_nodes());
    source_nodes.extend(get_expression.eager_source_nodes());
    source_nodes.extend(found_expression.eager_source_nodes());
    source_nodes.extend(unpack_expression.eager_source_nodes());
    source_nodes.extend(found_expression_nodes(found_result));
    source_nodes.extend(contract_nodes.iter().cloned());
    source_nodes.extend([
        new_expression.node_id.clone(),
        outer_match.node_id.clone(),
        set_expression.node_id.clone(),
        set_match.node_id.clone(),
        get_expression.node_id.clone(),
        found_result.node_id.clone(),
        condition.node_id.clone(),
        unpack_expression.node_id.clone(),
        found_expression.node_id.clone(),
        unpack_argument.node_id.clone(),
        then_block.node_id.clone(),
        else_block.node_id.clone(),
    ]);
    for statement in [
        new_binding,
        set_binding,
        get_binding,
        found_binding,
        unpack_binding,
        old_root_set_ok_drop,
        pair_drop,
        updated_root_get_drop,
        old_root_set_err_drop,
    ] {
        source_nodes.insert(statement.node_id.clone());
        if let ResolvedStmtKind::Bind { pattern, .. } = &statement.kind {
            source_nodes.insert(pattern.node_id.clone());
        }
    }
    for arm in [outer_ok_arm, outer_err_arm, set_ok_arm, set_err_arm] {
        source_nodes.insert(arm.node_id.clone());
        source_nodes.insert(arm.pattern.node_id.clone());
        source_nodes.insert(arm.body.node_id.clone());
    }
    let cfg_points = cfg
        .blocks
        .values()
        .flat_map(|block| &block.points)
        .collect::<Vec<_>>();
    let point_nodes = cfg_points
        .iter()
        .map(|point| point.source.node.clone())
        .collect::<BTreeSet<_>>();
    if point_nodes.len() != cfg_points.len()
        || cfg_points
            .iter()
            .any(|point| !source_nodes.contains(&point.source.node))
        || !has_point(
            cfg,
            &outer_dispatch,
            &new_expression.node_id,
            CfgPointKind::Expression,
        )
        || !has_point(
            cfg,
            &outer_dispatch,
            &new_binding.node_id,
            CfgPointKind::Binding,
        )
        || !has_local_load_point(
            cfg,
            body,
            &outer_dispatch,
            outer_scrutinee,
            new_result_local,
            CfgPointKind::Condition,
        )
        || !has_point(
            cfg,
            set_dispatch,
            &set_expression.node_id,
            CfgPointKind::Expression,
        )
        || !has_point(
            cfg,
            set_dispatch,
            &set_binding.node_id,
            CfgPointKind::Binding,
        )
        || !has_local_load_point(
            cfg,
            body,
            set_dispatch,
            set_scrutinee,
            set_result_local,
            CfgPointKind::Condition,
        )
        || !has_point(
            cfg,
            set_ok_block,
            &get_expression.node_id,
            CfgPointKind::Expression,
        )
        || !has_point(
            cfg,
            set_ok_block,
            &get_binding.node_id,
            CfgPointKind::Binding,
        )
        || !has_point(
            cfg,
            set_ok_block,
            &found_binding.node_id,
            CfgPointKind::Binding,
        )
        || !has_point(
            cfg,
            set_ok_block,
            &unpack_binding.node_id,
            CfgPointKind::Binding,
        )
        || !has_point(
            cfg,
            set_ok_block,
            &unpack_expression.node_id,
            CfgPointKind::Expression,
        )
        || !has_drop_point(
            cfg,
            body,
            set_ok_block,
            old_root_set_ok_drop,
            initial_root_local,
        )
        || !has_drop_point(cfg, body, set_ok_block, pair_drop, get_result_local)
        || !has_drop_point(
            cfg,
            body,
            set_ok_block,
            updated_root_get_drop,
            updated_root_local,
        )
        || !has_drop_point(
            cfg,
            body,
            set_err_block,
            old_root_set_err_drop,
            initial_root_local,
        )
        || !has_local_load_point(
            cfg,
            body,
            found_dispatch,
            condition,
            found_local,
            CfgPointKind::Condition,
        )
    {
        return None;
    }

    Some(Receipt {
        profile_version: Version::V1,
        owner: body.owner.clone(),
        new_operation: new_expression.node_id.clone(),
        new_result_binding: new_binding.node_id.clone(),
        new_result_local: new_result_local.clone(),
        new_result_type: new_expression.ty.clone(),
        outer_match: outer_match.node_id.clone(),
        outer_scrutinee: outer_scrutinee.node_id.clone(),
        outer_ok_arm: outer_ok_arm.node_id.clone(),
        outer_err_arm: outer_err_arm.node_id.clone(),
        initial_root_pattern: initial_root_pattern.node_id.clone(),
        initial_root_local: initial_root_local.clone(),
        initial_root_type: initial_root_type.clone(),
        initial_root: resource_identity(initial_root_local),
        outer_status_pattern: outer_status_pattern.node_id.clone(),
        outer_status_local: outer_status_local.clone(),
        outer_err_result: direct_arm_result(outer_err_arm)?.node_id.clone(),
        set_operation: set_expression.node_id.clone(),
        set_receiver: set_receiver.value.node_id.clone(),
        set_key: set_key.value.node_id.clone(),
        set_key_value: set_key_value.clone(),
        set_value: set_value.value.node_id.clone(),
        set_i32_value: *set_i32_value,
        set_result_binding: set_binding.node_id.clone(),
        set_result_local: set_result_local.clone(),
        set_result_type: set_expression.ty.clone(),
        set_match: set_match.node_id.clone(),
        set_scrutinee: set_scrutinee.node_id.clone(),
        set_ok_arm: set_ok_arm.node_id.clone(),
        set_err_arm: set_err_arm.node_id.clone(),
        updated_root_pattern: updated_root_pattern.node_id.clone(),
        updated_root_local: updated_root_local.clone(),
        updated_root_type: updated_root_type.clone(),
        updated_root: resource_identity(updated_root_local),
        set_error_pattern: set_error_pattern.node_id.clone(),
        set_error_local: set_error_local.clone(),
        set_error_result: direct_arm_result(set_err_arm)?.node_id.clone(),
        old_root_set_ok_drop: old_root_set_ok_drop.node_id.clone(),
        old_root_set_err_drop: old_root_set_err_drop.node_id.clone(),
        get_operation: get_expression.node_id.clone(),
        get_receiver: get_receiver.value.node_id.clone(),
        get_receiver_local: updated_root_local.clone(),
        get_key: get_key.value.node_id.clone(),
        get_key_type: get_key.value.ty.clone(),
        get_key_value: get_key_value.clone(),
        get_result_binding: get_binding.node_id.clone(),
        get_result_local: get_result_local.clone(),
        get_result_type: get_expression.ty.clone(),
        pair_found_projection: found_expression.node_id.clone(),
        pair_payload_projection: unpack_argument.node_id.clone(),
        found_binding: found_binding.node_id.clone(),
        found_local: found_local.clone(),
        unpack_operation: unpack_expression.node_id.clone(),
        unpack_argument: unpack_argument.node_id.clone(),
        unpack_conversion_kind: unpack_conversion.kind,
        unpack_source_type: unpack_conversion.from.clone(),
        unpack_target_type: unpack_conversion.to.clone(),
        unpack_result_local: unpack_result_local.clone(),
        pair_drop: pair_drop.node_id.clone(),
        updated_root_get_drop: updated_root_get_drop.node_id.clone(),
        found_result_match: found_result.node_id.clone(),
        found_result: condition.node_id.clone(),
        found_true_arm: then_block.node_id.clone(),
        found_false_arm: else_block.node_id.clone(),
        entry: outer_dispatch,
        operations: operations.clone(),
        cfg: ManagedAnyCfgReceipt::from_cfg(cfg, body),
    })
}

fn found_expression_nodes(expression: &ResolvedExpr) -> BTreeSet<NodeId> {
    let mut nodes = expression
        .eager_source_nodes()
        .into_iter()
        .collect::<BTreeSet<_>>();
    if let ResolvedExprKind::If {
        then_block,
        else_block,
        ..
    } = &expression.kind
    {
        nodes.insert(then_block.node_id.clone());
        nodes.insert(else_block.node_id.clone());
        if let Some(result) = &then_block.result {
            nodes.extend(result.eager_source_nodes());
        }
        if let Some(result) = &else_block.result {
            nodes.extend(result.eager_source_nodes());
        }
    }
    nodes
}

pub(super) fn validate(
    receipt: &Receipt,
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
    operations: &BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
) -> Result<(), String> {
    if collect(cfg, body, signature, types, operations).as_ref() == Some(receipt) {
        Ok(())
    } else {
        Err("ordinary Map fallible New/Set/GetAny i32 receipt disagrees with exact Checker source/CFG/operation attempts".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!(
        "../../../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_any_i32_lineage.mimi"
    );

    fn checked(source: &str) -> Result<crate::core::CheckedProgram, Vec<Diagnostic>> {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file)
    }

    fn main_receipt(program: &crate::core::CheckedProgram) -> Option<&Receipt> {
        program
            .callable(&NodeId("function:main".into()))?
            .resources
            .ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt
            .as_ref()
    }

    #[test]
    fn fallible_new_set_get_any_i32_receipt_binds_projections_cleanup_and_cfg() {
        let program = checked(FIXTURE).expect("fallible Set/GetAny source checks");
        let callable = program
            .callable(&NodeId("function:main".into()))
            .expect("main callable");
        let receipt = callable
            .resources
            .ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt
            .as_ref()
            .expect("exact New/Set/GetAny Checker receipt");
        validate(
            receipt,
            &callable.cfg,
            &callable.body,
            &callable.signature,
            program.resolved_types(),
            &callable
                .resources
                .ordinary_map_owned_any_v2_operation_attempts,
        )
        .expect("receipt replay binds exact source, CFG, and attempts");
        assert_eq!(receipt.profile_version, Version::V1);
        assert_eq!(receipt.operations.len(), 3);
        assert_eq!(receipt.set_key_value, "answer");
        assert_eq!(receipt.set_i32_value, 42);
        assert_eq!(receipt.get_key_value, "answer");
        assert_ne!(receipt.initial_root_local, receipt.updated_root_local);
        assert_eq!(
            receipt.initial_root,
            resource_identity(&receipt.initial_root_local)
        );
        assert_eq!(
            receipt.updated_root,
            resource_identity(&receipt.updated_root_local)
        );
        assert_eq!(receipt.get_receiver_local, receipt.updated_root_local);
        assert_ne!(
            receipt.pair_found_projection,
            receipt.pair_payload_projection
        );
        assert_eq!(receipt.pair_payload_projection, receipt.unpack_argument);
        assert_eq!(
            receipt.unpack_conversion_kind,
            CheckedConversionKind::DynamicAnyUnpack
        );
        assert_ne!(receipt.old_root_set_ok_drop, receipt.old_root_set_err_drop);
        assert_eq!(receipt.cfg.reachable_blocks.len(), 10);
        assert_eq!(receipt.cfg.reachable_edges.len(), 12);
        assert!(callable
            .resources
            .ordinary_map_owned_any_v2_set_get_i32_lineage_receipt
            .is_none());
    }

    #[test]
    fn leading_success_and_false_ensures_preserve_the_same_l1_lineage_receipt() {
        let contracts = [
            "    ensures: result == 42 || result == 0 || result == 2 || result == 3",
            "    ensures: result == 42 || result == 0",
        ];
        for contract in contracts {
            let source = FIXTURE.replacen(
                "func main() -> i32 {",
                &format!("func main() -> i32 {{\n{contract}"),
                1,
            );
            let program = checked(&source).expect("leading ensures source remains checked");
            let callable = program
                .callable(&NodeId("function:main".into()))
                .expect("main callable");
            let receipt = callable
                .resources
                .ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt
                .as_ref()
                .expect("contracts do not replace or suppress the exact L1 owner proof");
            validate(
                receipt,
                &callable.cfg,
                &callable.body,
                &callable.signature,
                program.resolved_types(),
                &callable
                    .resources
                    .ordinary_map_owned_any_v2_operation_attempts,
            )
            .expect("contract-bearing L1 receipt replays with its full source CFG");
            assert_eq!(receipt.operations.len(), 3);
            assert!(receipt.cfg.reachable_blocks.len() > 10);
        }
    }

    #[test]
    fn fallible_new_set_get_any_i32_receipt_accepts_the_canonical_miss_key() {
        let miss = FIXTURE.replace(
            "let pair = map_get(updated, \"answer\")",
            "let pair = map_get(updated, \"absent\")",
        );
        assert_ne!(miss, FIXTURE, "the canonical miss mutation must apply");
        let program = checked(&miss).expect("canonical miss source checks");
        let callable = program
            .callable(&NodeId("function:main".into()))
            .expect("main callable");
        let receipt = callable
            .resources
            .ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt
            .as_ref()
            .expect("a different static query key remains the same source profile");
        assert_eq!(receipt.set_key_value, "answer");
        assert_eq!(receipt.get_key_value, "absent");
        validate(
            receipt,
            &callable.cfg,
            &callable.body,
            &callable.signature,
            program.resolved_types(),
            &callable
                .resources
                .ordinary_map_owned_any_v2_operation_attempts,
        )
        .expect("canonical miss receipt replays");
    }

    #[test]
    fn fallible_new_set_get_any_i32_receipt_records_exact_keys_and_value() {
        let source = FIXTURE
            .replace(
                "map_set(root, \"answer\", 42)",
                "map_set(root, \"stored\", 73)",
            )
            .replace(
                "map_get(updated, \"answer\")",
                "map_get(updated, \"lookup\")",
            );
        assert_ne!(source, FIXTURE, "the literal mutation must apply");
        let program = checked(&source).expect("literal-bound source checks");
        let receipt = main_receipt(&program).expect("exact literal values are recorded");
        assert_eq!(receipt.set_key_value, "stored");
        assert_eq!(receipt.set_i32_value, 73);
        assert_eq!(receipt.get_key_value, "lookup");
    }

    #[test]
    fn fallible_new_set_get_any_i32_receipt_rejects_near_misses_and_extra_operations() {
        let mutations = [
            (
                "dynamic Get key",
                FIXTURE.replace(
                    "func main() -> i32 {",
                    "func main(query: string) -> i32 {",
                )
                .replace("map_get(updated, \"answer\")", "map_get(updated, query)"),
            ),
            (
                "Set Err skips old-root cleanup",
                FIXTURE.replace("Err(status) => {\n                    drop(root)", "Err(status) => {"),
            ),
            (
                "Set success does not settle old root",
                FIXTURE.replace("                    drop(root)\n                    let pair", "                    let pair"),
            ),
            (
                "Get pair is not explicitly dropped",
                FIXTURE.replace("                    drop(pair)\n", ""),
            ),
            (
                "Get uses a different root",
                FIXTURE.replace("map_get(updated, \"answer\")", "map_get(root, \"answer\")"),
            ),
            (
                "New Err does not propagate its status",
                FIXTURE.replace("Err(status) => status", "Err(status) => 0"),
            ),
            (
                "New Err contains an extra side effect",
                FIXTURE.replace(
                    "Err(status) => status,",
                    "Err(status) => {\n            println(\"hidden\")\n            status\n        },",
                ),
            ),
            (
                "Set success does not settle updated root",
                FIXTURE.replace("                    drop(updated)\n", ""),
            ),
            (
                "Set success drops updated root twice",
                FIXTURE.replace(
                    "                    drop(updated)\n",
                    "                    drop(updated)\n                    drop(updated)\n",
                ),
            ),
            (
                "second generic Get attempt",
                FIXTURE.replace(
                    "                    let pair = map_get(updated, \"answer\")",
                    "                    let extra = map_get(updated, \"answer\")\n                    drop(extra)\n                    let pair = map_get(updated, \"answer\")",
                ),
            ),
            (
                "same callable has an extra Map operation",
                FIXTURE.replace(
                    "Err(status) => {\n                    drop(root)",
                    "Err(status) => {\n                    map_size(root)\n                    drop(root)",
                ),
            ),
        ];
        for (label, source) in mutations {
            assert_ne!(source, FIXTURE, "the {label} mutation must apply");
            if let Ok(program) = checked(&source) {
                assert!(
                    main_receipt(&program).is_none(),
                    "{label} must withhold the dedicated source receipt"
                );
            }
        }
    }
}
