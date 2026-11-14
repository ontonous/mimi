//! Exact Checker authority for the nested Result form of the
//! `ValuesLocalIndexI32` owner profile. This parser intentionally accepts one
//! New/Set/Values/index/unpack graph; the generic owner walker remains closed
//! for Result matches.

use super::*;
use crate::core::cfg::{BasicBlockId, CfgPointKind, EdgeId, EdgeKind, Terminator};
use crate::core::ir::{CheckedConversionKind, ContractKind};
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2ValuesLocalIndexI32BranchDrop as BranchDrop,
    OrdinaryMapOwnedAnyV2ValuesLocalIndexI32BranchDropOwner as DropOwner,
    OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchProfileVersion as ProfileVersion,
    OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchReceipt as BranchReceipt,
    OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultPath as BranchPath,
    OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultPathKind as PathKind,
};
use crate::core::{PrimitiveType, ResolvedType};
use std::collections::VecDeque;

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
    matches!(
        types.get(ty),
        Some(ResolvedType::Nominal { item, arguments, .. })
            if item.as_str() == "builtin:type:Record" && arguments.is_empty()
    )
}

fn is_i32(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(
        types.get(ty),
        Some(ResolvedType::Primitive(PrimitiveType::I32))
    )
}

fn variant_arm<'a>(arms: &'a [MatchArm], name: &str) -> Option<&'a MatchArm> {
    let expected = format!("builtin:variant:Result::{name}");
    let mut found = arms.iter().filter(|arm| {
        arm.guard.is_none()
            && matches!(
                &arm.pattern.kind,
                ResolvedPatternKind::Constructor { variant, .. }
                    if variant.0 == expected
            )
    });
    let arm = found.next()?;
    found.next().is_none().then_some(arm)
}

fn payload_binding<'a>(
    arm: &'a MatchArm,
    name: &str,
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
    let local_info = body.locals.get(local)?;
    (variant.0 == format!("builtin:variant:Result::{name}")
        && field.0 == format!("builtin:variant:Result::{name}/payload:0")
        && arm.pattern.ty == *result_type
        && pattern.ty == *payload_type
        && local_info.ty == *payload_type
        && !local_info.mutable)
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

fn match_expression(expression: &ResolvedExpr) -> Option<(&ResolvedExpr, &[MatchArm])> {
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

fn direct_load(expression: &ResolvedExpr, local: &ResolvedLocalId) -> bool {
    matches!(&expression.kind, ResolvedExprKind::Load(place)
        if place.base == *local && place.projections.is_empty())
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
    (pattern.ty == info.ty && initializer.ty == info.ty && !info.mutable).then_some((
        local,
        pattern,
        initializer,
    ))
}

fn direct_drop(
    statement: &ResolvedStmt,
    owner: DropOwner,
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
        owner,
        local: local.clone(),
        binding_point: binding_point.clone(),
        binding_type: binding_type.clone(),
        settlement_point: statement.node_id.clone(),
        settlement_place_index: 0,
    })
}

fn exact_identity_argument(argument: &crate::core::ir::ResolvedArgument) -> bool {
    argument.conversion.kind == CheckedConversionKind::Identity
        && argument.conversion.from == argument.value.ty
        && argument.conversion.to == argument.value.ty
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

fn cfg_is_acyclic(cfg: &CallableCfg) -> bool {
    let mut incoming = cfg
        .reachable
        .iter()
        .cloned()
        .map(|block| (block, 0usize))
        .collect::<BTreeMap<_, _>>();
    for edge in cfg.edges.values() {
        let Some(count) = incoming.get_mut(&edge.to) else {
            return false;
        };
        *count += 1;
    }
    let mut ready = incoming
        .iter()
        .filter_map(|(block, count)| (*count == 0).then_some(block.clone()))
        .collect::<VecDeque<_>>();
    let mut visited = 0usize;
    while let Some(block_id) = ready.pop_front() {
        visited += 1;
        let Some(block) = cfg.blocks.get(&block_id) else {
            return false;
        };
        let edge_ids: Vec<_> = match &block.terminator {
            Terminator::Goto { edge } => vec![edge],
            Terminator::Branch {
                then_edge,
                else_edge,
                ..
            } => vec![then_edge, else_edge],
            Terminator::Match { arms, .. } => arms.iter().collect(),
            Terminator::Return { .. } => Vec::new(),
            _ => return false,
        };
        for edge_id in edge_ids {
            let Some(edge) = cfg.edges.get(edge_id) else {
                return false;
            };
            let Some(count) = incoming.get_mut(&edge.to) else {
                return false;
            };
            *count -= 1;
            if *count == 0 {
                ready.push_back(edge.to.clone());
            }
        }
    }
    visited == cfg.reachable.len()
}

fn edge_for_arm<'a>(
    cfg: &'a CallableCfg,
    dispatch: &BasicBlockId,
    arm: &MatchArm,
) -> Option<(&'a EdgeId, &'a BasicBlockId)> {
    let Terminator::Match { arms, .. } = &cfg.blocks.get(dispatch)?.terminator else {
        return None;
    };
    let mut matching = arms.iter().filter_map(|edge_id| {
        let edge = cfg.edges.get(edge_id)?;
        (edge.source.node == arm.node_id).then_some((edge_id, edge))
    });
    let (edge_id, edge) = matching.next()?;
    let target = cfg.blocks.get(&edge.to)?;
    (matching.next().is_none()
        && edge.from == *dispatch
        && edge.kind == EdgeKind::MatchArm
        && target.source.node == arm.node_id)
        .then_some((edge_id, &edge.to))
}

fn join_edge<'a>(
    cfg: &'a CallableCfg,
    arm_block: &BasicBlockId,
    arm: &MatchArm,
) -> Option<(&'a EdgeId, &'a BasicBlockId)> {
    let Terminator::Goto { edge } = &cfg.blocks.get(arm_block)?.terminator else {
        return None;
    };
    let edge_value = cfg.edges.get(edge)?;
    (edge_value.from == *arm_block
        && edge_value.kind == EdgeKind::Fallthrough
        && edge_value.source.node == arm.node_id)
        .then_some((edge, &edge_value.to))
}

fn has_point_kind(
    cfg: &CallableCfg,
    block: &BasicBlockId,
    source: &NodeId,
    kind: CfgPointKind,
) -> bool {
    cfg.blocks.get(block).is_some_and(|block| {
        let mut found = block
            .points
            .iter()
            .filter(|point| point.source.node == *source);
        found.next().is_some_and(|point| point.kind == kind) && found.next().is_none()
    })
}

fn points_in_order(cfg: &CallableCfg, block: &BasicBlockId, sources: &[NodeId]) -> bool {
    let Some(block) = cfg.blocks.get(block) else {
        return false;
    };
    let mut previous = None;
    for source in sources {
        let mut matching = block
            .points
            .iter()
            .enumerate()
            .filter(|(_, point)| point.source.node == *source);
        let Some((index, _)) = matching.next() else {
            return false;
        };
        if matching.next().is_some() || previous.is_some_and(|previous| index <= previous) {
            return false;
        }
        previous = Some(index);
    }
    true
}

fn exact_cfg(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    root_result: &ResolvedExpr,
    set_match: &ResolvedExpr,
    new_call: &ResolvedExpr,
    set_call: &ResolvedExpr,
    new_ok: &MatchArm,
    new_err: &MatchArm,
    set_ok: &MatchArm,
    set_err: &MatchArm,
    set_err_drop: &BranchDrop,
    success_drops: &[&BranchDrop],
    new_err_result: &NodeId,
    set_err_result: &NodeId,
    success_result: &NodeId,
    set_order: &[NodeId],
    set_kinds: &[(NodeId, CfgPointKind)],
    success_order: &[NodeId],
    success_kinds: &[(NodeId, CfgPointKind)],
) -> Option<ManagedAnyCfgReceipt> {
    let ensures_condition =
        body.root
            .statements
            .first()
            .and_then(|statement| match &statement.kind {
                ResolvedStmtKind::Contract {
                    kind: ContractKind::Ensures,
                    condition,
                } => Some(condition),
                _ => None,
            });
    let outer_dispatch_source = ensures_condition
        .map(|condition| condition.node_id.clone())
        .unwrap_or_else(|| body.root.node_id.clone());
    if cfg.owner != body.owner
        || crate::core::cfg::validate_cfg(cfg).is_err()
        || cfg.reachable.len() != cfg.blocks.len()
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != cfg.reachable
        || !cfg_is_acyclic(cfg)
    {
        return None;
    }

    let outer_dispatch = cfg.blocks.iter().find_map(|(id, block)| {
        matches!(&block.terminator, Terminator::Match { scrutinee, .. }
            if scrutinee == &new_call.node_id)
        .then_some(id.clone())
    })?;
    let set_dispatch = cfg.blocks.iter().find_map(|(id, block)| {
        matches!(&block.terminator, Terminator::Match { scrutinee, .. }
            if scrutinee == &set_call.node_id)
        .then_some(id.clone())
    })?;
    if outer_dispatch == set_dispatch {
        return None;
    }
    let (new_ok_edge, new_ok_block) = edge_for_arm(cfg, &outer_dispatch, new_ok)?;
    let (new_err_edge, new_err_block) = edge_for_arm(cfg, &outer_dispatch, new_err)?;
    let (set_ok_edge, set_ok_block) = edge_for_arm(cfg, &set_dispatch, set_ok)?;
    let (set_err_edge, set_err_block) = edge_for_arm(cfg, &set_dispatch, set_err)?;
    if set_dispatch != *new_ok_block {
        return None;
    }
    let (set_ok_join_edge, set_join) = join_edge(cfg, set_ok_block, set_ok)?;
    let (set_err_join_edge, set_err_join) = join_edge(cfg, set_err_block, set_err)?;
    let (new_err_join_edge, outer_join) = join_edge(cfg, new_err_block, new_err)?;
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
    let set_to_outer_cfg_edge = cfg.edges.get(&set_to_outer_edge)?;
    if set_to_outer != *outer_join
        || set_to_outer_cfg_edge.from != *set_join
        || set_to_outer_cfg_edge.kind != EdgeKind::Fallthrough
        || set_to_outer_cfg_edge.source.node != new_ok.node_id
        || cfg.blocks.get(outer_join)?.terminator
            != (Terminator::Return {
                value: Some(root_result.node_id.clone()),
                implicit: true,
            })
    {
        return None;
    }
    if cfg.blocks.get(set_join)?.source.node != set_match.node_id
        || cfg.blocks.get(outer_join)?.source.node != root_result.node_id
        || cfg.blocks.get(&outer_dispatch)?.source.node != outer_dispatch_source
        || !has_point_kind(
            cfg,
            set_ok_block,
            &set_ok.pattern.node_id,
            CfgPointKind::Binding,
        )
        || !points_in_order(cfg, new_ok_block, set_order)
        || set_kinds
            .iter()
            .any(|(source, kind)| !has_point_kind(cfg, new_ok_block, source, *kind))
        || success_kinds
            .iter()
            .any(|(source, kind)| !has_point_kind(cfg, set_ok_block, source, *kind))
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
    if expected_blocks.len() != 7 {
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
    if !expected_blocks
        .iter()
        .all(|block| cfg.blocks.contains_key(block))
        || !expected_edges
            .iter()
            .all(|edge| cfg.edges.contains_key(edge))
        || cfg
            .edges
            .values()
            .filter(|edge| expected_blocks.contains(&edge.from))
            .map(|edge| edge.id.clone())
            .collect::<BTreeSet<_>>()
            != expected_edges
        || !has_point_kind(
            cfg,
            &outer_dispatch,
            &new_call.node_id,
            CfgPointKind::Condition,
        )
        || !has_point_kind(
            cfg,
            new_ok_block,
            &set_call.node_id,
            CfgPointKind::Condition,
        )
        || !has_point_kind(
            cfg,
            new_err_block,
            &new_err.pattern.node_id,
            CfgPointKind::Binding,
        )
        || !has_point_kind(cfg, new_err_block, new_err_result, CfgPointKind::Expression)
        || !points_in_order(
            cfg,
            new_err_block,
            &[new_err.pattern.node_id.clone(), new_err_result.clone()],
        )
        || !has_point_kind(
            cfg,
            set_err_block,
            &set_err.pattern.node_id,
            CfgPointKind::Binding,
        )
        || !has_point_kind(
            cfg,
            set_err_block,
            &set_err_drop.settlement_point,
            CfgPointKind::ResourceAction,
        )
        || !has_point_kind(cfg, set_err_block, set_err_result, CfgPointKind::Expression)
        || !points_in_order(
            cfg,
            set_err_block,
            &[
                set_err.pattern.node_id.clone(),
                set_err_drop.settlement_point.clone(),
                set_err_result.clone(),
            ],
        )
        || success_drops.iter().any(|drop| {
            !has_point_kind(
                cfg,
                set_ok_block,
                &drop.settlement_point,
                CfgPointKind::ResourceAction,
            )
        })
        || !points_in_order(cfg, set_ok_block, success_order)
        || !has_point_kind(cfg, set_ok_block, success_result, CfgPointKind::Expression)
        || !has_point_kind(cfg, set_join, &set_match.node_id, CfgPointKind::Expression)
        || !has_point_kind(
            cfg,
            outer_join,
            &root_result.node_id,
            CfgPointKind::Expression,
        )
    {
        return None;
    }

    let contract_shortcut_nodes = match (body.root.statements.as_slice(), ensures_condition) {
        ([], None)
            if cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() == expected_blocks
                && cfg.edges.keys().cloned().collect::<BTreeSet<_>>() == expected_edges
                && cfg.entry == outer_dispatch =>
        {
            BTreeSet::new()
        }
        ([_statement], Some(condition)) => {
            if !crate::core::ir::values_local_index_i32_result_branch_contract_is_direct_result_domain(
                condition,
                &body.owner,
            ) {
                return None;
            }
            let shortcut_nodes =
                super::set_get_any_result_i32_lineage::validates_direct_result_domain_ensures_cfg(
                    cfg,
                    body,
                    condition,
                    &expected_blocks,
                    &expected_edges,
                )?;
            let proof_nodes = super::new_result::direct_proof_expression_nodes(body);
            let contract_nodes = super::new_result::direct_contract_expression_nodes(body);
            if !shortcut_nodes.is_subset(&proof_nodes)
                || !super::new_result::proof_prelude_is_closed(
                    cfg,
                    &expected_blocks,
                    &outer_dispatch,
                    &proof_nodes,
                )
                || !super::new_result::proof_nodes_have_exact_point_multiplicity(
                    cfg,
                    &proof_nodes,
                    &contract_nodes,
                )
            {
                return None;
            }
            shortcut_nodes
        }
        _ => return None,
    };

    let mut source_nodes = root_result
        .eager_source_nodes()
        .into_iter()
        .collect::<BTreeSet<_>>();
    source_nodes.insert(body.root.node_id.clone());
    for statement in &body.root.statements {
        let ResolvedStmtKind::Contract { condition, .. } = &statement.kind else {
            return None;
        };
        source_nodes.insert(statement.node_id.clone());
        source_nodes.extend(condition.eager_source_nodes());
    }
    source_nodes.extend(contract_shortcut_nodes);

    let mut match_scrutinees = BTreeSet::new();
    let mut accounted_edges = BTreeSet::new();
    let mut point_nodes = BTreeSet::new();
    let mut returns = 0usize;
    for (block_id, block) in &cfg.blocks {
        if !expected_blocks.contains(block_id) {
            continue;
        }
        match &block.terminator {
            Terminator::Match { scrutinee, arms } => {
                if (scrutinee != &new_call.node_id && scrutinee != &set_call.node_id)
                    || !match_scrutinees.insert(scrutinee.clone())
                    || arms.len() != 2
                {
                    return None;
                }
                let mut observed = BTreeSet::new();
                for edge_id in arms {
                    let edge = cfg.edges.get(edge_id)?;
                    let target = cfg.blocks.get(&edge.to)?;
                    if edge.from != *block_id
                        || edge.kind != EdgeKind::MatchArm
                        || !cfg.reachable.contains(&edge.to)
                        || target.source.node != edge.source.node
                        || !observed.insert(edge.source.node.clone())
                        || !accounted_edges.insert(edge_id.clone())
                    {
                        return None;
                    }
                }
                let expected = if scrutinee == &new_call.node_id {
                    BTreeSet::from([new_ok.node_id.clone(), new_err.node_id.clone()])
                } else {
                    BTreeSet::from([set_ok.node_id.clone(), set_err.node_id.clone()])
                };
                if observed != expected {
                    return None;
                }
            }
            Terminator::Goto { edge } => {
                let edge = cfg.edges.get(edge)?;
                if edge.from != *block_id
                    || edge.kind != EdgeKind::Fallthrough
                    || !cfg.reachable.contains(&edge.to)
                    || !accounted_edges.insert(edge.id.clone())
                {
                    return None;
                }
            }
            Terminator::Return {
                value: Some(value),
                implicit: true,
            } if value == &root_result.node_id => returns += 1,
            _ => return None,
        }
        for point in &block.points {
            if !source_nodes.contains(&point.source.node)
                || !point_nodes.insert(point.source.node.clone())
            {
                return None;
            }
        }
    }
    if match_scrutinees != BTreeSet::from([new_call.node_id.clone(), set_call.node_id.clone()])
        || returns != 1
        || accounted_edges != expected_edges
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
    _constants: &HashMap<NodeId, crate::core::resolved::ResolvedConstant>,
) -> Option<Collected> {
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
    if cfg.owner != body.owner
        || signature.owner != body.owner
        || !signature.parameters.is_empty()
        || !signature.generic_parameters.is_empty()
        || signature.result != i32_type
        || !body.parameters.is_empty()
        || !body.captures.is_empty()
        || !body.place_inputs.is_empty()
        || !body.default_values.is_empty()
        || body.root.ty != i32_type
        || body.root.statements.len() > 1
        || body.root.statements.iter().any(|statement| {
            !matches!(
                &statement.kind,
                ResolvedStmtKind::Contract {
                    kind: ContractKind::Ensures,
                    condition,
                } if condition.ty == bool_type
            )
        })
    {
        return None;
    }

    let outer_match = body.root.result.as_deref()?;
    let (new_call_expr, new_arms) = match_expression(outer_match)?;
    let new_call = call(new_call_expr, "map_new")?;
    let (root_type, new_error_type) = result_parts(types, &new_call_expr.ty)?;
    if !new_call.arguments.is_empty()
        || !new_call.type_arguments.is_empty()
        || new_call.permission.is_some()
        || !new_call.effects.is_empty()
        || !new_call.session.is_empty()
        || new_call.result != new_call_expr.ty
        || !super::ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(new_call_expr, new_call)
        || !is_map_root(types, root_type)
        || new_error_type != &i32_type
        || new_arms.len() != 2
    {
        return None;
    }
    let new_ok = variant_arm(new_arms, "Ok")?;
    let new_err = variant_arm(new_arms, "Err")?;
    let (initial_root, new_ok_pattern) =
        payload_binding(new_ok, "Ok", &new_call_expr.ty, root_type, body)?;
    let (new_status, _) = payload_binding(new_err, "Err", &new_call_expr.ty, &i32_type, body)?;
    if body.locals.get(initial_root)?.mutable
        || body.locals.get(initial_root)?.ty != *root_type
        || block(new_ok)?.statements.len() != 0
        || block(new_err)?.statements.len() != 0
        || !direct_load(block_result(new_err)?, new_status)
    {
        return None;
    }

    let set_match_expr = block_result(new_ok)?;
    let (set_call_expr, set_arms) = match_expression(set_match_expr)?;
    let set_call = call(set_call_expr, "map_set")?;
    let (set_root_type, set_error_type) = result_parts(types, &set_call_expr.ty)?;
    let [set_source, set_key, set_payload] = set_call.arguments.as_slice() else {
        return None;
    };
    if !set_call.type_arguments.is_empty()
        || set_call.permission.is_some()
        || !set_call.effects.is_empty()
        || !set_call.session.is_empty()
        || set_call.result != set_call_expr.ty
        || !super::ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(set_call_expr, set_call)
        || set_error_type != &i32_type
        || set_root_type != root_type
        || !is_map_root(types, &set_source.value.ty)
        || set_source.value.ty != *root_type
        || !direct_load(&set_source.value, initial_root)
        || !exact_identity_argument(set_source)
        || !exact_identity_argument(set_key)
        || !exact_identity_argument(set_payload)
        || !matches!(
            &set_key.value.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::String(value)) if value == "answer"
        )
        || !matches!(
            &set_payload.value.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(41))
        )
        || !is_i32(types, &set_payload.value.ty)
        || set_payload.value.ty != set_payload.conversion.from
        || set_payload.value.ty != set_payload.conversion.to
        || set_arms.len() != 2
    {
        return None;
    }
    let set_ok = variant_arm(set_arms, "Ok")?;
    let set_err = variant_arm(set_arms, "Err")?;
    let (populated_root, set_ok_pattern) =
        payload_binding(set_ok, "Ok", &set_call_expr.ty, root_type, body)?;
    let (set_status, _) = payload_binding(set_err, "Err", &set_call_expr.ty, &i32_type, body)?;
    if populated_root == initial_root
        || body.locals.get(populated_root)?.mutable
        || body.locals.get(populated_root)?.ty != *root_type
        || body.locals.get(set_status)?.mutable
    {
        return None;
    }

    let set_err_block = block(set_err)?;
    let [set_err_drop_stmt] = set_err_block.statements.as_slice() else {
        return None;
    };
    if !direct_load(block_result(set_err)?, set_status) {
        return None;
    }
    let set_err_drop = direct_drop(
        set_err_drop_stmt,
        DropOwner::MapRoot,
        initial_root,
        &new_ok_pattern.node_id,
        &new_ok_pattern.ty,
    )?;

    let success = block(set_ok)?;
    let [snapshot_stmt, index_stmt, unpack_stmt, snapshot_drop_stmt, initial_drop_stmt, populated_drop_stmt] =
        success.statements.as_slice()
    else {
        return None;
    };
    let (snapshot_local, _, values_expr) = immutable_binding(snapshot_stmt, body)?;
    let values_call = call(values_expr, "values")?;
    let [values_source] = values_call.arguments.as_slice() else {
        return None;
    };
    if !values_call.type_arguments.is_empty()
        || values_call.permission.is_some()
        || !values_call.effects.is_empty()
        || !values_call.session.is_empty()
        || values_call.result != values_expr.ty
        || !super::ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(values_expr, values_call)
        || !direct_load(&values_source.value, populated_root)
        || values_source.value.ty != *root_type
        || !exact_identity_argument(values_source)
    {
        return None;
    }
    let ResolvedType::Nominal {
        item: list_item,
        arguments: list_args,
        ..
    } = types.get(&values_expr.ty)?
    else {
        return None;
    };
    if list_item.as_str() != "builtin:type:List"
        || list_args.len() != 1
        || !matches!(
            types.get(&list_args[0]),
            Some(ResolvedType::DynamicAny { capability })
                if capability == "type.dynamic_value"
        )
    {
        return None;
    }
    let snapshot_type = values_expr.ty.clone();

    let (index_local, _, index_expr) = immutable_binding(index_stmt, body)?;
    if !is_i32(types, &index_expr.ty)
        || !matches!(
            &index_expr.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
        )
        || body.locals.get(index_local)?.ty != i32_type
    {
        return None;
    }

    let (unpack_local, _, unpack_expr) = immutable_binding(unpack_stmt, body)?;
    let ResolvedExprKind::Cast {
        value: cast_value,
        conversion,
    } = &unpack_expr.kind
    else {
        return None;
    };
    let ResolvedExprKind::Project {
        value: snapshot_load,
        projection: ResolvedValueProjection::Index(index_load),
    } = &cast_value.kind
    else {
        return None;
    };
    let any_type = list_args[0].clone();
    if conversion.kind != CheckedConversionKind::DynamicAnyUnpack
        || conversion.from != any_type
        || conversion.to != i32_type
        || unpack_expr.ty != i32_type
        || cast_value.ty != any_type
        || !direct_load(snapshot_load, snapshot_local)
        || snapshot_load.ty != snapshot_type
        || !direct_load(index_load, index_local)
        || index_load.ty != i32_type
        || body.locals.get(unpack_local)?.ty != i32_type
    {
        return None;
    }
    let index_census = super::ordinary_map_owned_any_v2_local_source_uses(body, index_local);
    if index_census.has_nested_callable
        || !index_census.captures.is_empty()
        || !index_census.has_unique_load_nodes()
        || index_census.loads.len() != 1
        || index_census.loads[0].node != index_load.node_id
        || index_census.loads[0].direct_field.is_some()
        || !index_census.loads[0].unprojected
        || !index_census.places.is_empty()
    {
        return None;
    }
    let snapshot_census = super::ordinary_map_owned_any_v2_local_source_uses(body, snapshot_local);
    if snapshot_census.has_nested_callable
        || !snapshot_census.captures.is_empty()
        || !snapshot_census.has_unique_load_nodes()
        || snapshot_census.loads.len() != 1
        || snapshot_census.loads[0].node != snapshot_load.node_id
        || !snapshot_census.loads[0].unprojected
        || snapshot_census.places.len() != 1
        || snapshot_census.places[0].statement != snapshot_drop_stmt.node_id
        || snapshot_census.places[0].place_index != 0
        || !snapshot_census.places[0].unprojected
    {
        return None;
    }
    if !direct_load(success.result.as_deref()?, unpack_local) {
        return None;
    }

    let snapshot_drop = direct_drop(
        snapshot_drop_stmt,
        DropOwner::ValuesSnapshot,
        snapshot_local,
        &snapshot_stmt.node_id,
        &snapshot_type,
    )?;
    let initial_success_drop = direct_drop(
        initial_drop_stmt,
        DropOwner::MapRoot,
        initial_root,
        &new_ok_pattern.node_id,
        &new_ok_pattern.ty,
    )?;
    let populated_success_drop = direct_drop(
        populated_drop_stmt,
        DropOwner::MapRoot,
        populated_root,
        &set_ok_pattern.node_id,
        &set_ok_pattern.ty,
    )?;
    let success_result = success.result.as_deref()?;

    let success_order = vec![
        set_ok.pattern.node_id.clone(),
        values_source.value.node_id.clone(),
        values_expr.node_id.clone(),
        snapshot_stmt.node_id.clone(),
        index_expr.node_id.clone(),
        index_stmt.node_id.clone(),
        snapshot_load.node_id.clone(),
        index_load.node_id.clone(),
        cast_value.node_id.clone(),
        unpack_expr.node_id.clone(),
        unpack_stmt.node_id.clone(),
        snapshot_drop_stmt.node_id.clone(),
        initial_drop_stmt.node_id.clone(),
        populated_drop_stmt.node_id.clone(),
        success_result.node_id.clone(),
    ];
    let success_kinds = vec![
        (set_ok.pattern.node_id.clone(), CfgPointKind::Binding),
        (
            values_source.value.node_id.clone(),
            CfgPointKind::Expression,
        ),
        (values_expr.node_id.clone(), CfgPointKind::Expression),
        (snapshot_stmt.node_id.clone(), CfgPointKind::Binding),
        (index_expr.node_id.clone(), CfgPointKind::Expression),
        (index_stmt.node_id.clone(), CfgPointKind::Binding),
        (snapshot_load.node_id.clone(), CfgPointKind::Expression),
        (index_load.node_id.clone(), CfgPointKind::Expression),
        (cast_value.node_id.clone(), CfgPointKind::Expression),
        (unpack_expr.node_id.clone(), CfgPointKind::Expression),
        (unpack_stmt.node_id.clone(), CfgPointKind::Binding),
        (
            snapshot_drop_stmt.node_id.clone(),
            CfgPointKind::ResourceAction,
        ),
        (
            initial_drop_stmt.node_id.clone(),
            CfgPointKind::ResourceAction,
        ),
        (
            populated_drop_stmt.node_id.clone(),
            CfgPointKind::ResourceAction,
        ),
        (success_result.node_id.clone(), CfgPointKind::Expression),
    ];
    let set_order = vec![
        new_ok.pattern.node_id.clone(),
        set_source.value.node_id.clone(),
        set_key.value.node_id.clone(),
        set_payload.value.node_id.clone(),
        set_call_expr.node_id.clone(),
    ];
    let set_kinds = vec![
        (new_ok.pattern.node_id.clone(), CfgPointKind::Binding),
        (set_source.value.node_id.clone(), CfgPointKind::Expression),
        (set_key.value.node_id.clone(), CfgPointKind::Expression),
        (set_payload.value.node_id.clone(), CfgPointKind::Expression),
        (set_call_expr.node_id.clone(), CfgPointKind::Condition),
    ];
    let new_err_result = block_result(new_err)?.node_id.clone();
    let set_err_result = block_result(set_err)?.node_id.clone();
    let cfg_receipt = exact_cfg(
        cfg,
        body,
        outer_match,
        set_match_expr,
        new_call_expr,
        set_call_expr,
        new_ok,
        new_err,
        set_ok,
        set_err,
        &set_err_drop,
        &[
            &snapshot_drop,
            &initial_success_drop,
            &populated_success_drop,
        ],
        &new_err_result,
        &set_err_result,
        &success_result.node_id,
        &set_order,
        &set_kinds,
        &success_order,
        &success_kinds,
    )?;

    let mut actions = Vec::new();
    let mut new_action = OrdinaryMapOwnedAnyV2Checker::build_action(
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
    actions.push(new_action);

    let mut set_action = OrdinaryMapOwnedAnyV2Checker::build_action(
        OrdinaryMapOwnedAnyV2ActionKind::Set,
        set_call_expr.node_id.clone(),
        populated_root.clone(),
        Some(set_key.value.node_id.clone()),
        Some(set_key.value.ty.clone()),
        Some(set_payload.value.node_id.clone()),
        Some(set_payload.value.ty.clone()),
        true,
        Some(initial_root.clone()),
    );
    set_action.source_node = Some(set_source.value.node_id.clone());
    set_action.source_type = Some(set_source.value.ty.clone());
    set_action.key_provenance =
        Some(crate::core::OrdinaryMapOwnedAnyV2KeyProvenance::StringLiteral);
    set_action.key_static_digest = Some(blake3::hash(b"answer").to_hex().to_string());
    actions.push(set_action);

    let mut values_action = OrdinaryMapOwnedAnyV2Checker::build_action(
        OrdinaryMapOwnedAnyV2ActionKind::Values,
        values_expr.node_id.clone(),
        populated_root.clone(),
        None,
        None,
        None,
        None,
        false,
        Some(populated_root.clone()),
    );
    values_action.source_node = Some(values_source.value.node_id.clone());
    values_action.source_type = Some(root_type.clone());
    values_action.result_type = Some(snapshot_type.clone());
    values_action.result_local = Some(snapshot_local.clone());
    actions.push(values_action);

    let map_root_drops = [
        set_err_drop.clone(),
        initial_success_drop.clone(),
        populated_success_drop.clone(),
    ];
    for drop in &map_root_drops {
        actions.push(OrdinaryMapOwnedAnyV2Checker::build_action(
            OrdinaryMapOwnedAnyV2ActionKind::Drop,
            drop.settlement_point.clone(),
            drop.local.clone(),
            None,
            None,
            None,
            None,
            false,
            None,
        ));
    }

    let mut attempts = BTreeSet::new();
    attempts.insert(attempt(
        OrdinaryMapOwnedAnyV2AttemptKind::New,
        &new_call_expr.node_id,
        &[],
        None,
        Some(initial_root),
    ));
    attempts.insert(attempt(
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
    attempts.insert(attempt(
        OrdinaryMapOwnedAnyV2AttemptKind::Values,
        &values_expr.node_id,
        std::slice::from_ref(&values_source.value.node_id),
        Some(populated_root),
        Some(snapshot_local),
    ));
    for drop in &map_root_drops {
        attempts.insert(attempt(
            OrdinaryMapOwnedAnyV2AttemptKind::Drop,
            &drop.settlement_point,
            std::slice::from_ref(&drop.local.0),
            Some(&drop.local),
            None,
        ));
    }
    if actions.len() != 6 || attempts.len() != 6 {
        return None;
    }

    // The owning CFG receipt stores actions in deterministic CFG-point order.
    // Replay must use the same order so the operation ledger cannot be
    // reshuffled independently of branch reachability.
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
    actions.sort_by_key(|action| {
        point_rank
            .get(&action.point)
            .copied()
            .unwrap_or((usize::MAX, usize::MAX))
    });
    if actions
        .iter()
        .any(|action| !point_rank.contains_key(&action.point))
    {
        return None;
    }

    let root_loads = vec![
        (set_source.value.node_id.clone(), initial_root.clone()),
        (values_source.value.node_id.clone(), populated_root.clone()),
    ];
    let ordered_action_points = actions.iter().map(|action| action.point.clone()).collect();
    let paths = vec![
        BranchPath {
            arm_path: vec![new_err.node_id.clone()],
            result: block_result(new_err)?.node_id.clone(),
            kind: PathKind::MapNewErr,
            drops: Vec::new(),
        },
        BranchPath {
            arm_path: vec![new_ok.node_id.clone(), set_err.node_id.clone()],
            result: block_result(set_err)?.node_id.clone(),
            kind: PathKind::MapSetErr,
            drops: vec![set_err_drop],
        },
        BranchPath {
            arm_path: vec![new_ok.node_id.clone(), set_ok.node_id.clone()],
            result: success_result.node_id.clone(),
            kind: PathKind::Success,
            drops: vec![
                snapshot_drop.clone(),
                initial_success_drop,
                populated_success_drop,
            ],
        },
    ];

    Some(Collected {
        receipt: BranchReceipt {
            profile_version: ProfileVersion::V2,
            owner: body.owner.clone(),
            map_new_operation: new_call_expr.node_id.clone(),
            map_new_match: outer_match.node_id.clone(),
            map_new_ok_arm: new_ok.node_id.clone(),
            map_new_err_arm: new_err.node_id.clone(),
            map_new_ok_pattern: new_ok.pattern.node_id.clone(),
            map_new_err_pattern: new_err.pattern.node_id.clone(),
            map_new_ok_body: new_ok.body.node_id.clone(),
            map_new_err_body: new_err.body.node_id.clone(),
            map_new_error_status: new_status.clone(),
            initial_root: initial_root.clone(),
            map_set_operation: set_call_expr.node_id.clone(),
            map_set_match: set_match_expr.node_id.clone(),
            map_set_ok_arm: set_ok.node_id.clone(),
            map_set_err_arm: set_err.node_id.clone(),
            map_set_ok_pattern: set_ok.pattern.node_id.clone(),
            map_set_err_pattern: set_err.pattern.node_id.clone(),
            map_set_ok_body: set_ok.body.node_id.clone(),
            map_set_err_body: set_err.body.node_id.clone(),
            map_set_error_status: set_status.clone(),
            populated_root: populated_root.clone(),
            key_node: set_key.value.node_id.clone(),
            key_literal: "answer".to_string(),
            payload_node: set_payload.value.node_id.clone(),
            payload_type: set_payload.value.ty.clone(),
            payload_literal: 41,
            values_operation: values_expr.node_id.clone(),
            snapshot_binding_point: snapshot_stmt.node_id.clone(),
            snapshot_local: snapshot_local.clone(),
            snapshot_type,
            index_binding_point: index_stmt.node_id.clone(),
            index_local: index_local.clone(),
            index_initializer: index_expr.node_id.clone(),
            index_expression: index_load.node_id.clone(),
            snapshot_index_node: snapshot_load.node_id.clone(),
            unpack_binding_point: unpack_stmt.node_id.clone(),
            unpack_local: unpack_local.clone(),
            unpack_cast: unpack_expr.node_id.clone(),
            snapshot_drop: snapshot_drop.settlement_point,
            result: success_result.node_id.clone(),
            paths,
            ordered_action_points,
            root_loads: root_loads.clone(),
            cfg: cfg_receipt,
        },
        actions,
        attempts,
        root_loads,
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
        "ValuesLocalIndexI32 Result-branch source or CFG no longer matches its closed profile"
            .to_string()
    })?;
    if &collected.receipt != receipt {
        return Err("ValuesLocalIndexI32 Result-branch receipt differs from Checker replay".into());
    }
    if collected.actions != actions || collected.attempts != *attempts {
        return Err(
            "ValuesLocalIndexI32 Result-branch actions or attempts differ from Checker replay"
                .into(),
        );
    }
    Ok(())
}
