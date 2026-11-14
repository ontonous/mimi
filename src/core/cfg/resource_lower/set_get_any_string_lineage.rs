//! Exact Checker source/CFG authority for one Result-aware ordinary Map
//! lineage ending in the public tuple-valued `map_get -> (bool, Any)` API and
//! a checked `unpack_string` of the exact same-key String Set.
//!
//! This profile is independent of the i32 profile and of Result-valued
//! `map_get_any`. A static same-key Set on the successful Result path proves
//! presence before String unpack; a different or missing key is rejected.

use super::*;
use crate::core::cfg::{BasicBlockId, CfgPointKind, EdgeId};
use crate::core::ir::{CheckedConversionKind, PrimitiveType, ResolvedProjection};
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2Attempt,
    OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
    OrdinaryMapOwnedAnyV2SetGetAnyStringLineageProfileVersion as Version,
    OrdinaryMapOwnedAnyV2SetGetAnyStringLineageReceipt as Receipt,
};

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
    arity: usize,
) -> Option<&'a ResolvedCall> {
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return None;
    };
    (matches!(&call.callee, ResolvedCallee::Builtin(id) if id.as_str() == name)
        && call.arguments.len() == arity
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
        arm.guard.is_none()
            && matches!(&arm.pattern.kind, ResolvedPatternKind::Constructor { variant, .. }
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

fn has_point(cfg: &CallableCfg, block: &BasicBlockId, node: &NodeId, kind: CfgPointKind) -> bool {
    let Some(block) = cfg.blocks.get(block) else {
        return false;
    };
    let mut found = block
        .points
        .iter()
        .filter(|point| point.source.node == *node);
    found.next().is_some_and(|point| point.kind == kind) && found.next().is_none()
}

fn has_drop_point(
    cfg: &CallableCfg,
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
                && point.read_places.len() == 1
                && point.read_places[0].base == crate::core::LocalId(local.0.clone())
                && point.read_places[0].projections.is_empty()
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

    let executable = body
        .root
        .statements
        .iter()
        .filter(|statement| {
            !matches!(
                &statement.kind,
                ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_)
            )
        })
        .collect::<Vec<_>>();
    let [new_binding] = executable.as_slice() else {
        return None;
    };
    let (new_result_local, new_expression) = immutable_binding(new_binding, body)?;
    let new_call = builtin_call(new_expression, "map_new", 0)?;
    if !new_call.arguments.is_empty() {
        return None;
    }
    let (empty_root_type, new_error_type) = result_parts(types, &new_expression.ty)?;
    if new_error_type != &i32_type || !record_type(types, empty_root_type) {
        return None;
    }

    let outer_match = body.root.result.as_deref()?;
    let ResolvedExprKind::Match {
        scrutinee: new_scrutinee,
        arms: new_arms,
    } = &outer_match.kind
    else {
        return None;
    };
    if outer_match.ty != i32_type
        || new_scrutinee.ty != new_expression.ty
        || !matches!(&new_scrutinee.kind, ResolvedExprKind::Load(place)
            if place.base == *new_result_local && place.projections.is_empty())
        || new_arms.len() != 2
        || new_arms
            .iter()
            .any(|arm| arm.guard.is_some() || arm.body.ty != i32_type)
    {
        return None;
    }
    let new_ok_arm = variant_arm(new_arms, "Ok")?;
    let new_err_arm = variant_arm(new_arms, "Err")?;
    let (empty_root_local, _) =
        payload(new_ok_arm, "Ok", &new_expression.ty, empty_root_type, body)?;
    let (new_status_local, _) = payload(new_err_arm, "Err", &new_expression.ty, &i32_type, body)?;
    if BTreeSet::from([
        new_result_local.clone(),
        empty_root_local.clone(),
        new_status_local.clone(),
    ])
    .len()
        != 3
    {
        return None;
    }

    let ResolvedExprKind::Block(new_ok_body) = &new_ok_arm.body.kind else {
        return None;
    };
    let [set_binding] = new_ok_body.statements.as_slice() else {
        return None;
    };
    let (set_result_local, set_expression) = immutable_binding(set_binding, body)?;
    let set_call = builtin_call(set_expression, "map_set", 3)?;
    let [set_receiver, set_key, set_payload] = set_call.arguments.as_slice() else {
        return None;
    };
    if !load(&set_receiver.value, empty_root_local, empty_root_type)
        || set_receiver.conversion.kind != CheckedConversionKind::Identity
        || set_receiver.conversion.from != *empty_root_type
        || set_receiver.conversion.to != *empty_root_type
        || set_key.value.ty != string_type
        || !matches!(
            &set_key.value.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::String(_))
        )
        || set_key.conversion.kind != CheckedConversionKind::Identity
        || set_key.conversion.from != string_type
        || set_key.conversion.to != string_type
        || set_payload.value.ty != string_type
        || !matches!(
            &set_payload.value.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::String(_))
        )
        || set_payload.conversion.kind != CheckedConversionKind::Identity
        || set_payload.conversion.from != string_type
        || set_payload.conversion.to != string_type
    {
        return None;
    }
    let ResolvedExprKind::Literal(ResolvedLiteral::String(set_key_value)) = &set_key.value.kind
    else {
        return None;
    };
    let ResolvedExprKind::Literal(ResolvedLiteral::String(set_payload_text)) =
        &set_payload.value.kind
    else {
        return None;
    };
    let (updated_root_type, set_error_type) = result_parts(types, &set_expression.ty)?;
    if set_error_type != &i32_type || updated_root_type != empty_root_type {
        return None;
    }
    let set_match = new_ok_body.result.as_deref()?;
    let ResolvedExprKind::Match {
        scrutinee: set_scrutinee,
        arms: set_arms,
    } = &set_match.kind
    else {
        return None;
    };
    if set_match.ty != i32_type
        || set_scrutinee.ty != set_expression.ty
        || !matches!(&set_scrutinee.kind, ResolvedExprKind::Load(place)
            if place.base == *set_result_local && place.projections.is_empty())
        || set_arms.len() != 2
        || set_arms
            .iter()
            .any(|arm| arm.guard.is_some() || arm.body.ty != i32_type)
    {
        return None;
    }
    let set_ok_arm = variant_arm(set_arms, "Ok")?;
    let set_err_arm = variant_arm(set_arms, "Err")?;
    let (updated_root_local, _) = payload(
        set_ok_arm,
        "Ok",
        &set_expression.ty,
        updated_root_type,
        body,
    )?;
    let (set_status_local, _) = payload(set_err_arm, "Err", &set_expression.ty, &i32_type, body)?;
    if BTreeSet::from([
        set_result_local.clone(),
        updated_root_local.clone(),
        set_status_local.clone(),
    ])
    .len()
        != 3
        || BTreeSet::from([empty_root_local.clone(), updated_root_local.clone()]).len() != 2
    {
        return None;
    }

    let ResolvedExprKind::Block(set_ok_body) = &set_ok_arm.body.kind else {
        return None;
    };
    let [old_root_drop, get_binding, updated_root_drop, unpack_binding, pair_drop, unpack_drop] =
        set_ok_body.statements.as_slice()
    else {
        return None;
    };
    if !drop_local(old_root_drop, empty_root_local)
        || !drop_local(updated_root_drop, updated_root_local)
    {
        return None;
    }
    let (pair_local, get_expression) = immutable_binding(get_binding, body)?;
    let get_call = builtin_call(get_expression, "map_get", 2)?;
    let [get_receiver, get_key] = get_call.arguments.as_slice() else {
        return None;
    };
    if !load(&get_receiver.value, updated_root_local, updated_root_type)
        || get_receiver.conversion.kind != CheckedConversionKind::Identity
        || get_receiver.conversion.from != *updated_root_type
        || get_receiver.conversion.to != *updated_root_type
        || get_key.value.ty != string_type
        || !matches!(
            &get_key.value.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::String(_))
        )
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
    if set_key_value != get_key_value {
        return None;
    }
    let ResolvedType::Tuple(pair_types) = types.get(&get_expression.ty)? else {
        return None;
    };
    let [found_type, any_type] = pair_types.as_slice() else {
        return None;
    };
    if found_type != &crate::core::ir::primitive_type_id(PrimitiveType::Bool)
        || !matches!(types.get(any_type), Some(ResolvedType::DynamicAny { .. }))
        || body.locals.get(pair_local)?.ty != get_expression.ty
        || body.locals.get(pair_local)?.mutable
    {
        return None;
    }

    let (unpack_local, unpack_expression) = immutable_binding(unpack_binding, body)?;
    let ResolvedExprKind::Cast {
        value: unpack_argument,
        conversion: unpack_conversion,
    } = &unpack_expression.kind
    else {
        return None;
    };
    if unpack_expression.ty != string_type
        || unpack_conversion.kind != CheckedConversionKind::DynamicAnyUnpack
        || unpack_conversion.from != *any_type
        || unpack_conversion.to != string_type
        || !tuple_projection(unpack_argument, pair_local, 1, any_type)
        || unpack_argument.ty != *any_type
        || body.locals.get(unpack_local)?.ty != string_type
        || body.locals.get(unpack_local)?.mutable
        || !drop_local(pair_drop, pair_local)
        || !drop_local(unpack_drop, unpack_local)
        || !matches!(
            &set_ok_body.result.as_ref()?.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
        )
        || set_ok_body.result.as_ref()?.ty != i32_type
    {
        return None;
    }
    let ResolvedExprKind::Block(set_err_body) = &set_err_arm.body.kind else {
        return None;
    };
    let [old_root_set_err_drop] = set_err_body.statements.as_slice() else {
        return None;
    };
    if !drop_local(old_root_set_err_drop, empty_root_local)
        || !load(direct_arm_result(set_err_arm)?, set_status_local, &i32_type)
        || !load(direct_arm_result(new_err_arm)?, new_status_local, &i32_type)
    {
        return None;
    }

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
            receiver_local: Some(empty_root_local.clone()),
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
            result_local: Some(pair_local.clone()),
        },
    ]);
    if operations != &expected_operations {
        return None;
    }

    let expected_locals = BTreeSet::from([
        new_result_local.clone(),
        empty_root_local.clone(),
        new_status_local.clone(),
        set_result_local.clone(),
        updated_root_local.clone(),
        set_status_local.clone(),
        pair_local.clone(),
        unpack_local.clone(),
    ]);
    if body.locals.keys().cloned().collect::<BTreeSet<_>>() != expected_locals
        || cfg.reachable.len() != cfg.blocks.len()
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != cfg.reachable
        || crate::core::cfg::validate_cfg(cfg).is_err()
    {
        return None;
    }

    let mut outer_dispatches = cfg.blocks.iter().filter_map(|(block, basic_block)| {
        matches!(&basic_block.terminator, Terminator::Match { scrutinee, .. }
            if scrutinee == &new_scrutinee.node_id)
        .then_some(block.clone())
    });
    let outer_dispatch = outer_dispatches.next()?;
    if outer_dispatches.next().is_some() {
        return None;
    }
    let (new_ok_edge, set_dispatch) = edge_for_arm(cfg, &outer_dispatch, new_ok_arm)?;
    let (new_err_edge, new_err_block) = edge_for_arm(cfg, &outer_dispatch, new_err_arm)?;
    if set_dispatch == new_err_block {
        return None;
    }
    let Terminator::Match { scrutinee, .. } = &cfg.blocks.get(set_dispatch)?.terminator else {
        return None;
    };
    if scrutinee != &set_scrutinee.node_id {
        return None;
    }
    let (set_ok_edge, set_ok_block) = edge_for_arm(cfg, set_dispatch, set_ok_arm)?;
    let (set_err_edge, set_err_block) = edge_for_arm(cfg, set_dispatch, set_err_arm)?;
    if set_ok_block == set_err_block {
        return None;
    }
    let (set_ok_join_edge, set_join) = goto_edge(cfg, set_ok_block)?;
    let (set_err_join_edge, set_err_join) = goto_edge(cfg, set_err_block)?;
    if set_join != set_err_join {
        return None;
    }
    let (set_outer_join_edge, outer_join) = goto_edge(cfg, set_join)?;
    let (new_err_join_edge, new_err_outer_join) = goto_edge(cfg, new_err_block)?;
    if outer_join != new_err_outer_join
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
        new_err_block.clone(),
        set_ok_block.clone(),
        set_err_block.clone(),
        set_join.clone(),
        outer_join.clone(),
    ]);
    let expected_edges = BTreeSet::from([
        new_ok_edge.clone(),
        new_err_edge.clone(),
        set_ok_edge.clone(),
        set_err_edge.clone(),
        set_ok_join_edge.clone(),
        set_err_join_edge.clone(),
        set_outer_join_edge.clone(),
        new_err_join_edge.clone(),
    ]);
    if expected_blocks.len() != 7
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != expected_blocks
        || cfg.edges.keys().cloned().collect::<BTreeSet<_>>() != expected_edges
    {
        return None;
    }

    let point_nodes = cfg
        .blocks
        .values()
        .flat_map(|block| &block.points)
        .map(|point| point.source.node.clone())
        .collect::<Vec<_>>();
    let point_set = point_nodes.iter().cloned().collect::<BTreeSet<_>>();
    if point_set.len() != point_nodes.len()
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
            &unpack_expression.node_id,
            CfgPointKind::Expression,
        )
        || !has_point(
            cfg,
            set_ok_block,
            &unpack_binding.node_id,
            CfgPointKind::Binding,
        )
        || !has_drop_point(cfg, set_ok_block, old_root_drop, empty_root_local)
        || !has_drop_point(cfg, set_ok_block, updated_root_drop, updated_root_local)
        || !has_drop_point(cfg, set_ok_block, pair_drop, pair_local)
        || !has_drop_point(cfg, set_ok_block, unpack_drop, unpack_local)
        || !has_drop_point(cfg, set_err_block, old_root_set_err_drop, empty_root_local)
    {
        return None;
    }

    Some(Receipt {
        profile_version: Version::V1,
        owner: body.owner.clone(),
        source_body_commitment: format!("{:#?}", body.root),
        new_operation: new_expression.node_id.clone(),
        new_match: outer_match.node_id.clone(),
        new_scrutinee: new_scrutinee.node_id.clone(),
        new_ok_arm: new_ok_arm.node_id.clone(),
        new_err_arm: new_err_arm.node_id.clone(),
        empty_root_pattern: new_ok_arm.pattern.node_id.clone(),
        empty_root_local: empty_root_local.clone(),
        empty_root_type: empty_root_type.clone(),
        new_status_pattern: new_err_arm.pattern.node_id.clone(),
        new_status_local: new_status_local.clone(),
        set_operation: set_expression.node_id.clone(),
        set_match: set_match.node_id.clone(),
        set_scrutinee: set_scrutinee.node_id.clone(),
        set_ok_arm: set_ok_arm.node_id.clone(),
        set_err_arm: set_err_arm.node_id.clone(),
        set_receiver: set_receiver.value.node_id.clone(),
        set_key: set_key.value.node_id.clone(),
        set_key_value: set_key_value.clone(),
        set_payload: set_payload.value.node_id.clone(),
        set_payload_text: set_payload_text.clone(),
        updated_root_pattern: set_ok_arm.pattern.node_id.clone(),
        updated_root_local: updated_root_local.clone(),
        updated_root_type: updated_root_type.clone(),
        set_status_pattern: set_err_arm.pattern.node_id.clone(),
        set_status_local: set_status_local.clone(),
        empty_root_set_ok_drop: old_root_drop.node_id.clone(),
        empty_root_set_err_drop: old_root_set_err_drop.node_id.clone(),
        get_operation: get_expression.node_id.clone(),
        get_receiver: get_receiver.value.node_id.clone(),
        get_key: get_key.value.node_id.clone(),
        get_key_value: get_key_value.clone(),
        pair_binding: get_binding.node_id.clone(),
        pair_local: pair_local.clone(),
        pair_type: get_expression.ty.clone(),
        pair_payload_projection: unpack_argument.node_id.clone(),
        unpack_operation: unpack_expression.node_id.clone(),
        unpack_conversion_kind: unpack_conversion.kind,
        unpack_source_type: unpack_conversion.from.clone(),
        unpack_target_type: unpack_conversion.to.clone(),
        unpack_binding: unpack_binding.node_id.clone(),
        unpack_local: unpack_local.clone(),
        pair_drop: pair_drop.node_id.clone(),
        unpack_drop: unpack_drop.node_id.clone(),
        updated_root_drop: updated_root_drop.node_id.clone(),
        operations: operations.clone(),
        cfg: ManagedAnyCfgReceipt::from_cfg(cfg, body),
    })
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
        Err("ordinary Map fallible New/Set/tuple-GetAny String receipt disagrees with exact Checker source/CFG/operation attempts".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(set_key: &str, get_key: &str, payload: &str, unpack: &str) -> String {
        format!(
            r#"
func main() -> i32 {{
    let created = map_new()
    match created {{
        Ok(empty) => {{
            let changed = map_set(empty, "{set_key}", {payload})
            match changed {{
                Ok(updated) => {{
                    drop(empty)
                    let pair = map_get(updated, "{get_key}")
                    drop(updated)
                    let decoded = {unpack}(pair.1)
                    drop(pair)
                    drop(decoded)
                    0
                }},
                Err(status) => {{ drop(empty); status }},
            }}
        }},
        Err(status) => status,
    }}
}}
"#
        )
    }

    fn checked(source: &str) -> Result<crate::core::CheckedProgram, Vec<Diagnostic>> {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file)
    }

    fn receipt_for(source: &str) -> Option<Receipt> {
        let program = checked(source).ok()?;
        let callable = program.callable(&NodeId("function:main".into()))?;
        callable
            .resources
            .ordinary_map_owned_any_v2_set_get_any_string_lineage_receipt
            .clone()
    }

    #[test]
    fn same_key_string_tuple_get_receipt_binds_snapshot_and_owner_drops() {
        let source = source("answer", "answer", r#""owned text""#, "unpack_string");
        let program = checked(&source).expect("same-key String source typechecks");
        let callable = program
            .callable(&NodeId("function:main".into()))
            .expect("main callable");
        let receipt = callable
            .resources
            .ordinary_map_owned_any_v2_set_get_any_string_lineage_receipt
            .as_ref()
            .expect("exact legacy tuple String receipt");
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
        .expect("receipt replay binds source, CFG, and operation census");
        assert_eq!(receipt.profile_version, Version::V1);
        assert_eq!(receipt.set_key_value, "answer");
        assert_eq!(receipt.get_key_value, "answer");
        assert_eq!(receipt.set_payload_text, "owned text");
        assert_eq!(
            receipt.unpack_conversion_kind,
            CheckedConversionKind::DynamicAnyUnpack
        );
        assert_eq!(
            receipt.unpack_target_type,
            crate::core::ir::primitive_type_id(PrimitiveType::String)
        );
        assert_eq!(receipt.operations.len(), 3);
        assert_ne!(receipt.empty_root_local, receipt.updated_root_local);
        assert_ne!(receipt.pair_local, receipt.unpack_local);
    }

    #[test]
    fn string_tuple_get_receipt_rejects_miss_type_confusion_and_unsettled_roots() {
        for (source, description) in [
            (
                source("stored", "missing", r#""owned text""#, "unpack_string"),
                "miss",
            ),
            (
                source("answer", "answer", r#""owned text""#, "unpack_i32"),
                "wrong type",
            ),
            (
                source("answer", "answer", r#""owned text""#, "unpack_string")
                    .replace("drop(updated)\n", ""),
                "updated root leak",
            ),
            (
                source("answer", "answer", r#""owned text""#, "unpack_string")
                    .replace("drop(pair)\n", ""),
                "pair leak",
            ),
            (
                source("answer", "answer", r#""owned text""#, "unpack_string")
                    .replace("drop(decoded)\n", ""),
                "decoded String leak",
            ),
        ] {
            match checked(&source) {
                Ok(program) => {
                    let callable = program
                        .callable(&NodeId("function:main".into()))
                        .expect("main callable");
                    assert!(
                        callable
                            .resources
                            .ordinary_map_owned_any_v2_set_get_any_string_lineage_receipt
                            .is_none(),
                        "{description} must withhold the exact String source receipt"
                    );
                }
                Err(diagnostics) if description == "miss" || description == "wrong type" => {
                    assert!(
                        diagnostics
                            .iter()
                            .any(|diagnostic| { diagnostic.code.as_deref() == Some("E0200") }),
                        "{description} must fail closed with E0200: {diagnostics:?}"
                    );
                }
                Err(diagnostics) => {
                    assert!(
                        diagnostics.iter().any(|diagnostic| {
                            matches!(
                                diagnostic.code.as_deref(),
                                Some("E0256" | "E0304" | "E0200")
                            )
                        }),
                        "{description} must retain an ownership/source diagnostic: {diagnostics:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn string_tuple_get_receipt_does_not_accept_unmatched_result_root_or_extra_map_call() {
        let direct_result_root = r#"
func main() -> i32 {
    let created = map_new()
    let pair = map_get(created, "answer")
    drop(pair)
    drop(created)
    0
}
"#;
        assert!(checked(direct_result_root).is_err());
        assert!(receipt_for(&source("answer", "answer", r#""text""#, "unpack_string")).is_some());

        let extra_get = source("answer", "answer", r#""text""#, "unpack_string").replace(
            "drop(updated)\n",
            "let extra = map_get(updated, \"answer\")\ndrop(extra)\ndrop(updated)\n",
        );
        let diagnostics = checked(&extra_get)
            .expect_err("an extra tuple Get must make String unpack fail closed");
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")));
    }
}
