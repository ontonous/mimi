//! Exact Checker source authority for the ordinary Map/Any V2 Set lineage.
//!
//! This profile is intentionally separate from NewResult: it proves two
//! Result dispatches and the persistent root transfer performed by map_set.

use super::*;
use crate::core::cfg::{BasicBlockId, CfgPoint, CfgPointKind, EdgeId};
use crate::core::ir::{CheckedConversionKind, PrimitiveType};
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2SetLineageProfileVersion as Version,
    OrdinaryMapOwnedAnyV2SetLineageReceipt as Receipt,
};
use crate::core::{LocalId, Place, ResourceId};

fn local_name(body: &ResolvedBody, local: &ResolvedLocalId) -> String {
    body.locals
        .get(local)
        .map(|b| b.display_name.clone())
        .unwrap_or_else(|| local.0 .0.clone())
}
fn root_place(body: &ResolvedBody, local: &ResolvedLocalId) -> Place {
    Place::root(LocalId(local.0.clone()), local_name(body, local))
}
fn resource_identity(local: &ResolvedLocalId) -> ResourceId {
    ResourceId(NodeId(format!(
        "ordinary-map-owned-any-v2-set-lineage-v1:{}",
        local.0 .0
    )))
}
fn immutable_binding<'a>(
    statement: &'a ResolvedStmt,
    body: &ResolvedBody,
) -> Option<(&'a ResolvedLocalId, &'a ResolvedExpr)> {
    let ResolvedStmtKind::Bind {
        pattern,
        initializer: Some(expr),
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
    (!binding.mutable && binding.ty == pattern.ty && binding.ty == expr.ty).then_some((local, expr))
}
fn call<'a>(expr: &'a ResolvedExpr, name: &str, argc: usize) -> Option<&'a ResolvedCall> {
    let ResolvedExprKind::Call(call) = &expr.kind else {
        return None;
    };
    (matches!(&call.callee, ResolvedCallee::Builtin(b) if b.as_str() == name)
        && call.arguments.len() == argc
        && call.result == expr.ty
        && call.type_arguments.is_empty()
        && call.permission.is_none()
        && call.session.is_empty())
    .then_some(call)
}
fn load(expr: &ResolvedExpr, local: &ResolvedLocalId, ty: &ResolvedTypeId) -> bool {
    expr.ty == *ty
        && matches!(&expr.kind, ResolvedExprKind::Load(place) if place.base == *local && place.projections.is_empty())
}
fn variant_arm<'a>(arms: &'a [MatchArm], name: &str) -> Option<&'a MatchArm> {
    let expected = format!("builtin:variant:Result::{name}");
    let mut matches = arms.iter().filter(|a| matches!(&a.pattern.kind, ResolvedPatternKind::Constructor { variant, .. } if variant.0 == expected));
    let arm = matches.next()?;
    matches.next().is_none().then_some(arm)
}
fn payload<'a>(
    arm: &'a MatchArm,
    name: &str,
    result: &ResolvedTypeId,
    ty: &ResolvedTypeId,
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
    (variant.0 == format!("builtin:variant:Result::{name}")
        && field.0 == format!("builtin:variant:Result::{name}/payload:0")
        && arm.pattern.ty == *result
        && pattern.ty == *ty
        && binding.ty == *ty
        && !binding.mutable)
        .then_some((local, pattern))
}
fn drop_local(statement: &ResolvedStmt, local: &ResolvedLocalId) -> bool {
    matches!(&statement.kind, ResolvedStmtKind::Drop(places) if places.len() == 1 && places[0].base == *local && places[0].projections.is_empty())
}
fn edge_for_arm<'a>(
    cfg: &'a CallableCfg,
    dispatch: &BasicBlockId,
    arm: &MatchArm,
) -> Option<(&'a EdgeId, &'a BasicBlockId)> {
    let Terminator::Match { arms, .. } = &cfg.blocks.get(dispatch)?.terminator else {
        return None;
    };
    let mut found = arms.iter().filter_map(|id| {
        let edge = cfg.edges.get(id)?;
        (edge.from == *dispatch
            && edge.kind == EdgeKind::MatchArm
            && edge.source.node == arm.node_id)
            .then_some((id, &edge.to))
    });
    let result = found.next()?;
    found.next().is_none().then_some(result)
}
fn join_edge<'a>(
    cfg: &'a CallableCfg,
    block: &BasicBlockId,
    arm: &MatchArm,
) -> Option<(&'a EdgeId, &'a BasicBlockId)> {
    let Terminator::Goto { edge } = &cfg.blocks.get(block)?.terminator else {
        return None;
    };
    let e = cfg.edges.get(edge)?;
    (e.from == *block && e.kind == EdgeKind::Fallthrough && e.source.node == arm.node_id)
        .then_some((edge, &e.to))
}
fn point_is(
    point: &CfgPoint,
    node: &NodeId,
    origin: &crate::core::Origin,
    kind: CfgPointKind,
    uses: &[String],
    defs: &[String],
    places: &[Place],
) -> bool {
    point.source.node == *node
        && point.source.origin == *origin
        && point.source.span == origin.user_span()
        && point.kind == kind
        && point.uses == uses
        && point.defs == defs
        && point.reads == places.iter().map(Place::display).collect::<Vec<_>>()
        && point.writes.is_empty()
        && point.read_places == places
        && point.write_places.is_empty()
}
fn has_point(
    cfg: &CallableCfg,
    block: &BasicBlockId,
    node: &NodeId,
    origin: &crate::core::Origin,
    kind: CfgPointKind,
    uses: &[String],
    defs: &[String],
    places: &[Place],
) -> bool {
    cfg.blocks.get(block).is_some_and(|b| {
        b.points
            .iter()
            .filter(|actual| actual.source.node == *node)
            .count()
            == 1
            && b.points
                .iter()
                .any(|actual| point_is(actual, node, origin, kind, uses, defs, places))
    })
}
pub(super) fn collect(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
) -> Option<Receipt> {
    let i32_ty = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    if cfg.owner != body.owner
        || signature.owner != body.owner
        || signature.result != i32_ty
        || body.root.ty != i32_ty
        || !signature.generic_parameters.is_empty()
        || !signature.parameters.is_empty()
        || !body.parameters.is_empty()
        || !body.captures.is_empty()
        || !body.place_inputs.is_empty()
        || !body.default_values.is_empty()
    {
        return None;
    }
    let mut leading_contracts = Vec::new();
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
                leading_contracts.push(statement);
            }
            ResolvedStmtKind::Contract { .. } => return None,
            _ => {
                executable_started = true;
                executable.push(statement);
            }
        }
    }
    let [created_stmt] = executable.as_slice() else {
        return None;
    };
    let (created_local, new_expr) = immutable_binding(created_stmt, body)?;
    let new_call = call(new_expr, "map_new", 0)?;
    let ResolvedType::Result { ok: root_ty, error } = types.get(&new_expr.ty)? else {
        return None;
    };
    if error != &i32_ty || !record_type(types, root_ty) || new_call.result != new_expr.ty {
        return None;
    }
    let outer = body.root.result.as_deref()?;
    let ResolvedExprKind::Match {
        scrutinee: outer_scrutinee,
        arms: outer_arms,
    } = &outer.kind
    else {
        return None;
    };
    if outer.ty != i32_ty
        || !load(outer_scrutinee, created_local, &new_expr.ty)
        || outer_arms.len() != 2
        || outer_arms
            .iter()
            .any(|a| a.guard.is_some() || a.body.ty != i32_ty)
    {
        return None;
    }
    let outer_ok = variant_arm(outer_arms, "Ok")?;
    let outer_err = variant_arm(outer_arms, "Err")?;
    let (old_local, old_pattern) = payload(outer_ok, "Ok", &new_expr.ty, root_ty, body)?;
    let (outer_status, outer_status_pattern) =
        payload(outer_err, "Err", &new_expr.ty, &i32_ty, body)?;
    let ResolvedExprKind::Block(ok_block) = &outer_ok.body.kind else {
        return None;
    };
    let [set_stmt] = ok_block.statements.as_slice() else {
        return None;
    };
    let (set_local, set_expr) = immutable_binding(set_stmt, body)?;
    let set = call(set_expr, "map_set", 3)?;
    let [receiver, key, value] = set.arguments.as_slice() else {
        return None;
    };
    if !load(&receiver.value, old_local, root_ty)
        || receiver.conversion.kind != CheckedConversionKind::Identity
        || receiver.conversion.from != *root_ty
        || receiver.conversion.to != *root_ty
    {
        return None;
    }
    if !matches!(&key.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(s)) if s == "answer")
        || key.conversion.kind != CheckedConversionKind::Identity
    {
        return None;
    }
    if !matches!(
        &value.value.kind,
        ResolvedExprKind::Literal(ResolvedLiteral::Int(42))
    ) || value.conversion.kind != CheckedConversionKind::Identity
    {
        return None;
    }
    let ResolvedType::Result {
        ok: updated_ty,
        error: set_error,
    } = types.get(&set_expr.ty)?
    else {
        return None;
    };
    if set_error != &i32_ty || updated_ty != root_ty {
        return None;
    }
    let inner = ok_block.result.as_deref()?;
    let ResolvedExprKind::Match {
        scrutinee: inner_scrutinee,
        arms: inner_arms,
    } = &inner.kind
    else {
        return None;
    };
    if inner_arms.len() != 2
        || inner_arms
            .iter()
            .any(|a| a.guard.is_some() || a.body.ty != i32_ty)
        || !load(inner_scrutinee, set_local, &set_expr.ty)
    {
        return None;
    }
    let inner_ok = variant_arm(inner_arms, "Ok")?;
    let inner_err = variant_arm(inner_arms, "Err")?;
    let (updated_local, updated_pattern) = payload(inner_ok, "Ok", &set_expr.ty, updated_ty, body)?;
    let (inner_status, inner_status_pattern) =
        payload(inner_err, "Err", &set_expr.ty, &i32_ty, body)?;
    let ResolvedExprKind::Block(inner_ok_body) = &inner_ok.body.kind else {
        return None;
    };
    let [drop_old_stmt, size_stmt, drop_updated_stmt] = inner_ok_body.statements.as_slice() else {
        return None;
    };
    if !drop_local(drop_old_stmt, old_local) || !drop_local(drop_updated_stmt, updated_local) {
        return None;
    }
    let (size_local, size_expr) = immutable_binding(size_stmt, body)?;
    let size = call(size_expr, "map_size", 1)?;
    let [size_arg] = size.arguments.as_slice() else {
        return None;
    };
    if size_expr.ty != i32_ty
        || !load(&size_arg.value, updated_local, updated_ty)
        || !load(inner_ok_body.result.as_deref()?, size_local, &i32_ty)
    {
        return None;
    }
    let inner_err_result = direct_arm_result(inner_err)?;
    let ResolvedExprKind::Block(inner_err_body) = &inner_err.body.kind else {
        return None;
    };
    let [drop_error_root_stmt] = inner_err_body.statements.as_slice() else {
        return None;
    };
    if !drop_local(drop_error_root_stmt, old_local) {
        return None;
    }
    if !load(inner_err_result, inner_status, &i32_ty) {
        return None;
    }
    let outer_err_result = empty_block_or_direct_result(outer_err)?;
    if !load(outer_err_result, outer_status, &i32_ty) {
        return None;
    }
    let mut locals = [
        created_local.clone(),
        old_local.clone(),
        outer_status.clone(),
        set_local.clone(),
        updated_local.clone(),
        inner_status.clone(),
        size_local.clone(),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let contract_result =
        ResolvedLocalId(NodeId(format!("{}/contract-result/local", body.owner.0)));
    let has_ensures = leading_contracts.iter().any(|statement| {
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
        if result_local.ty != i32_ty || result_local.mutable {
            return None;
        }
        locals.insert(contract_result);
    } else if body.locals.contains_key(&contract_result) {
        return None;
    }
    if locals.len() != 7 + usize::from(has_ensures)
        || body.locals.keys().cloned().collect::<BTreeSet<_>>() != locals
    {
        return None;
    }
    if cfg.reachable.len() != cfg.blocks.len()
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != cfg.reachable.clone()
        || crate::core::cfg::validate_cfg(cfg).is_err()
    {
        return None;
    }
    let outer_dispatch = cfg.blocks.iter().find_map(|(id,b)| matches!(&b.terminator, Terminator::Match { scrutinee, .. } if scrutinee == &outer_scrutinee.node_id).then_some(id.clone()))?;
    let inner_dispatch = cfg.blocks.iter().find_map(|(id,b)| matches!(&b.terminator, Terminator::Match { scrutinee, .. } if scrutinee == &inner_scrutinee.node_id).then_some(id.clone()))?;
    if outer_dispatch == inner_dispatch || cfg.blocks.len() < 7 {
        return None;
    }
    let (outer_ok_edge, outer_ok_block) = edge_for_arm(cfg, &outer_dispatch, outer_ok)?;
    let (outer_err_edge, outer_err_block) = edge_for_arm(cfg, &outer_dispatch, outer_err)?;
    let (inner_ok_edge, inner_ok_block) = edge_for_arm(cfg, &inner_dispatch, inner_ok)?;
    let (inner_err_edge, inner_err_block) = edge_for_arm(cfg, &inner_dispatch, inner_err)?;
    if inner_dispatch != *outer_ok_block {
        return None;
    }
    let (inner_ok_join_edge, inner_join) = join_edge(cfg, inner_ok_block, inner_ok)?;
    let (inner_err_join_edge, inner_err_join) = join_edge(cfg, inner_err_block, inner_err)?;
    let (outer_err_join_edge, outer_join) = join_edge(cfg, outer_err_block, outer_err)?;
    if inner_join != inner_err_join || inner_join == outer_join {
        return None;
    }
    let (inner_to_outer_join_edge, inner_outer_join) = match &cfg.blocks.get(inner_join)?.terminator
    {
        Terminator::Goto { edge } => {
            let e = cfg.edges.get(edge)?;
            (edge, &e.to)
        }
        _ => return None,
    };
    if inner_outer_join != outer_join
        || cfg.blocks[outer_join].terminator
            != (Terminator::Return {
                value: Some(outer.node_id.clone()),
                implicit: true,
            })
    {
        return None;
    }
    let expected_blocks = BTreeSet::from([
        outer_dispatch.clone(),
        outer_ok_block.clone(),
        outer_err_block.clone(),
        inner_ok_block.clone(),
        inner_err_block.clone(),
        inner_join.clone(),
        outer_join.clone(),
    ]);
    if expected_blocks.len() != 7 {
        return None;
    }
    let expected_edges = BTreeSet::from([
        outer_ok_edge.clone(),
        outer_err_edge.clone(),
        inner_ok_edge.clone(),
        inner_err_edge.clone(),
        inner_ok_join_edge.clone(),
        inner_err_join_edge.clone(),
        inner_to_outer_join_edge.clone(),
        outer_err_join_edge.clone(),
    ]);
    let core_edges = cfg
        .edges
        .values()
        .filter(|edge| expected_blocks.contains(&edge.from))
        .map(|edge| edge.id.clone())
        .collect::<BTreeSet<_>>();
    if core_edges != expected_edges {
        return None;
    }
    let contract_nodes = super::new_result::direct_contract_expression_nodes(body);
    let proof_nodes = contract_nodes.clone();
    if !super::new_result::proof_prelude_is_closed(
        cfg,
        &expected_blocks,
        &outer_dispatch,
        &proof_nodes,
    ) || !super::new_result::proof_nodes_have_exact_point_multiplicity(
        cfg,
        &proof_nodes,
        &contract_nodes,
    ) {
        return None;
    }
    // The AST shape above is closed. Require every checked CFG point to name
    // one of its nodes, and bind the four cleanup points to the exact source
    // Drop statements. No extra statement/expression can hide in a branch.
    let mut source_nodes = body
        .root
        .result
        .as_deref()?
        .eager_source_nodes()
        .into_iter()
        .collect::<BTreeSet<_>>();
    source_nodes.extend(new_expr.eager_source_nodes());
    for arm in outer_arms.iter().chain(inner_arms) {
        source_nodes.insert(arm.node_id.clone());
        source_nodes.insert(arm.pattern.node_id.clone());
        source_nodes.insert(arm.body.node_id.clone());
    }
    source_nodes.insert(created_stmt.node_id.clone());
    source_nodes.insert(match &created_stmt.kind {
        ResolvedStmtKind::Bind { pattern, .. } => pattern.node_id.clone(),
        _ => return None,
    });
    source_nodes.insert(set_stmt.node_id.clone());
    source_nodes.insert(match &set_stmt.kind {
        ResolvedStmtKind::Bind { pattern, .. } => pattern.node_id.clone(),
        _ => return None,
    });
    for statement in ok_block
        .statements
        .iter()
        .chain(inner_ok_body.statements.iter())
    {
        source_nodes.insert(statement.node_id.clone());
    }
    source_nodes.insert(outer.node_id.clone());
    source_nodes.extend(proof_nodes);
    let points = cfg
        .blocks
        .values()
        .flat_map(|b| &b.points)
        .collect::<Vec<_>>();
    let point_nodes = points
        .iter()
        .map(|p| p.source.node.clone())
        .collect::<BTreeSet<_>>();
    if point_nodes.len() != points.len()
        || points
            .iter()
            .any(|p| !source_nodes.contains(&p.source.node))
        || [new_expr, set_expr, size_expr, outer, inner]
            .iter()
            .any(|expr| !points.iter().any(|p| p.source.node == expr.node_id))
        || [created_stmt, set_stmt, size_stmt]
            .iter()
            .any(|statement| !points.iter().any(|p| p.source.node == statement.node_id))
    {
        return None;
    }
    for (block, statement, local) in [
        (inner_ok_block, drop_old_stmt, old_local),
        (inner_ok_block, drop_updated_stmt, updated_local),
        (inner_err_block, drop_error_root_stmt, old_local),
    ] {
        if !cfg.blocks[block].points.iter().any(|p| {
            p.source.node == statement.node_id
                && p.kind == CfgPointKind::ResourceAction
                && p.read_places == vec![root_place(body, local)]
        }) {
            return None;
        }
    }
    let empty_point = |block: &BasicBlockId, expr: &ResolvedExpr, kind| {
        has_point(cfg, block, &expr.node_id, &expr.origin, kind, &[], &[], &[])
    };
    let binding_point =
        |block: &BasicBlockId, statement: &ResolvedStmt, local: &ResolvedLocalId| {
            has_point(
                cfg,
                block,
                &statement.node_id,
                &statement.origin,
                CfgPointKind::Binding,
                &[],
                &[local_name(body, local)],
                &[],
            )
        };
    let pattern_point = |block: &BasicBlockId, arm: &MatchArm, local: &ResolvedLocalId| {
        has_point(
            cfg,
            block,
            &arm.pattern.node_id,
            &arm.pattern.origin,
            CfgPointKind::Binding,
            &[],
            &[local_name(body, local)],
            &[],
        )
    };
    let load_point = |block: &BasicBlockId, expr: &ResolvedExpr, local: &ResolvedLocalId, kind| {
        let place = root_place(body, local);
        let name = place.base_name.clone();
        has_point(
            cfg,
            block,
            &expr.node_id,
            &expr.origin,
            kind,
            &[name],
            &[],
            &[place],
        )
    };
    if !empty_point(&outer_dispatch, new_expr, CfgPointKind::Expression)
        || !binding_point(&outer_dispatch, created_stmt, created_local)
        || !load_point(
            &outer_dispatch,
            outer_scrutinee,
            created_local,
            CfgPointKind::Condition,
        )
        || !pattern_point(outer_ok_block, outer_ok, old_local)
        || !pattern_point(outer_err_block, outer_err, outer_status)
        || !load_point(
            outer_ok_block,
            &receiver.value,
            old_local,
            CfgPointKind::Expression,
        )
        || !empty_point(outer_ok_block, set_expr, CfgPointKind::Expression)
        || !binding_point(outer_ok_block, set_stmt, set_local)
        || !load_point(
            &inner_dispatch,
            inner_scrutinee,
            set_local,
            CfgPointKind::Condition,
        )
        || !pattern_point(inner_ok_block, inner_ok, updated_local)
        || !pattern_point(inner_err_block, inner_err, inner_status)
        || !load_point(
            inner_ok_block,
            &size_arg.value,
            updated_local,
            CfgPointKind::Expression,
        )
        || !empty_point(inner_ok_block, size_expr, CfgPointKind::Expression)
        || !binding_point(inner_ok_block, size_stmt, size_local)
        || !load_point(
            inner_ok_block,
            inner_ok_body.result.as_deref()?,
            size_local,
            CfgPointKind::Expression,
        )
        || !load_point(
            inner_err_block,
            inner_err_result,
            inner_status,
            CfgPointKind::Expression,
        )
        || !load_point(
            outer_err_block,
            outer_err_result,
            outer_status,
            CfgPointKind::Expression,
        )
    {
        return None;
    }
    Some(Receipt {
        profile_version: Version::V1,
        owner: body.owner.clone(),
        new_operation: new_expr.node_id.clone(),
        new_result_binding: created_stmt.node_id.clone(),
        new_result_local: created_local.clone(),
        new_result_type: new_expr.ty.clone(),
        initial_root_type: root_ty.clone(),
        initial_root: resource_identity(old_local),
        outer_match: outer.node_id.clone(),
        outer_scrutinee: outer_scrutinee.node_id.clone(),
        outer_ok_arm: outer_ok.node_id.clone(),
        outer_err_arm: outer_err.node_id.clone(),
        initial_root_pattern: old_pattern.node_id.clone(),
        initial_root_local: old_local.clone(),
        set_operation: set_expr.node_id.clone(),
        set_receiver: receiver.value.node_id.clone(),
        set_result_binding: set_stmt.node_id.clone(),
        set_result_local: set_local.clone(),
        set_result_type: set_expr.ty.clone(),
        updated_root_type: updated_ty.clone(),
        updated_root: resource_identity(updated_local),
        inner_match: inner.node_id.clone(),
        inner_scrutinee: inner_scrutinee.node_id.clone(),
        inner_ok_arm: inner_ok.node_id.clone(),
        inner_err_arm: inner_err.node_id.clone(),
        updated_root_pattern: updated_pattern.node_id.clone(),
        updated_root_local: updated_local.clone(),
        inner_status_pattern: inner_status_pattern.node_id.clone(),
        inner_status_local: inner_status.clone(),
        outer_status_pattern: outer_status_pattern.node_id.clone(),
        outer_status_local: outer_status.clone(),
        old_root_drop: drop_old_stmt.node_id.clone(),
        inner_error_root_drop: drop_error_root_stmt.node_id.clone(),
        size_operation: size_expr.node_id.clone(),
        size_receiver: size_arg.value.node_id.clone(),
        size_binding: size_stmt.node_id.clone(),
        size_local: size_local.clone(),
        updated_root_drop: drop_updated_stmt.node_id.clone(),
        inner_ok_result: inner_ok_body.result.as_deref()?.node_id.clone(),
        inner_err_result: inner_err_result.node_id.clone(),
        outer_err_result: outer_err_result.node_id.clone(),
        entry: outer_dispatch.clone(),
        outer_ok_block: outer_ok_block.clone(),
        outer_err_block: outer_err_block.clone(),
        inner_dispatch_block: inner_dispatch.clone(),
        inner_ok_block: inner_ok_block.clone(),
        inner_err_block: inner_err_block.clone(),
        inner_join_block: inner_join.clone(),
        outer_join_block: outer_join.clone(),
        outer_ok_edge: outer_ok_edge.clone(),
        outer_err_edge: outer_err_edge.clone(),
        inner_ok_edge: inner_ok_edge.clone(),
        inner_err_edge: inner_err_edge.clone(),
        inner_ok_join_edge: inner_ok_join_edge.clone(),
        inner_err_join_edge: inner_err_join_edge.clone(),
        inner_to_outer_join_edge: inner_to_outer_join_edge.clone(),
        outer_err_join_edge: outer_err_join_edge.clone(),
        cfg: ManagedAnyCfgReceipt::from_cfg(cfg, body),
    })
}
fn record_type(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(types.get(ty), Some(ResolvedType::Nominal { item, arguments, is_linear: false }) if item.as_str() == "builtin:type:Record" && arguments.is_empty())
}
fn direct_arm_result(arm: &MatchArm) -> Option<&ResolvedExpr> {
    match &arm.body.kind {
        // Drop statements are checked independently by the exact arm-shape
        // validation. They do not hide or replace a block's tail result.
        ResolvedExprKind::Block(block) => block.result.as_deref(),
        _ => Some(&arm.body),
    }
}

fn empty_block_or_direct_result(arm: &MatchArm) -> Option<&ResolvedExpr> {
    match &arm.body.kind {
        ResolvedExprKind::Block(block) if block.statements.is_empty() => block.result.as_deref(),
        ResolvedExprKind::Block(block) if block.result.is_none() => {
            // Parser normalization wraps a direct expression match arm in a
            // one-expression block. Do not accept statements around it.
            let [statement] = block.statements.as_slice() else {
                return None;
            };
            match &statement.kind {
                ResolvedStmtKind::Expr(expression) => Some(expression),
                _ => None,
            }
        }
        ResolvedExprKind::Block(_) => None,
        _ => Some(&arm.body),
    }
}
pub(super) fn validate(
    receipt: &Receipt,
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
) -> Result<(), String> {
    if collect(cfg, body, signature, types).as_ref() == Some(receipt) {
        Ok(())
    } else {
        Err(
            "ordinary Map Set lineage receipt disagrees with exact Checker source/CFG evidence"
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
        func main() -> i32 {
            let created = map_new()
            match created {
                Ok(root) => {
                    let changed = map_set(root, "answer", 42)
                    match changed {
                        Ok(updated) => {
                            drop(root)
                            let size = map_size(updated)
                            drop(updated)
                            size
                        },
                        Err(status) => {
                            drop(root)
                            status
                        }
                    }
                },
                Err(status) => status
            }
        }
    "#;

    fn checked(source: &str) -> Result<crate::core::CheckedProgram, Vec<Diagnostic>> {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file)
    }

    #[test]
    fn set_lineage_source_receipt_binds_both_result_chains_and_cfg() {
        let program = checked(SOURCE).expect("Set lineage source checks");
        let owner = NodeId("function:main".into());
        let callable = program.callable(&owner).expect("main callable");
        let receipt = callable
            .resources
            .ordinary_map_owned_any_v2_set_lineage_receipt
            .as_ref()
            .expect("Set lineage receipt");
        validate(
            receipt,
            &callable.cfg,
            &callable.body,
            &callable.signature,
            program.resolved_types(),
        )
        .expect("exact replay");
        assert_eq!(
            receipt.profile_version,
            crate::core::ownership::OrdinaryMapOwnedAnyV2SetLineageProfileVersion::V1
        );
        assert_eq!(receipt.new_result_type, receipt.set_result_type);
        assert_ne!(receipt.initial_root_local, receipt.updated_root_local);
        assert_ne!(receipt.old_root_drop, receipt.updated_root_drop);
        assert_ne!(receipt.old_root_drop, receipt.inner_error_root_drop);
        assert_ne!(receipt.updated_root_drop, receipt.inner_error_root_drop);
        assert_eq!(receipt.cfg.reachable_blocks.len(), 7);
        assert_eq!(receipt.cfg.reachable_edges.len(), 8);
        assert!(
            callable
                .resources
                .ordinary_map_owned_any_v2_new_result_receipt
                .is_none(),
            "Set lineage must not be fabricated as a New-only Result receipt"
        );
    }

    #[test]
    fn ordinary_map_set_result_shared_fixture_receives_exact_lineage_and_mutation_withholds_it() {
        let fixture = include_str!(
            "../../../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_result_lineage.mimi"
        );
        let program = checked(fixture).expect("shared SetResult fixture checks");
        let callable = program
            .callable(&NodeId("function:main".into()))
            .expect("main callable");
        let receipt = callable
            .resources
            .ordinary_map_owned_any_v2_set_lineage_receipt
            .as_ref()
            .expect("shared fixture must produce Set lineage authority");
        validate(
            receipt,
            &callable.cfg,
            &callable.body,
            &callable.signature,
            program.resolved_types(),
        )
        .expect("shared fixture receipt replays exactly");
        assert!(
            callable
                .resources
                .ordinary_map_owned_any_v2_new_result_receipt
                .is_none(),
            "the SetResult lineage must remain distinct from NewResult-only authority"
        );

        let mutated = fixture.replace("\"answer\", 42", "\"answer\", 43");
        let changed_program = checked(&mutated).expect("literal mutation remains well typed");
        let changed_callable = changed_program
            .callable(&NodeId("function:main".into()))
            .expect("mutated main callable");
        assert!(
            changed_callable
                .resources
                .ordinary_map_owned_any_v2_set_lineage_receipt
                .is_none(),
            "a changed Set payload must withhold the exact source receipt"
        );
    }

    #[test]
    fn ordinary_map_set_result_rejects_extra_outer_error_action() {
        let fixture = include_str!(
            "../../../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_result_lineage.mimi"
        );
        let mutated = fixture.replace(
            "Err(status) => status,",
            "Err(status) => { println(\"unexpected\")\n status },",
        );
        assert_ne!(mutated, fixture, "the source mutation must be applied");
        let program = checked(&mutated).expect("extra outer Err action remains well typed");
        let callable = program
            .callable(&NodeId("function:main".into()))
            .expect("mutated main callable");
        assert!(
            callable
                .resources
                .ordinary_map_owned_any_v2_set_lineage_receipt
                .is_none(),
            "outer Err must propagate its exact status without preceding side effects"
        );
    }

    #[test]
    fn set_lineage_rejects_missing_extra_or_misplaced_cleanup_and_operations() {
        for (label, source) in [
            ("missing old root cleanup", SOURCE.replace("drop(root)", "")),
            (
                "duplicate old root cleanup",
                SOURCE.replace("drop(root)", "drop(root)\n drop(root)"),
            ),
            (
                "missing inner error cleanup",
                SOURCE.replace("drop(root)\n                            status", "status"),
            ),
            (
                "missing updated cleanup",
                SOURCE.replace("drop(updated)", ""),
            ),
            (
                "extra map operation",
                SOURCE.replace(
                    "drop(updated)",
                    "let extra = map_size(updated)\n drop(updated)",
                ),
            ),
            (
                "wrong inner status",
                SOURCE.replace(
                    "                            status\n                        }",
                    "                            9\n                        }",
                ),
            ),
            (
                "wrong outer status",
                SOURCE.replace("Err(status) => status", "Err(status) => 9"),
            ),
            (
                "extra outer Err action",
                SOURCE.replace(
                    "Err(status) => status",
                    "Err(status) => { println(\"unexpected\")\n status }",
                ),
            ),
            (
                "guarded arm",
                SOURCE.replace("Ok(updated) =>", "Ok(updated) if true =>"),
            ),
            (
                "extra Set",
                SOURCE.replace(
                    "match changed",
                    "let second = map_set(root, \"other\", 9)\n match changed",
                ),
            ),
        ] {
            if let Ok(program) = checked(&source) {
                let analysis = program
                    .resource_analysis(&NodeId("function:main".into()))
                    .expect("main analysis");
                assert!(
                    analysis
                        .ordinary_map_owned_any_v2_set_lineage_receipt
                        .is_none(),
                    "{label} must withhold source authority"
                );
            }
        }
    }

    #[test]
    fn set_lineage_rejects_forged_receipt_fields_and_cfg_changes() {
        let program = checked(SOURCE).expect("Set lineage source checks");
        let callable = program.callable(&NodeId("function:main".into())).unwrap();
        let receipt = callable
            .resources
            .ordinary_map_owned_any_v2_set_lineage_receipt
            .as_ref()
            .expect("Set lineage receipt");
        let types = program.resolved_types();
        let mut forged = receipt.clone();
        forged.set_operation = forged.new_operation.clone();
        assert!(validate(
            &forged,
            &callable.cfg,
            &callable.body,
            &callable.signature,
            types
        )
        .is_err());
        let mut forged = receipt.clone();
        std::mem::swap(&mut forged.inner_ok_edge, &mut forged.inner_err_edge);
        assert!(validate(
            &forged,
            &callable.cfg,
            &callable.body,
            &callable.signature,
            types
        )
        .is_err());
        let mut cfg = callable.cfg.clone();
        cfg.blocks
            .get_mut(&receipt.inner_ok_block)
            .unwrap()
            .points
            .clear();
        assert!(collect(&cfg, &callable.body, &callable.signature, types).is_none());
    }
}
