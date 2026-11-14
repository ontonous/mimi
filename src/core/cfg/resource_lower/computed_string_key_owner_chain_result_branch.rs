//! Exact Checker authority for the computed String-key Map owner-chain
//! fixture whose `map_new` and `map_set` results are consumed by nested
//! `Result` matches. This is intentionally a single closed source shape;
//! unsupported Result topology remains outside the owner-v2 receipt.

use super::*;
use crate::core::cfg::{EdgeKind, Terminator};
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainBranchDrop as BranchDrop,
    OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchProfileVersion as ProfileVersion,
    OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchReceipt as BranchReceipt,
    OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultPath as BranchPath,
    OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultPathKind as PathKind,
};
use crate::core::{PrimitiveType, ResolvedType};

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

fn variant_arm<'a>(arms: &'a [MatchArm], name: &str) -> Option<&'a MatchArm> {
    let expected = format!("builtin:variant:Result::{name}");
    let mut matches = arms.iter().filter(|arm| {
        arm.guard.is_none()
            && matches!(&arm.pattern.kind, ResolvedPatternKind::Constructor { variant, .. }
                if variant.0 == expected)
    });
    let arm = matches.next()?;
    matches.next().is_none().then_some(arm)
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

fn direct_load(expression: &ResolvedExpr, local: &ResolvedLocalId) -> bool {
    matches!(&expression.kind, ResolvedExprKind::Load(place)
        if place.base == *local && place.projections.is_empty())
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

fn exact_cfg(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    outer_call: &ResolvedExpr,
    inner_call: &ResolvedExpr,
    outer_arms: &BTreeSet<NodeId>,
    inner_arms: &BTreeSet<NodeId>,
    root_result: &ResolvedExpr,
) -> Option<ManagedAnyCfgReceipt> {
    if cfg.owner != body.owner
        || crate::core::cfg::validate_cfg(cfg).is_err()
        || cfg.reachable.len() != cfg.blocks.len()
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != cfg.reachable
    {
        return None;
    }
    let mut source_nodes = root_result
        .eager_source_nodes()
        .into_iter()
        .collect::<BTreeSet<_>>();
    source_nodes.insert(body.root.node_id.clone());
    // The fixture binds its computed key and payload before the outer Match.
    // ResolvedBlock has no eager-source visitor, so close these root-level
    // bindings explicitly and fail closed on any other pre-match statement.
    for statement in &body.root.statements {
        let ResolvedStmtKind::Bind {
            pattern,
            initializer: Some(initializer),
        } = &statement.kind
        else {
            return None;
        };
        source_nodes.insert(statement.node_id.clone());
        source_nodes.insert(pattern.node_id.clone());
        source_nodes.extend(initializer.eager_source_nodes());
    }
    let mut match_scrutinees = BTreeSet::new();
    let mut match_counts = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();
    let mut point_nodes = BTreeSet::new();
    let mut accounted_edges = BTreeSet::new();
    let mut returns = 0usize;
    for (block_id, block) in &cfg.blocks {
        match &block.terminator {
            Terminator::Match { scrutinee, arms } => {
                let expected = if scrutinee == &outer_call.node_id {
                    outer_arms
                } else if scrutinee == &inner_call.node_id {
                    inner_arms
                } else {
                    return None;
                };
                if !match_scrutinees.insert(scrutinee.clone()) || arms.len() != 2 {
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
                        || !accounted_edges.insert(edge_id.clone())
                        || !expected.contains(&edge.source.node)
                        || !observed.insert(edge.source.node.clone())
                    {
                        return None;
                    }
                }
                if &observed != expected {
                    return None;
                }
                match_counts.insert(scrutinee.clone(), observed);
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
    if match_scrutinees != BTreeSet::from([outer_call.node_id.clone(), inner_call.node_id.clone()])
        || returns != 1
        || match_counts.len() != 2
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
    if cfg.owner != body.owner
        || signature.owner != body.owner
        || !signature.parameters.is_empty()
        || !signature.generic_parameters.is_empty()
        || !body.parameters.is_empty()
        || !body.captures.is_empty()
        || !body.place_inputs.is_empty()
        || !body.default_values.is_empty()
    {
        return None;
    }
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let outer_result = body.root.result.as_deref()?;
    let (new_call_expr, outer_arms) = match_expression(outer_result)?;
    let new_call = call(new_call_expr, "map_new")?;
    let (root_type, new_error_type) = result_parts(types, &new_call_expr.ty)?;
    if !new_call.arguments.is_empty()
        || !new_call.type_arguments.is_empty()
        || new_call.permission.is_some()
        || !new_call.session.is_empty()
        || new_call.result != new_call_expr.ty
        || !super::ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(new_call_expr, new_call)
        || !is_map_root(types, root_type)
        || new_error_type != &i32_type
        || outer_arms.len() != 2
    {
        return None;
    }
    let new_ok = variant_arm(outer_arms, "Ok")?;
    let new_err = variant_arm(outer_arms, "Err")?;
    let (initial_root, new_ok_pattern) =
        payload_binding(new_ok, "Ok", &new_call_expr.ty, root_type, body)?;
    let (new_status, _new_err_pattern) =
        payload_binding(new_err, "Err", &new_call_expr.ty, &i32_type, body)?;
    if body.locals.get(initial_root)?.mutable
        || body.locals.get(initial_root)?.ty != *root_type
        || !block(new_ok)?.statements.is_empty()
        || block(new_err)?.statements.len() != 0
        || !direct_load(block_result(new_err)?, new_status)
    {
        return None;
    }
    let (set_match_expr, inner_arms) = match_expression(block_result(new_ok)?)?;
    let set_call = call(set_match_expr, "map_set")?;
    let (set_root_type, set_error_type) = result_parts(types, &set_match_expr.ty)?;
    let [set_source, set_key, set_value] = set_call.arguments.as_slice() else {
        return None;
    };
    if !set_call.type_arguments.is_empty()
        || set_call.permission.is_some()
        || !set_call.session.is_empty()
        || set_call.result != set_match_expr.ty
        || !super::ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(set_match_expr, set_call)
        || set_error_type != &i32_type
        || set_root_type != root_type
        || !is_map_root(types, &set_source.value.ty)
        || set_source.value.ty != *root_type
        || !direct_load(&set_source.value, initial_root)
        || set_source.conversion.kind != crate::core::ir::CheckedConversionKind::Identity
        || set_key.conversion.kind != crate::core::ir::CheckedConversionKind::Identity
        // The Map builtin itself accepts a concrete payload and erases it at
        // the runtime boundary; unlike a normal Any-typed parameter, its
        // ResolvedCall argument conversion remains identity. Bind the proof
        // to that exact concrete type instead of requiring a synthetic pack
        // conversion that the Checker does not produce for this builtin.
        || set_value.conversion.kind != crate::core::ir::CheckedConversionKind::Identity
        || set_value.conversion.from != set_value.value.ty
        || set_value.conversion.to != set_value.value.ty
        || !matches!(
            types.get(&set_key.value.ty),
            Some(ResolvedType::Primitive(PrimitiveType::String))
        )
        || !matches!(
            &set_key.value.kind,
            ResolvedExprKind::Binary {
                op: ResolvedBinaryOp::Add,
                ..
            }
        )
        || !matches!(&set_value.value.kind, ResolvedExprKind::Load(place) if place.projections.is_empty())
        || !matches!(types.get(&set_value.value.ty), Some(ResolvedType::Nominal { item, arguments, .. }) if !item.as_str().starts_with("builtin:type:") && arguments.is_empty())
        || inner_arms.len() != 2
    {
        return None;
    }
    let set_ok = variant_arm(inner_arms, "Ok")?;
    let set_err = variant_arm(inner_arms, "Err")?;
    let (populated_root, set_ok_pattern) =
        payload_binding(set_ok, "Ok", &set_match_expr.ty, root_type, body)?;
    let (set_status, _set_err_pattern) =
        payload_binding(set_err, "Err", &set_match_expr.ty, &i32_type, body)?;
    if populated_root == initial_root
        || body.locals.get(populated_root)?.mutable
        || body.locals.get(populated_root)?.ty != *root_type
        || body.locals.get(set_status)?.mutable
        || block(set_ok)?.statements.is_empty()
        || block(set_err)?.statements.len() != 1
        || !direct_load(block_result(set_err)?, set_status)
    {
        return None;
    }
    let error_block = block(set_err)?;
    let error_drop = direct_drop(
        &error_block.statements[0],
        initial_root,
        &new_ok_pattern.node_id,
        &new_ok_pattern.ty,
    )?;
    let success_block = block(set_ok)?;
    let removed_action = success_block.statements.iter().find_map(|statement| {
        let ResolvedStmtKind::Bind {
            pattern: ResolvedPattern {
                kind: ResolvedPatternKind::Binding { local, .. },
                ..
            },
            initializer: Some(initializer),
        } = &statement.kind
        else {
            return None;
        };
        matches!(&initializer.kind, ResolvedExprKind::Call(call)
            if matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "map_remove"))
            .then_some((local.clone(), statement.node_id.clone()))
    })?;
    let removed_root = removed_action.0;
    if body.locals.get(&removed_root)?.mutable || body.locals.get(&removed_root)?.ty != *root_type {
        return None;
    }
    let result_local_roots = BTreeSet::from([
        initial_root.clone(),
        populated_root.clone(),
        removed_root.clone(),
    ]);
    let success_root_drops = success_block
        .statements
        .iter()
        .filter_map(|statement| match &statement.kind {
            ResolvedStmtKind::Drop(places)
                if places
                    .iter()
                    .any(|place| result_local_roots.contains(&place.base)) =>
            {
                let [place] = places.as_slice() else {
                    return None;
                };
                (place.projections.is_empty() && result_local_roots.contains(&place.base))
                    .then(|| (statement, place.base.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let success_drop_locals = success_root_drops
        .iter()
        .map(|(_, local)| local.clone())
        .collect::<Vec<_>>();
    if success_drop_locals.len() != 3
        || success_drop_locals
            .iter()
            .filter(|local| **local == *initial_root)
            .count()
            != 1
        || success_drop_locals
            .iter()
            .filter(|local| **local == *populated_root)
            .count()
            != 1
        || success_drop_locals
            .iter()
            .filter(|local| **local == removed_root)
            .count()
            != 1
    {
        return None;
    }
    let success_drops = success_root_drops
        .iter()
        .map(|(statement, local)| {
            let binding = if local == initial_root {
                new_ok_pattern
            } else if local == populated_root {
                set_ok_pattern
            } else if local == &removed_root {
                success_block
                    .statements
                    .iter()
                    .find_map(|statement| match &statement.kind {
                        ResolvedStmtKind::Bind {
                            pattern,
                            initializer: Some(initializer),
                        } if statement.node_id == removed_action.1 => match &pattern.kind {
                            ResolvedPatternKind::Binding { local, .. }
                                if local == &removed_root =>
                            {
                                Some(pattern)
                            }
                            _ => None,
                        },
                        _ => None,
                    })?
            } else {
                return None;
            };
            direct_drop(statement, local, &binding.node_id, &binding.ty)
        })
        .collect::<Option<Vec<_>>>()?;

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
    checker.operation_attempts.insert(attempt_from_call(
        OrdinaryMapOwnedAnyV2AttemptKind::New,
        &new_call_expr.node_id,
        &[],
        None,
        None,
    ));

    checker
        .allowed_root_loads
        .insert(set_source.value.node_id.clone());
    checker
        .all_root_loads
        .push((set_source.value.node_id.clone(), initial_root.clone()));
    let mut set_action = super::OrdinaryMapOwnedAnyV2Checker::build_action(
        OrdinaryMapOwnedAnyV2ActionKind::Set,
        set_match_expr.node_id.clone(),
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
    checker.operation_attempts.insert(attempt_from_call(
        OrdinaryMapOwnedAnyV2AttemptKind::Set,
        &set_match_expr.node_id,
        &set_call
            .arguments
            .iter()
            .map(|arg| arg.value.node_id.clone())
            .collect::<Vec<_>>(),
        Some(initial_root),
        Some(populated_root),
    ));

    // Analyze only the successful Set path with its own live-root state.
    // The Err path receives a separate Drop event below; no mutable state is
    // shared across the mutually exclusive arms.
    checker.visit_block(success_block);
    if checker.invalid {
        return None;
    }
    for drop in std::iter::once(error_drop.clone()) {
        let action = super::OrdinaryMapOwnedAnyV2Checker::build_action(
            OrdinaryMapOwnedAnyV2ActionKind::Drop,
            drop.settlement_point.clone(),
            drop.local.clone(),
            None,
            None,
            None,
            None,
            false,
            None,
        );
        checker.actions.push(action);
        checker.operation_attempts.insert(attempt_from_call(
            OrdinaryMapOwnedAnyV2AttemptKind::Drop,
            &drop.settlement_point,
            std::slice::from_ref(&drop.local.0),
            Some(&drop.local),
            None,
        ));
    }
    let expected_action_kinds = [
        (OrdinaryMapOwnedAnyV2ActionKind::New, 1),
        (OrdinaryMapOwnedAnyV2ActionKind::Set, 1),
        (OrdinaryMapOwnedAnyV2ActionKind::HasKey, 3),
        (OrdinaryMapOwnedAnyV2ActionKind::GetAny, 1),
        (OrdinaryMapOwnedAnyV2ActionKind::Remove, 1),
        (OrdinaryMapOwnedAnyV2ActionKind::Size, 1),
        (OrdinaryMapOwnedAnyV2ActionKind::Drop, 4),
    ];
    if checker.actions.len() != 12
        || checker.operation_attempts.len() != 12
        || expected_action_kinds.iter().any(|(kind, count)| {
            checker
                .actions
                .iter()
                .filter(|action| action.kind == *kind)
                .count()
                != *count
        })
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
                            OrdinaryMapOwnedAnyV2ActionKind::HasKey,
                            OrdinaryMapOwnedAnyV2AttemptKind::HasKey,
                        ) | (
                            OrdinaryMapOwnedAnyV2ActionKind::GetAny,
                            OrdinaryMapOwnedAnyV2AttemptKind::GetAny,
                        ) | (
                            OrdinaryMapOwnedAnyV2ActionKind::Remove,
                            OrdinaryMapOwnedAnyV2AttemptKind::Remove,
                        ) | (
                            OrdinaryMapOwnedAnyV2ActionKind::Size,
                            OrdinaryMapOwnedAnyV2AttemptKind::Size,
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
    let root_locals = BTreeSet::from([
        initial_root.clone(),
        populated_root.clone(),
        removed_root.clone(),
    ]);
    let root_drops = std::iter::once(error_drop.clone())
        .chain(success_drops.clone())
        .collect::<Vec<_>>();
    if root_drops.len() != 4
        || root_drops
            .iter()
            .any(|drop| !root_locals.contains(&drop.local))
        || root_drops
            .iter()
            .map(|drop| drop.settlement_point.clone())
            .collect::<BTreeSet<_>>()
            .len()
            != 4
    {
        return None;
    }
    checker.roots = root_locals
        .iter()
        .cloned()
        .map(|local| (local, true))
        .collect();
    let outer_arm_ids = BTreeSet::from([new_ok.node_id.clone(), new_err.node_id.clone()]);
    let inner_arm_ids = BTreeSet::from([set_ok.node_id.clone(), set_err.node_id.clone()]);
    let cfg_receipt = exact_cfg(
        cfg,
        body,
        new_call_expr,
        set_match_expr,
        &outer_arm_ids,
        &inner_arm_ids,
        outer_result,
    )?;
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
            drops: vec![error_drop],
        },
        BranchPath {
            arm_path: vec![new_ok.node_id.clone(), set_ok.node_id.clone()],
            result: success_block.result.as_deref()?.node_id.clone(),
            kind: PathKind::Success,
            drops: success_drops,
        },
    ];
    let ordered_action_points = checker
        .actions
        .iter()
        .map(|action| action.point.clone())
        .collect();
    Some(Collected {
        receipt: BranchReceipt {
            profile_version: ProfileVersion::V1,
            owner: body.owner.clone(),
            map_new_operation: new_call_expr.node_id.clone(),
            map_new_match: outer_result.node_id.clone(),
            map_new_ok_arm: new_ok.node_id.clone(),
            map_new_err_arm: new_err.node_id.clone(),
            initial_root: initial_root.clone(),
            map_set_operation: set_match_expr.node_id.clone(),
            map_set_match: block_result(new_ok)?.node_id.clone(),
            map_set_ok_arm: set_ok.node_id.clone(),
            map_set_err_arm: set_err.node_id.clone(),
            populated_root: populated_root.clone(),
            paths,
            ordered_action_points,
            cfg: cfg_receipt,
        },
        actions: checker.actions,
        attempts: checker.operation_attempts,
        root_loads: checker.all_root_loads,
    })
}

fn attempt_from_call(
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
