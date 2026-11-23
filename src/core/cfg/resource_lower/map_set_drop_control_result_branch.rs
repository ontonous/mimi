//! Exact Checker authority for the three-path fallible Map Set cleanup slice.
//!
//! This collector accepts one closed source shape: `match map_new()`, a
//! nested `match map_set(...)` in New-Ok, explicit input-root cleanup in
//! Set-Err, and explicit cleanup of both roots plus the source payload in
//! Set-Ok. The Result paths are analyzed independently; no sibling arm can
//! discharge another arm's ownership obligation.

use super::*;
use crate::core::cfg::{BasicBlockId, EdgeId, EdgeKind, Terminator};
use crate::core::ir::{CheckedConversionKind, PrimitiveType};
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2MapSetDropControlBranchDrop as BranchDrop,
    OrdinaryMapOwnedAnyV2MapSetDropControlResultBranchProfileVersion as ProfileVersion,
    OrdinaryMapOwnedAnyV2MapSetDropControlResultBranchReceipt as BranchReceipt,
    OrdinaryMapOwnedAnyV2MapSetDropControlResultPath as BranchPath,
    OrdinaryMapOwnedAnyV2MapSetDropControlResultPathKind as PathKind,
};

pub(super) struct Collected {
    pub(super) receipt: BranchReceipt,
    pub(super) actions: Vec<OrdinaryMapOwnedAnyV2Action>,
    pub(super) attempts: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    pub(super) root_loads: Vec<(NodeId, ResolvedLocalId)>,
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

fn is_map_root(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(types.get(ty), Some(ResolvedType::Nominal { item, .. })
        if item.as_str() == "builtin:type:Record")
}

fn is_concrete_record_payload(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(types.get(ty), Some(ResolvedType::Nominal { item, arguments, .. })
        if !item.as_str().starts_with("builtin:type:") && arguments.is_empty())
}

fn is_i32(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(
        types.get(ty),
        Some(ResolvedType::Primitive(PrimitiveType::I32))
    )
}

fn is_string(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(
        types.get(ty),
        Some(ResolvedType::Primitive(PrimitiveType::String))
    )
}

fn immutable_binding<'a>(
    statement: &'a ResolvedStmt,
    body: &ResolvedBody,
) -> Option<(&'a ResolvedLocalId, &'a ResolvedPattern, &'a ResolvedExpr)> {
    let ResolvedStmtKind::Bind {
        pattern,
        initializer: Some(initializer),
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
    let info = body.locals.get(local)?;
    (info.ty == pattern.ty && info.ty == initializer.ty && !info.mutable).then_some((
        local,
        pattern,
        initializer,
    ))
}

fn variant_arm<'a>(arms: &'a [MatchArm], name: &str) -> Option<&'a MatchArm> {
    let expected = format!("builtin:variant:Result::{name}");
    let mut matching = arms.iter().filter(|arm| {
        arm.guard.is_none()
            && matches!(&arm.pattern.kind, ResolvedPatternKind::Constructor { variant, .. }
                if variant.0 == expected)
    });
    let arm = matching.next()?;
    matching.next().is_none().then_some(arm)
}

fn payload_binding<'a>(
    arm: &'a MatchArm,
    variant: &str,
    result_type: &ResolvedTypeId,
    payload_type: &ResolvedTypeId,
    body: &ResolvedBody,
) -> Option<(&'a ResolvedLocalId, &'a ResolvedPattern)> {
    let ResolvedPatternKind::Constructor {
        variant: found,
        fields,
    } = &arm.pattern.kind
    else {
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
    let info = body.locals.get(local)?;
    (found.0 == format!("builtin:variant:Result::{variant}")
        && field.0 == format!("builtin:variant:Result::{variant}/payload:0")
        && arm.guard.is_none()
        && arm.pattern.ty == *result_type
        && pattern.ty == *payload_type
        && info.ty == *payload_type
        && !info.mutable)
        .then_some((local, pattern))
}

fn block(arm: &MatchArm) -> Option<&ResolvedBlock> {
    match &arm.body.kind {
        ResolvedExprKind::Block(block) => Some(block),
        _ => None,
    }
}

fn block_result(arm: &MatchArm) -> Option<&ResolvedExpr> {
    block(arm)?.result.as_deref()
}

fn match_parts(expression: &ResolvedExpr) -> Option<(&ResolvedExpr, &[MatchArm])> {
    match &expression.kind {
        ResolvedExprKind::Match { scrutinee, arms } => Some((scrutinee, arms)),
        _ => None,
    }
}

fn call<'a>(expression: &'a ResolvedExpr, builtin: &str) -> Option<&'a ResolvedCall> {
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return None;
    };
    matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == builtin)
        .then_some(call)
}

fn direct_load(expression: &ResolvedExpr, local: &ResolvedLocalId, ty: &ResolvedTypeId) -> bool {
    expression.ty == *ty
        && matches!(&expression.kind, ResolvedExprKind::Load(place)
            if place.base == *local && place.projections.is_empty())
}

fn direct_drop(
    statement: &ResolvedStmt,
    local: &ResolvedLocalId,
    binding_point: &NodeId,
    binding_type: &ResolvedTypeId,
) -> Option<BranchDrop> {
    let ResolvedStmtKind::Drop(places) = &statement.kind else {
        return None;
    };
    let [place] = places.as_slice() else {
        return None;
    };
    (place.base == *local && place.projections.is_empty()).then(|| BranchDrop {
        local: local.clone(),
        binding_point: binding_point.clone(),
        binding_type: binding_type.clone(),
        settlement_point: statement.node_id.clone(),
        settlement_place_index: 0,
    })
}

fn attempt(
    kind: OrdinaryMapOwnedAnyV2AttemptKind,
    point: &NodeId,
    arguments: &[NodeId],
    receiver: Option<&ResolvedLocalId>,
    result: Option<&ResolvedLocalId>,
) -> OrdinaryMapOwnedAnyV2Attempt {
    OrdinaryMapOwnedAnyV2Attempt {
        kind,
        point: point.clone(),
        arguments: arguments.to_vec(),
        receiver_local: receiver.cloned(),
        result_local: result.cloned(),
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
    let matched = found.next()?;
    found.next().is_none().then_some(matched)
}

fn join_edge<'a>(
    cfg: &'a CallableCfg,
    block: &BasicBlockId,
    arm: &MatchArm,
) -> Option<(&'a EdgeId, &'a BasicBlockId)> {
    let Terminator::Goto { edge } = &cfg.blocks.get(block)?.terminator else {
        return None;
    };
    let edge_value = cfg.edges.get(edge)?;
    (edge_value.from == *block
        && edge_value.kind == EdgeKind::Fallthrough
        && edge_value.source.node == arm.node_id)
        .then_some((edge, &edge_value.to))
}

fn exact_cfg(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    outer_result: &ResolvedExpr,
    new_call: &ResolvedExpr,
    set_match: &ResolvedExpr,
    new_arms: &[MatchArm],
    set_arms: &[MatchArm],
    payload_statement: &ResolvedStmt,
    payload_pattern: &ResolvedPattern,
    payload_initializer: &ResolvedExpr,
    payload_drops: &[&ResolvedStmt],
    root_drops: &[&ResolvedStmt],
) -> Option<ManagedAnyCfgReceipt> {
    if cfg.owner != body.owner
        || crate::core::cfg::validate_cfg(cfg).is_err()
        || cfg.reachable.len() != cfg.blocks.len()
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != cfg.reachable
        || cfg.blocks.len() != 7
        || cfg.edges.len() != 8
    {
        return None;
    }
    let outer_ok = variant_arm(new_arms, "Ok")?;
    let outer_err = variant_arm(new_arms, "Err")?;
    let set_ok = variant_arm(set_arms, "Ok")?;
    let set_err = variant_arm(set_arms, "Err")?;
    let outer_dispatch = cfg.blocks.iter().find_map(|(id, block)| {
        matches!(&block.terminator, Terminator::Match { scrutinee, .. }
            if scrutinee == &new_call.node_id)
        .then_some(id.clone())
    })?;
    let set_dispatch = cfg.blocks.iter().find_map(|(id, block)| {
        matches!(&block.terminator, Terminator::Match { scrutinee, .. }
            if scrutinee == &set_match.node_id)
        .then_some(id.clone())
    })?;
    if outer_dispatch != cfg.entry || outer_dispatch == set_dispatch {
        return None;
    }
    let (new_ok_edge, new_ok_block) = edge_for_arm(cfg, &outer_dispatch, outer_ok)?;
    let (new_err_edge, new_err_block) = edge_for_arm(cfg, &outer_dispatch, outer_err)?;
    let (set_ok_edge, set_ok_block) = edge_for_arm(cfg, &set_dispatch, set_ok)?;
    let (set_err_edge, set_err_block) = edge_for_arm(cfg, &set_dispatch, set_err)?;
    if set_dispatch != *new_ok_block {
        return None;
    }
    let (set_ok_join_edge, set_join) = join_edge(cfg, set_ok_block, set_ok)?;
    let (set_err_join_edge, set_err_join) = join_edge(cfg, set_err_block, set_err)?;
    let (new_err_join_edge, outer_join) = join_edge(cfg, new_err_block, outer_err)?;
    if set_join != set_err_join || set_join == outer_join {
        return None;
    }
    let (set_to_outer_edge, set_to_outer) = match &cfg.blocks.get(set_join)?.terminator {
        Terminator::Goto { edge } => {
            let edge = cfg.edges.get(edge)?;
            (edge.id.clone(), edge.to.clone())
        }
        _ => return None,
    };
    if set_to_outer != *outer_join
        || cfg.blocks.get(outer_join)?.terminator
            != (Terminator::Return {
                value: Some(outer_result.node_id.clone()),
                implicit: true,
            })
    {
        return None;
    }
    let expected_blocks = BTreeSet::from([
        outer_dispatch.clone(),
        new_ok_block.clone(),
        new_err_block.clone(),
        set_ok_block.clone(),
        set_err_block.clone(),
        set_join.clone(),
        outer_join.clone(),
    ]);
    if expected_blocks.len() != 7
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != expected_blocks
    {
        return None;
    }
    let expected_edges = BTreeSet::from([
        new_ok_edge.clone(),
        new_err_edge.clone(),
        set_ok_edge.clone(),
        set_err_edge.clone(),
        set_ok_join_edge.clone(),
        set_err_join_edge.clone(),
        set_to_outer_edge.clone(),
        new_err_join_edge.clone(),
    ]);
    if cfg.edges.keys().cloned().collect::<BTreeSet<_>>() != expected_edges {
        return None;
    }

    let mut source_nodes = outer_result
        .eager_source_nodes()
        .into_iter()
        .collect::<BTreeSet<_>>();
    source_nodes.insert(body.root.node_id.clone());
    source_nodes.insert(payload_statement.node_id.clone());
    source_nodes.insert(payload_pattern.node_id.clone());
    source_nodes.extend(payload_initializer.eager_source_nodes());
    source_nodes.extend(payload_drops.iter().map(|drop| drop.node_id.clone()));
    source_nodes.extend(root_drops.iter().map(|drop| drop.node_id.clone()));
    let points = cfg
        .blocks
        .values()
        .flat_map(|block| &block.points)
        .collect::<Vec<_>>();
    let point_nodes = points
        .iter()
        .map(|point| point.source.node.clone())
        .collect::<BTreeSet<_>>();
    if point_nodes.len() != points.len()
        || points
            .iter()
            .any(|point| !source_nodes.contains(&point.source.node))
        || [new_call, set_match, outer_result]
            .iter()
            .any(|expr| !point_nodes.contains(&expr.node_id))
        || !point_nodes.contains(&payload_statement.node_id)
        || payload_drops
            .iter()
            .any(|drop| !point_nodes.contains(&drop.node_id))
        || root_drops
            .iter()
            .any(|drop| !point_nodes.contains(&drop.node_id))
    {
        return None;
    }
    // Match-arm identity is carried by each exact CFG edge source below.
    // Arm/pattern AST ids are replayed from the ResolvedBody, but the CFG
    // does not emit them as execution points.
    let mut observed_matches = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();
    let mut accounted_edges = BTreeSet::new();
    let mut returns = 0usize;
    for (block_id, block) in &cfg.blocks {
        match &block.terminator {
            Terminator::Match { scrutinee, arms } => {
                let expected = if scrutinee == &new_call.node_id {
                    BTreeSet::from([outer_ok.node_id.clone(), outer_err.node_id.clone()])
                } else if scrutinee == &set_match.node_id {
                    BTreeSet::from([set_ok.node_id.clone(), set_err.node_id.clone()])
                } else {
                    return None;
                };
                if arms.len() != 2 || observed_matches.contains_key(scrutinee) {
                    return None;
                }
                let mut observed = BTreeSet::new();
                for edge_id in arms {
                    let edge = cfg.edges.get(edge_id)?;
                    let target = cfg.blocks.get(&edge.to)?;
                    if edge.from != *block_id
                        || edge.kind != EdgeKind::MatchArm
                        || target.source.node != edge.source.node
                        || !observed.insert(edge.source.node.clone())
                        || !accounted_edges.insert(edge_id.clone())
                    {
                        return None;
                    }
                }
                if observed != expected {
                    return None;
                }
                observed_matches.insert(scrutinee.clone(), observed);
            }
            Terminator::Goto { edge } => {
                let edge = cfg.edges.get(edge)?;
                if edge.from != *block_id
                    || edge.kind != EdgeKind::Fallthrough
                    || !accounted_edges.insert(edge.id.clone())
                {
                    return None;
                }
            }
            Terminator::Return {
                value: Some(value),
                implicit: true,
            } if value == &outer_result.node_id => returns += 1,
            _ => return None,
        }
    }
    if observed_matches.keys().cloned().collect::<BTreeSet<_>>()
        != BTreeSet::from([new_call.node_id.clone(), set_match.node_id.clone()])
        || returns != 1
        || accounted_edges != cfg.edges.keys().cloned().collect::<BTreeSet<_>>()
    {
        return None;
    }
    Some(ManagedAnyCfgReceipt::from_cfg(cfg, body))
}

pub(super) fn collect(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
    constants: &HashMap<NodeId, crate::core::resolved::ResolvedConstant>,
) -> Option<Collected> {
    let i32_ty = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    if cfg.owner != body.owner
        || signature.owner != body.owner
        || signature.result != i32_ty
        || body.root.ty != i32_ty
        || !signature.parameters.is_empty()
        || !signature.generic_parameters.is_empty()
        || !body.parameters.is_empty()
        || !body.captures.is_empty()
        || !body.place_inputs.is_empty()
        || !body.default_values.is_empty()
    {
        return None;
    }
    let [payload_statement] = body.root.statements.as_slice() else {
        return None;
    };
    let (payload_local, payload_pattern, payload_initializer) =
        immutable_binding(payload_statement, body)?;
    if !is_concrete_record_payload(types, &payload_initializer.ty)
        || !matches!(
            &payload_initializer.kind,
            ResolvedExprKind::Record { fields, rest: None, .. }
                if fields.len() == 1
                    && matches!(&fields[0].value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(_)))
                    && is_string(types, &fields[0].value.ty)
        )
    {
        return None;
    }

    let outer_result = body.root.result.as_deref()?;
    let (new_call_expr, new_arms) = match_parts(outer_result)?;
    let new_call = call(new_call_expr, "map_new")?;
    let (root_type, new_error_type) = result_parts(types, &new_call_expr.ty)?;
    if !new_call.arguments.is_empty()
        || !new_call.type_arguments.is_empty()
        || new_call.result != new_call_expr.ty
        || !super::ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(new_call_expr, new_call)
        || !is_map_root(types, root_type)
        || new_error_type != &i32_ty
        || new_arms.len() != 2
        || outer_result.ty != i32_ty
    {
        return None;
    }
    let new_ok = variant_arm(new_arms, "Ok")?;
    let new_err = variant_arm(new_arms, "Err")?;
    let (initial_root, new_ok_pattern) =
        payload_binding(new_ok, "Ok", &new_call_expr.ty, root_type, body)?;
    let (new_error_status, _) = payload_binding(new_err, "Err", &new_call_expr.ty, &i32_ty, body)?;
    if initial_root == payload_local
        || new_error_status == payload_local
        || body.locals.get(initial_root)?.ty != *root_type
        || body.locals.get(new_error_status)?.ty != i32_ty
        || !block(new_ok)?.statements.is_empty()
        || !direct_load(block_result(new_err)?, new_error_status, &i32_ty)
    {
        return None;
    }
    let new_err_block = block(new_err)?;
    let [new_err_payload_drop_statement] = new_err_block.statements.as_slice() else {
        return None;
    };
    let new_err_payload_drop = direct_drop(
        new_err_payload_drop_statement,
        payload_local,
        &payload_pattern.node_id,
        &payload_pattern.ty,
    )?;

    let set_match_expr = block_result(new_ok)?;
    let (set_call_expr, set_arms) = match_parts(set_match_expr)?;
    let set_call = call(set_call_expr, "map_set")?;
    let (set_root_type, set_error_type) = result_parts(types, &set_call_expr.ty)?;
    let [set_source, set_key, set_value] = set_call.arguments.as_slice() else {
        return None;
    };
    if set_call.type_arguments.len() != 0
        || set_call.result != set_call_expr.ty
        || !super::ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(set_call_expr, set_call)
        || set_root_type != root_type
        || set_error_type != &i32_ty
        || !direct_load(&set_source.value, initial_root, root_type)
        || set_source.conversion.kind != CheckedConversionKind::Identity
        || set_source.conversion.from != *root_type
        || set_source.conversion.to != *root_type
        || !matches!(&set_key.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(key)) if key == "payload")
        || !is_string(types, &set_key.value.ty)
        || set_key.conversion.kind != CheckedConversionKind::Identity
        || set_key.conversion.from != set_key.value.ty
        || set_key.conversion.to != set_key.value.ty
        || !direct_load(&set_value.value, payload_local, &payload_initializer.ty)
        || !is_concrete_record_payload(types, &set_value.value.ty)
        || set_value.conversion.kind != CheckedConversionKind::Identity
        || set_value.conversion.from != set_value.value.ty
        || set_value.conversion.to != set_value.value.ty
        || set_arms.len() != 2
    {
        return None;
    }
    let set_ok = variant_arm(set_arms, "Ok")?;
    let set_err = variant_arm(set_arms, "Err")?;
    let (populated_root, set_ok_pattern) =
        payload_binding(set_ok, "Ok", &set_call_expr.ty, root_type, body)?;
    let (set_error_status, _) = payload_binding(set_err, "Err", &set_call_expr.ty, &i32_ty, body)?;
    if populated_root == initial_root
        || populated_root == payload_local
        || set_error_status == payload_local
        || set_error_status == initial_root
        || set_error_status == populated_root
        || body.locals.get(populated_root)?.ty != *root_type
        || body.locals.get(set_error_status)?.ty != i32_ty
        || !direct_load(block_result(set_err)?, set_error_status, &i32_ty)
    {
        return None;
    }

    let set_err_block = block(set_err)?;
    let [set_err_drop_statement, set_err_payload_drop_statement] =
        set_err_block.statements.as_slice()
    else {
        return None;
    };
    let set_err_drop = direct_drop(
        set_err_drop_statement,
        initial_root,
        &new_ok_pattern.node_id,
        root_type,
    )?;
    let set_err_payload_drop = direct_drop(
        set_err_payload_drop_statement,
        payload_local,
        &payload_pattern.node_id,
        &payload_pattern.ty,
    )?;
    let set_ok_block = block(set_ok)?;
    let [drop_old_statement, drop_new_statement, drop_payload_statement] =
        set_ok_block.statements.as_slice()
    else {
        return None;
    };
    let success_old_drop = direct_drop(
        drop_old_statement,
        initial_root,
        &new_ok_pattern.node_id,
        root_type,
    )?;
    let success_new_drop = direct_drop(
        drop_new_statement,
        populated_root,
        &set_ok_pattern.node_id,
        root_type,
    )?;
    let payload_drop = direct_drop(
        drop_payload_statement,
        payload_local,
        &payload_pattern.node_id,
        &payload_pattern.ty,
    )?;
    if !matches!(
        &set_ok_block.result.as_deref()?.kind,
        ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
    ) || !is_i32(types, &set_ok_block.result.as_deref()?.ty)
    {
        return None;
    }
    let expected_locals = BTreeSet::from([
        payload_local.clone(),
        initial_root.clone(),
        new_error_status.clone(),
        populated_root.clone(),
        set_error_status.clone(),
    ]);
    if body.locals.keys().cloned().collect::<BTreeSet<_>>() != expected_locals {
        return None;
    }

    let mut checker = super::OrdinaryMapOwnedAnyV2Checker {
        body,
        types,
        constants,
        actions: Vec::new(),
        operation_attempts: BTreeSet::new(),
        roots: BTreeMap::from([
            (initial_root.clone(), false),
            (populated_root.clone(), false),
        ]),
        returned_map_root_candidates: BTreeMap::new(),
        result_branch_candidates: Vec::new(),
        result_branch_depth: 0,
        temporary_roots: BTreeMap::new(),
        temporary_root_receipts: Vec::new(),
        allowed_root_loads: BTreeSet::new(),
        all_root_loads: Vec::new(),
        invalid: false,
    };
    let mut new_action = super::OrdinaryMapOwnedAnyV2Checker::build_action(
        OrdinaryMapOwnedAnyV2ActionKind::New,
        new_call_expr.node_id.clone(),
        initial_root.clone(),
        None,
        None,
        None,
        None,
        false,
        None,
    );
    new_action.source_node = Some(new_call_expr.node_id.clone());
    new_action.source_type = Some(new_call_expr.ty.clone());
    checker.actions.push(new_action);
    checker.operation_attempts.insert(attempt(
        OrdinaryMapOwnedAnyV2AttemptKind::New,
        &new_call_expr.node_id,
        &[],
        None,
        Some(initial_root),
    ));
    checker
        .allowed_root_loads
        .insert(set_source.value.node_id.clone());
    checker
        .all_root_loads
        .push((set_source.value.node_id.clone(), initial_root.clone()));
    let mut set_action = super::OrdinaryMapOwnedAnyV2Checker::build_action(
        OrdinaryMapOwnedAnyV2ActionKind::Set,
        set_call_expr.node_id.clone(),
        populated_root.clone(),
        Some(set_key.value.node_id.clone()),
        Some(set_key.value.ty.clone()),
        Some(set_value.value.node_id.clone()),
        Some(set_value.value.ty.clone()),
        true,
        Some(initial_root.clone()),
    );
    set_action.source_node = Some(set_source.value.node_id.clone());
    set_action.source_type = Some(set_source.value.ty.clone());
    checker.set_key_metadata(&mut set_action, &set_key.value);
    checker.actions.push(set_action);
    checker.operation_attempts.insert(attempt(
        OrdinaryMapOwnedAnyV2AttemptKind::Set,
        &set_call_expr.node_id,
        &set_call
            .arguments
            .iter()
            .map(|argument| argument.value.node_id.clone())
            .collect::<Vec<_>>(),
        Some(initial_root),
        Some(populated_root),
    ));
    checker.visit_block(set_ok_block);
    if checker.invalid
        || checker.roots.get(initial_root) != Some(&true)
        || checker.roots.get(populated_root) != Some(&true)
    {
        return None;
    }
    checker
        .actions
        .push(super::OrdinaryMapOwnedAnyV2Checker::build_action(
            OrdinaryMapOwnedAnyV2ActionKind::Drop,
            set_err_drop.settlement_point.clone(),
            initial_root.clone(),
            None,
            None,
            None,
            None,
            false,
            None,
        ));
    checker.operation_attempts.insert(attempt(
        OrdinaryMapOwnedAnyV2AttemptKind::Drop,
        &set_err_drop.settlement_point,
        std::slice::from_ref(&initial_root.0),
        Some(initial_root),
        None,
    ));

    let set_err_drops = vec![set_err_drop];
    let success_root_drops = vec![success_old_drop, success_new_drop];
    let success_drops = success_root_drops.clone();
    let all_root_drop_statements = [
        set_err_drop_statement,
        drop_old_statement,
        drop_new_statement,
    ];
    let all_payload_drop_statements = [
        new_err_payload_drop_statement,
        set_err_payload_drop_statement,
        drop_payload_statement,
    ];
    let cfg_receipt = exact_cfg(
        cfg,
        body,
        outer_result,
        new_call_expr,
        set_call_expr,
        new_arms,
        set_arms,
        payload_statement,
        payload_pattern,
        payload_initializer,
        &all_payload_drop_statements,
        &all_root_drop_statements,
    )?;

    let cfg_order = super::OrdinaryMapOwnedAnyV2Checker::acyclic_v2_cfg_order(cfg)?;
    let point_rank = cfg_order
        .iter()
        .enumerate()
        .flat_map(|(block_rank, block_id)| {
            cfg.blocks.get(block_id).into_iter().flat_map(move |block| {
                block
                    .points
                    .iter()
                    .enumerate()
                    .map(move |(rank, point)| (point.source.node.clone(), (block_rank, rank)))
            })
        })
        .collect::<BTreeMap<_, _>>();
    checker.actions.sort_by_key(|action| {
        point_rank
            .get(&action.point)
            .copied()
            .unwrap_or((usize::MAX, usize::MAX))
    });
    if checker
        .actions
        .iter()
        .any(|action| !point_rank.contains_key(&action.point))
        || checker.actions.len() != 5
        || checker.operation_attempts.len() != 5
        || checker.actions.iter().any(|action| {
            !checker.operation_attempts.iter().any(|attempt| {
                attempt.point == action.point
                    && matches!(
                        (action.kind, attempt.kind),
                        (
                            OrdinaryMapOwnedAnyV2ActionKind::New,
                            OrdinaryMapOwnedAnyV2AttemptKind::New,
                        ) | (
                            OrdinaryMapOwnedAnyV2ActionKind::Set,
                            OrdinaryMapOwnedAnyV2AttemptKind::Set,
                        ) | (
                            OrdinaryMapOwnedAnyV2ActionKind::Drop,
                            OrdinaryMapOwnedAnyV2AttemptKind::Drop,
                        )
                    )
            })
        })
    {
        return None;
    }

    let paths = vec![
        BranchPath {
            arm_path: vec![new_err.node_id.clone()],
            result: block_result(new_err)?.node_id.clone(),
            kind: PathKind::MapNewErr,
            drops: Vec::new(),
            payload_drop: new_err_payload_drop,
        },
        BranchPath {
            arm_path: vec![new_ok.node_id.clone(), set_err.node_id.clone()],
            result: block_result(set_err)?.node_id.clone(),
            kind: PathKind::MapSetErr,
            drops: set_err_drops,
            payload_drop: set_err_payload_drop,
        },
        BranchPath {
            arm_path: vec![new_ok.node_id.clone(), set_ok.node_id.clone()],
            result: set_ok_block.result.as_deref()?.node_id.clone(),
            kind: PathKind::Success,
            drops: success_drops,
            payload_drop: payload_drop.clone(),
        },
    ];
    if paths[0].drops.len() != 0
        || paths[1].drops.len() != 1
        || paths[1].drops[0].local != *initial_root
        || paths[2].drops.len() != 2
        || paths[2]
            .drops
            .iter()
            .map(|drop| drop.local.clone())
            .collect::<Vec<_>>()
            != [initial_root.clone(), populated_root.clone()]
        || payload_drop.local != *payload_local
        || paths
            .iter()
            .any(|path| path.payload_drop.local != *payload_local)
        || paths
            .iter()
            .map(|path| path.payload_drop.settlement_point.clone())
            .collect::<BTreeSet<_>>()
            .len()
            != 3
        || success_root_drops.len() != 2
    {
        return None;
    }

    let receipt = BranchReceipt {
        profile_version: ProfileVersion::V1,
        owner: body.owner.clone(),
        map_new_operation: new_call_expr.node_id.clone(),
        map_new_match: outer_result.node_id.clone(),
        map_new_ok_arm: new_ok.node_id.clone(),
        map_new_err_arm: new_err.node_id.clone(),
        map_new_error_status: new_error_status.clone(),
        initial_root: initial_root.clone(),
        map_set_operation: set_call_expr.node_id.clone(),
        map_set_match: set_match_expr.node_id.clone(),
        map_set_ok_arm: set_ok.node_id.clone(),
        map_set_err_arm: set_err.node_id.clone(),
        map_set_error_status: set_error_status.clone(),
        populated_root: populated_root.clone(),
        key: set_key.value.node_id.clone(),
        payload: payload_local.clone(),
        payload_drop: payload_drop.settlement_point,
        paths,
        ordered_action_points: checker
            .actions
            .iter()
            .map(|action| action.point.clone())
            .collect(),
        cfg: cfg_receipt,
    };
    Some(Collected {
        receipt,
        actions: checker.actions,
        attempts: checker.operation_attempts,
        root_loads: checker.all_root_loads,
    })
}

pub(super) fn validate(
    receipt: &BranchReceipt,
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
    constants: &HashMap<NodeId, crate::core::resolved::ResolvedConstant>,
    actions: &[OrdinaryMapOwnedAnyV2Action],
    attempts: &BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
) -> Result<(), String> {
    let collected = collect(cfg, body, signature, types, constants).ok_or_else(|| {
        "source does not match the exact MapSetDropControl Result branch".to_string()
    })?;
    if &collected.receipt != receipt
        || collected.actions != actions
        || &collected.attempts != attempts
        || receipt.cfg != ManagedAnyCfgReceipt::from_cfg(cfg, body)
    {
        return Err("MapSetDropControl Result branch receipt failed Checker replay".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!(
        "../../../../tests/fixtures/mir_ordinary_map_any_v2_map_set_drop_control.mimi"
    );
    fn owner() -> NodeId {
        NodeId("function:main".into())
    }

    fn parse(source: &str) -> crate::ast::File {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex fixture");
        crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse fixture")
    }

    fn checked(source: &str) -> Option<crate::core::resolved::CheckedProgram> {
        crate::core::check_program(&parse(source)).ok()
    }

    fn receipt(
        source: &str,
    ) -> Option<crate::core::ownership::OrdinaryMapOwnedAnyV2MapSetDropControlResultBranchReceipt>
    {
        let checked = checked(source)?;
        checked
            .resource_analysis(&owner())?
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()?
            .map_set_drop_control_result_branch_receipt
            .clone()
    }

    #[test]
    fn map_set_drop_control_result_branch_receipt_binds_all_three_paths() {
        let checked = checked(FIXTURE).expect("exact fixture type checks");
        let callable = checked.callable(&owner()).expect("resolved main");
        let direct = collect(
            &callable.cfg,
            &callable.body,
            &callable.signature,
            checked.resolved_types(),
            checked.constants(),
        )
        .expect("closed checker-side source and CFG collector");
        let cfg_receipt = callable
            .resources
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("ordinary V2 Checker receipt");
        let receipt = cfg_receipt
            .map_set_drop_control_result_branch_receipt
            .as_ref()
            .expect("three-path Result receipt");
        assert_eq!(receipt, &direct.receipt);
        assert_eq!(receipt.profile_version, ProfileVersion::V1);
        assert_eq!(receipt.paths.len(), 3);
        let by_kind = receipt
            .paths
            .iter()
            .map(|path| (path.kind, path))
            .collect::<BTreeMap<_, _>>();
        assert!(by_kind[&PathKind::MapNewErr].drops.is_empty());
        assert_eq!(
            by_kind[&PathKind::MapSetErr]
                .drops
                .iter()
                .map(|drop| &drop.local)
                .collect::<Vec<_>>(),
            [&receipt.initial_root]
        );
        assert_eq!(
            by_kind[&PathKind::Success]
                .drops
                .iter()
                .map(|drop| drop.local.clone())
                .collect::<Vec<_>>(),
            [receipt.initial_root.clone(), receipt.populated_root.clone()]
        );
        for path in &receipt.paths {
            assert_eq!(path.payload_drop.local, receipt.payload);
        }
        assert_eq!(
            receipt
                .paths
                .iter()
                .map(|path| path.payload_drop.settlement_point.clone())
                .collect::<BTreeSet<_>>()
                .len(),
            3,
            "each mutually exclusive path must settle the payload at its own source point"
        );
        assert_eq!(cfg_receipt.actions.len(), 5);
        assert_eq!(cfg_receipt.attempts.len(), 5);
        assert_eq!(
            cfg_receipt
                .actions
                .iter()
                .filter(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Drop)
                .count(),
            3
        );
        assert!(cfg_receipt
            .actions
            .iter()
            .all(|action| { receipt.ordered_action_points.contains(&action.point) }));
        crate::core::cfg::validate_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_receipt(
            receipt,
            &callable.cfg,
            &callable.body,
            &callable.signature,
            checked.resolved_types(),
            checked.constants(),
            &cfg_receipt.actions,
            &cfg_receipt.attempts,
        )
        .expect("Checker replay of exact source and CFG");
    }

    #[test]
    fn map_set_drop_control_result_branch_rejects_missing_or_duplicate_set_error_drop() {
        let missing = FIXTURE.replace(
            "        drop(empty)\n        drop(payload)\n        status",
            "        drop(payload)\n        status",
        );
        assert_ne!(missing, FIXTURE);
        assert!(
            receipt(&missing).is_none(),
            "Set-Err must drop its input root"
        );

        let duplicate = FIXTURE.replace(
            "        drop(empty)\n        drop(payload)\n        status",
            "        drop(empty)\n        drop(empty)\n        drop(payload)\n        status",
        );
        assert_ne!(duplicate, FIXTURE);
        assert!(
            receipt(&duplicate).is_none(),
            "a duplicated Set-Err root Drop cannot receive a receipt"
        );
    }

    #[test]
    fn map_set_drop_control_result_branch_rejects_sibling_path_drop_substitution() {
        let missing_error = FIXTURE.replace(
            "        drop(empty)\n        drop(payload)\n        status",
            "        drop(payload)\n        status",
        );
        let sibling_only = missing_error.replace(
            "            drop(empty)\n            drop(populated)",
            "            drop(empty)\n            drop(populated)\n            drop(empty)",
        );
        assert_ne!(sibling_only, FIXTURE);
        assert!(
            receipt(&sibling_only).is_none(),
            "a Success-path Drop cannot stand in for the Set-Err cleanup"
        );
    }

    #[test]
    fn map_set_drop_control_result_branch_rejects_missing_error_path_payload_drop() {
        let missing_new_error_payload = FIXTURE.replace(
            "        drop(payload)\n        status\n        },\n    }\n}",
            "        status\n        },\n    }\n}",
        );
        assert_ne!(missing_new_error_payload, FIXTURE);
        assert!(
            receipt(&missing_new_error_payload).is_none(),
            "New-Err must settle the payload created before map_new"
        );

        let missing_set_error_payload = FIXTURE.replace(
            "        drop(empty)\n        drop(payload)\n        status",
            "        drop(empty)\n        status",
        );
        assert_ne!(missing_set_error_payload, FIXTURE);
        assert!(
            receipt(&missing_set_error_payload).is_none(),
            "Set-Err must settle the input root and source payload"
        );
    }

    #[test]
    fn map_set_drop_control_result_branch_rejects_wrong_error_status_propagation() {
        let wrong_set_status = FIXTURE.replace(
            "        drop(empty)\n        drop(payload)\n        status",
            "        drop(empty)\n        drop(payload)\n        17",
        );
        assert_ne!(wrong_set_status, FIXTURE);
        assert!(
            receipt(&wrong_set_status).is_none(),
            "Set-Err must return its exact bound status"
        );

        let status = "        status\n";
        let last_status = FIXTURE.rfind(status).expect("New-Err status occurrence");
        let mut wrong_new_status = FIXTURE.to_owned();
        wrong_new_status.replace_range(last_status..last_status + status.len(), "        23\n");
        assert!(
            receipt(&wrong_new_status).is_none(),
            "New-Err must return its exact bound status"
        );
    }

    #[test]
    fn map_set_drop_control_result_branch_rejects_cfg_edge_and_reachability_mutations() {
        let checked = checked(FIXTURE).expect("fixture type checks");
        let callable = checked.callable(&owner()).expect("resolved main");
        let original = callable
            .resources
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .and_then(|receipt| receipt.map_set_drop_control_result_branch_receipt.as_ref())
            .expect("exact branch receipt");

        let mut forged_edge_cfg = callable.cfg.clone();
        let edge_id = forged_edge_cfg
            .edges
            .keys()
            .next()
            .expect("CFG edge")
            .clone();
        forged_edge_cfg.edges.get_mut(&edge_id).unwrap().source.node =
            NodeId("source:wrong-result-arm-edge".into());
        assert!(
            collect(
                &forged_edge_cfg,
                &callable.body,
                &callable.signature,
                checked.resolved_types(),
                checked.constants(),
            )
            .is_none(),
            "the exact arm-to-edge source identity is required"
        );

        let mut incomplete_cfg = callable.cfg.clone();
        let removed = incomplete_cfg
            .reachable
            .iter()
            .find(|block| **block != incomplete_cfg.entry)
            .unwrap()
            .clone();
        incomplete_cfg.reachable.remove(&removed);
        assert!(
            collect(
                &incomplete_cfg,
                &callable.body,
                &callable.signature,
                checked.resolved_types(),
                checked.constants(),
            )
            .is_none(),
            "all CFG blocks must remain reachable and receipted"
        );
        assert_eq!(original.cfg.reachable_blocks.len(), 7);
        assert_eq!(original.cfg.reachable_edges.len(), 8);
    }
}
