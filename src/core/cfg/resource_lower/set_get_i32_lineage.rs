//! Exact Checker source authority for one typed i32 Get on a persistent
//! Result-wrapped ordinary Map Set lineage.
//!
//! This profile is deliberately independent of `set_lineage`: the SetResult
//! receipt keeps its existing `map_size` tail, while this receipt proves one
//! `map_get_i32` on the Set-success root and settles that root in both Get
//! result arms.

use super::*;
use crate::core::cfg::{BasicBlockId, CfgPoint, CfgPointKind, EdgeId};
use crate::core::ir::{CheckedConversionKind, PrimitiveType};
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2SetGetI32LineageProfileVersion as Version,
    OrdinaryMapOwnedAnyV2SetGetI32LineageReceipt as Receipt,
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

fn call<'a>(expression: &'a ResolvedExpr, name: &str, argc: usize) -> Option<&'a ResolvedCall> {
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return None;
    };
    (matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == name)
        && call.arguments.len() == argc
        && call.result == expression.ty
        && call.type_arguments.is_empty()
        && call.permission.is_none()
        && call.session.is_empty())
    .then_some(call)
}

fn load(expression: &ResolvedExpr, local: &ResolvedLocalId, ty: &ResolvedTypeId) -> bool {
    expression.ty == *ty
        && matches!(&expression.kind, ResolvedExprKind::Load(place) if place.base == *local && place.projections.is_empty())
}

fn variant_arm<'a>(arms: &'a [MatchArm], name: &str) -> Option<&'a MatchArm> {
    let expected = format!("builtin:variant:Result::{name}");
    let mut found = arms.iter().filter(|arm| {
        matches!(&arm.pattern.kind, ResolvedPatternKind::Constructor { variant, .. } if variant.0 == expected)
    });
    let arm = found.next()?;
    found.next().is_none().then_some(arm)
}

fn payload<'a>(
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
    let binding = body.locals.get(local)?;
    (variant.0 == format!("builtin:variant:Result::{name}")
        && field.0 == format!("builtin:variant:Result::{name}/payload:0")
        && arm.pattern.ty == *result_type
        && pattern.ty == *payload_type
        && binding.ty == *payload_type
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
    let mut found = arms.iter().filter_map(|edge_id| {
        let edge = cfg.edges.get(edge_id)?;
        (edge.from == *dispatch
            && edge.kind == EdgeKind::MatchArm
            && edge.source.node == arm.node_id)
            .then_some((edge_id, &edge.to))
    });
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

fn join_edge<'a>(
    cfg: &'a CallableCfg,
    block: &BasicBlockId,
    arm: &MatchArm,
) -> Option<(&'a EdgeId, &'a BasicBlockId)> {
    let Terminator::Goto { edge } = &cfg.blocks.get(block)?.terminator else {
        return None;
    };
    let edge = cfg.edges.get(edge)?;
    (edge.from == *block && edge.kind == EdgeKind::Fallthrough && edge.source.node == arm.node_id)
        .then_some((&edge.id, &edge.to))
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
    cfg.blocks.get(block).is_some_and(|basic_block| {
        basic_block
            .points
            .iter()
            .filter(|actual| actual.source.node == *node)
            .count()
            == 1
            && basic_block
                .points
                .iter()
                .any(|actual| point_is(actual, node, origin, kind, uses, defs, places))
    })
}

fn record_type(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(types.get(ty), Some(ResolvedType::Nominal { item, arguments, is_linear: false }) if item.as_str() == "builtin:type:Record" && arguments.is_empty())
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

fn direct_arm_result(arm: &MatchArm) -> Option<&ResolvedExpr> {
    match &arm.body.kind {
        ResolvedExprKind::Block(block) => block.result.as_deref(),
        _ => Some(&arm.body),
    }
}

fn empty_block_or_direct_result(arm: &MatchArm) -> Option<&ResolvedExpr> {
    match &arm.body.kind {
        ResolvedExprKind::Block(block) if block.statements.is_empty() => block.result.as_deref(),
        ResolvedExprKind::Block(block) if block.result.is_none() => {
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

pub(super) fn collect(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
) -> Option<Receipt> {
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
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

    // Permit only a leading sequence of callable-entry Requires/Ensures
    // contracts before the exact executable binding. Contracts elsewhere,
    // including nested and interleaved clauses, stay outside this profile.
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
    let [new_binding] = executable.as_slice() else {
        return None;
    };
    let (new_local, new_expression) = immutable_binding(new_binding, body)?;
    call(new_expression, "map_new", 0)?;
    let ResolvedType::Result {
        ok: initial_root_type,
        error: new_error_type,
    } = types.get(&new_expression.ty)?
    else {
        return None;
    };
    if new_error_type != &i32_type || !record_type(types, initial_root_type) {
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
    if outer.ty != i32_type
        || !load(outer_scrutinee, new_local, &new_expression.ty)
        || outer_arms.len() != 2
        || outer_arms
            .iter()
            .any(|arm| arm.guard.is_some() || arm.body.ty != i32_type)
    {
        return None;
    }
    let outer_ok = variant_arm(outer_arms, "Ok")?;
    let outer_err = variant_arm(outer_arms, "Err")?;
    let (initial_root_local, initial_root_pattern) =
        payload(outer_ok, "Ok", &new_expression.ty, initial_root_type, body)?;
    let (outer_status_local, outer_status_pattern) =
        payload(outer_err, "Err", &new_expression.ty, &i32_type, body)?;

    // Set remains precisely the existing narrow `answer -> i32 literal 42`
    // profile. Its Ok payload becomes the only permitted Get receiver.
    let ResolvedExprKind::Block(outer_ok_body) = &outer_ok.body.kind else {
        return None;
    };
    let [set_binding] = outer_ok_body.statements.as_slice() else {
        return None;
    };
    let (set_result_local, set_expression) = immutable_binding(set_binding, body)?;
    let set_call = call(set_expression, "map_set", 3)?;
    let [set_receiver, set_key, set_value] = set_call.arguments.as_slice() else {
        return None;
    };
    if !load(&set_receiver.value, initial_root_local, initial_root_type)
        || set_receiver.conversion.kind != CheckedConversionKind::Identity
        || set_receiver.conversion.from != *initial_root_type
        || set_receiver.conversion.to != *initial_root_type
        || !matches!(&set_key.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(value)) if value == "answer")
        || set_key.conversion.kind != CheckedConversionKind::Identity
        || !matches!(
            &set_value.value.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(42))
        )
        || set_value.conversion.kind != CheckedConversionKind::Identity
    {
        return None;
    }
    let ResolvedType::Result {
        ok: updated_root_type,
        error: set_error_type,
    } = types.get(&set_expression.ty)?
    else {
        return None;
    };
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
    if set_arms.len() != 2
        || set_arms
            .iter()
            .any(|arm| arm.guard.is_some() || arm.body.ty != i32_type)
        || !load(set_scrutinee, set_result_local, &set_expression.ty)
    {
        return None;
    }
    let set_ok = variant_arm(set_arms, "Ok")?;
    let set_err = variant_arm(set_arms, "Err")?;
    let (updated_root_local, updated_root_pattern) =
        payload(set_ok, "Ok", &set_expression.ty, updated_root_type, body)?;
    let (set_error_local, set_error_pattern) =
        payload(set_err, "Err", &set_expression.ty, &i32_type, body)?;

    let ResolvedExprKind::Block(set_ok_body) = &set_ok.body.kind else {
        return None;
    };
    let [set_success_root_drop, get_binding] = set_ok_body.statements.as_slice() else {
        return None;
    };
    if !drop_local(set_success_root_drop, initial_root_local) {
        return None;
    }
    let (get_result_local, get_expression) = immutable_binding(get_binding, body)?;
    let get_call = call(get_expression, "map_get_i32", 2)?;
    let [get_receiver, get_key] = get_call.arguments.as_slice() else {
        return None;
    };
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    if !load(&get_receiver.value, updated_root_local, updated_root_type)
        || get_receiver.conversion.kind != CheckedConversionKind::Identity
        || get_receiver.conversion.from != *updated_root_type
        || get_receiver.conversion.to != *updated_root_type
        || !matches!(&get_key.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(value)) if value == "answer")
        || get_key.value.ty != string_type
        || get_key.conversion.kind != CheckedConversionKind::Identity
        || get_key.conversion.from != string_type
        || get_key.conversion.to != string_type
    {
        return None;
    }
    let (get_ok_type, get_error_type) = result_parts(types, &get_expression.ty)?;
    if get_ok_type != &i32_type || get_error_type != &i32_type {
        return None;
    }
    let get_match = set_ok_body.result.as_deref()?;
    let ResolvedExprKind::Match {
        scrutinee: get_scrutinee,
        arms: get_arms,
    } = &get_match.kind
    else {
        return None;
    };
    if get_match.ty != i32_type
        || get_arms.len() != 2
        || get_arms
            .iter()
            .any(|arm| arm.guard.is_some() || arm.body.ty != i32_type)
        || !load(get_scrutinee, get_result_local, &get_expression.ty)
    {
        return None;
    }
    let get_ok = variant_arm(get_arms, "Ok")?;
    let get_err = variant_arm(get_arms, "Err")?;
    let (get_ok_local, get_ok_pattern) =
        payload(get_ok, "Ok", &get_expression.ty, &i32_type, body)?;
    let (get_err_local, get_err_pattern) =
        payload(get_err, "Err", &get_expression.ty, &i32_type, body)?;

    let ResolvedExprKind::Block(get_ok_body) = &get_ok.body.kind else {
        return None;
    };
    let [get_ok_root_drop] = get_ok_body.statements.as_slice() else {
        return None;
    };
    if !drop_local(get_ok_root_drop, updated_root_local)
        || !load(get_ok_body.result.as_deref()?, get_ok_local, &i32_type)
    {
        return None;
    }
    let ResolvedExprKind::Block(get_err_body) = &get_err.body.kind else {
        return None;
    };
    let [get_err_root_drop] = get_err_body.statements.as_slice() else {
        return None;
    };
    if !drop_local(get_err_root_drop, updated_root_local)
        || !load(get_err_body.result.as_deref()?, get_err_local, &i32_type)
    {
        return None;
    }

    let ResolvedExprKind::Block(set_err_body) = &set_err.body.kind else {
        return None;
    };
    let [set_error_root_drop] = set_err_body.statements.as_slice() else {
        return None;
    };
    if !drop_local(set_error_root_drop, initial_root_local)
        || !load(direct_arm_result(set_err)?, set_error_local, &i32_type)
    {
        return None;
    }
    if !load(
        empty_block_or_direct_result(outer_err)?,
        outer_status_local,
        &i32_type,
    ) {
        return None;
    }

    let mut locals = BTreeSet::from([
        new_local.clone(),
        initial_root_local.clone(),
        outer_status_local.clone(),
        set_result_local.clone(),
        updated_root_local.clone(),
        set_error_local.clone(),
        get_result_local.clone(),
        get_ok_local.clone(),
        get_err_local.clone(),
    ]);
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
        if result_local.ty != i32_type || result_local.mutable {
            return None;
        }
        locals.insert(contract_result);
    } else if body.locals.contains_key(&contract_result) {
        return None;
    }
    if locals.len() != 9 + usize::from(has_ensures)
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

    let dispatch_for = |scrutinee: &ResolvedExpr| {
        let mut found = cfg.blocks.iter().filter_map(|(id, block)| {
            matches!(&block.terminator, Terminator::Match { scrutinee: node, .. } if node == &scrutinee.node_id)
                .then_some(id.clone())
        });
        let dispatch = found.next()?;
        found.next().is_none().then_some(dispatch)
    };
    let outer_dispatch = dispatch_for(outer_scrutinee)?;
    let set_dispatch = dispatch_for(set_scrutinee)?;
    let get_dispatch = dispatch_for(get_scrutinee)?;
    if outer_dispatch == set_dispatch
        || outer_dispatch == get_dispatch
        || set_dispatch == get_dispatch
    {
        return None;
    }
    let (outer_ok_edge, outer_ok_block) = edge_for_arm(cfg, &outer_dispatch, outer_ok)?;
    let (outer_err_edge, outer_err_block) = edge_for_arm(cfg, &outer_dispatch, outer_err)?;
    let (set_ok_edge, set_ok_block) = edge_for_arm(cfg, &set_dispatch, set_ok)?;
    let (set_err_edge, set_err_block) = edge_for_arm(cfg, &set_dispatch, set_err)?;
    let (get_ok_edge, get_ok_block) = edge_for_arm(cfg, &get_dispatch, get_ok)?;
    let (get_err_edge, get_err_block) = edge_for_arm(cfg, &get_dispatch, get_err)?;
    if set_dispatch != *outer_ok_block || get_dispatch != *set_ok_block {
        return None;
    }
    let (get_ok_join_edge, get_join) = join_edge(cfg, get_ok_block, get_ok)?;
    let (get_err_join_edge, get_err_join) = join_edge(cfg, get_err_block, get_err)?;
    let (set_err_join_edge, set_join_from_err) = join_edge(cfg, set_err_block, set_err)?;
    let (outer_err_join_edge, outer_join_from_err) = join_edge(cfg, outer_err_block, outer_err)?;
    if get_join != get_err_join || get_join == set_err_block || get_join == outer_err_block {
        return None;
    }
    let get_to_set_join_edge = match &cfg.blocks.get(get_join)?.terminator {
        Terminator::Goto { edge } => {
            let edge = cfg.edges.get(edge)?;
            if edge.from != *get_join
                || edge.kind != EdgeKind::Fallthrough
                || edge.source.node != set_ok.node_id
            {
                return None;
            }
            edge
        }
        _ => return None,
    };
    let set_join = &get_to_set_join_edge.to;
    if set_join_from_err != set_join || set_join == get_join {
        return None;
    }
    let set_to_outer_join_edge = match &cfg.blocks.get(set_join)?.terminator {
        Terminator::Goto { edge } => {
            let edge = cfg.edges.get(edge)?;
            if edge.from != *set_join
                || edge.kind != EdgeKind::Fallthrough
                || edge.source.node != outer_ok.node_id
            {
                return None;
            }
            edge
        }
        _ => return None,
    };
    let outer_join = &set_to_outer_join_edge.to;
    if outer_join_from_err != outer_join
        || cfg.blocks.get(outer_join)?.terminator
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
        set_ok_block.clone(),
        set_err_block.clone(),
        get_ok_block.clone(),
        get_err_block.clone(),
        get_join.clone(),
        set_join.clone(),
        outer_join.clone(),
    ]);
    if expected_blocks.len() != 10 {
        return None;
    }
    let expected_edges = BTreeSet::from([
        outer_ok_edge.clone(),
        outer_err_edge.clone(),
        set_ok_edge.clone(),
        set_err_edge.clone(),
        get_ok_edge.clone(),
        get_err_edge.clone(),
        get_ok_join_edge.clone(),
        get_err_join_edge.clone(),
        get_to_set_join_edge.id.clone(),
        set_err_join_edge.clone(),
        set_to_outer_join_edge.id.clone(),
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

    // Bind every reachable CFG point to a source node in this closed shape.
    // This rejects hidden extra calls, statements, guards, or branch-local
    // cleanup operations even if they do not alter the selected payload.
    let mut source_nodes = outer
        .eager_source_nodes()
        .into_iter()
        .collect::<BTreeSet<_>>();
    source_nodes.extend(new_expression.eager_source_nodes());
    for expression in [set_expression, get_expression] {
        source_nodes.extend(expression.eager_source_nodes());
    }
    for arm in outer_arms.iter().chain(set_arms).chain(get_arms) {
        source_nodes.insert(arm.node_id.clone());
        source_nodes.insert(arm.pattern.node_id.clone());
        source_nodes.insert(arm.body.node_id.clone());
    }
    source_nodes.insert(new_binding.node_id.clone());
    source_nodes.insert(match &new_binding.kind {
        ResolvedStmtKind::Bind { pattern, .. } => pattern.node_id.clone(),
        _ => return None,
    });
    source_nodes.insert(set_binding.node_id.clone());
    source_nodes.insert(match &set_binding.kind {
        ResolvedStmtKind::Bind { pattern, .. } => pattern.node_id.clone(),
        _ => return None,
    });
    source_nodes.insert(get_binding.node_id.clone());
    source_nodes.insert(match &get_binding.kind {
        ResolvedStmtKind::Bind { pattern, .. } => pattern.node_id.clone(),
        _ => return None,
    });
    for statement in [
        set_success_root_drop,
        set_error_root_drop,
        get_ok_root_drop,
        get_err_root_drop,
    ] {
        source_nodes.insert(statement.node_id.clone());
    }
    source_nodes.insert(outer.node_id.clone());
    source_nodes.insert(set_match.node_id.clone());
    source_nodes.insert(get_match.node_id.clone());
    source_nodes.extend(proof_nodes);
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
        || [
            new_expression,
            set_expression,
            get_expression,
            outer,
            set_match,
            get_match,
        ]
        .iter()
        .any(|expression| {
            !points
                .iter()
                .any(|point| point.source.node == expression.node_id)
        })
        || [new_binding, set_binding, get_binding]
            .iter()
            .any(|statement| {
                !points
                    .iter()
                    .any(|point| point.source.node == statement.node_id)
            })
    {
        return None;
    }

    let empty_point = |block: &BasicBlockId, expression: &ResolvedExpr, kind| {
        has_point(
            cfg,
            block,
            &expression.node_id,
            &expression.origin,
            kind,
            &[],
            &[],
            &[],
        )
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
    let load_point =
        |block: &BasicBlockId, expression: &ResolvedExpr, local: &ResolvedLocalId, kind| {
            let place = root_place(body, local);
            let name = place.base_name.clone();
            has_point(
                cfg,
                block,
                &expression.node_id,
                &expression.origin,
                kind,
                &[name],
                &[],
                &[place],
            )
        };
    let drop_point = |block: &BasicBlockId, statement: &ResolvedStmt, local: &ResolvedLocalId| {
        cfg.blocks.get(block).is_some_and(|basic_block| {
            basic_block.points.iter().any(|point| {
                point.source.node == statement.node_id
                    && point.kind == CfgPointKind::ResourceAction
                    && point.read_places == vec![root_place(body, local)]
                    && point.write_places.is_empty()
            })
        })
    };
    if !empty_point(&outer_dispatch, new_expression, CfgPointKind::Expression)
        || !binding_point(&outer_dispatch, new_binding, new_local)
        || !load_point(
            &outer_dispatch,
            outer_scrutinee,
            new_local,
            CfgPointKind::Condition,
        )
        || !pattern_point(outer_ok_block, outer_ok, initial_root_local)
        || !pattern_point(outer_err_block, outer_err, outer_status_local)
        || !load_point(
            outer_ok_block,
            &set_receiver.value,
            initial_root_local,
            CfgPointKind::Expression,
        )
        || !empty_point(outer_ok_block, set_expression, CfgPointKind::Expression)
        || !binding_point(outer_ok_block, set_binding, set_result_local)
        || !load_point(
            &set_dispatch,
            set_scrutinee,
            set_result_local,
            CfgPointKind::Condition,
        )
        || !pattern_point(set_ok_block, set_ok, updated_root_local)
        || !pattern_point(set_err_block, set_err, set_error_local)
        || !drop_point(set_ok_block, set_success_root_drop, initial_root_local)
        || !load_point(
            set_ok_block,
            &get_receiver.value,
            updated_root_local,
            CfgPointKind::Expression,
        )
        || !empty_point(set_ok_block, get_expression, CfgPointKind::Expression)
        || !binding_point(set_ok_block, get_binding, get_result_local)
        || !load_point(
            &get_dispatch,
            get_scrutinee,
            get_result_local,
            CfgPointKind::Condition,
        )
        || !pattern_point(get_ok_block, get_ok, get_ok_local)
        || !pattern_point(get_err_block, get_err, get_err_local)
        || !drop_point(get_ok_block, get_ok_root_drop, updated_root_local)
        || !drop_point(get_err_block, get_err_root_drop, updated_root_local)
        || !drop_point(set_err_block, set_error_root_drop, initial_root_local)
        || !load_point(
            get_ok_block,
            get_ok_body.result.as_deref()?,
            get_ok_local,
            CfgPointKind::Expression,
        )
        || !load_point(
            get_err_block,
            get_err_body.result.as_deref()?,
            get_err_local,
            CfgPointKind::Expression,
        )
        || !load_point(
            set_err_block,
            direct_arm_result(set_err)?,
            set_error_local,
            CfgPointKind::Expression,
        )
        || !load_point(
            outer_err_block,
            empty_block_or_direct_result(outer_err)?,
            outer_status_local,
            CfgPointKind::Expression,
        )
    {
        return None;
    }

    Some(Receipt {
        profile_version: Version::V1,
        owner: body.owner.clone(),
        new_operation: new_expression.node_id.clone(),
        new_result_binding: new_binding.node_id.clone(),
        new_result_local: new_local.clone(),
        outer_match: outer.node_id.clone(),
        outer_scrutinee: outer_scrutinee.node_id.clone(),
        outer_ok_arm: outer_ok.node_id.clone(),
        outer_err_arm: outer_err.node_id.clone(),
        initial_root_pattern: initial_root_pattern.node_id.clone(),
        initial_root_local: initial_root_local.clone(),
        initial_root_type: initial_root_type.clone(),
        initial_root: resource_identity(initial_root_local),
        outer_status_pattern: outer_status_pattern.node_id.clone(),
        outer_status_local: outer_status_local.clone(),
        outer_err_result: empty_block_or_direct_result(outer_err)?.node_id.clone(),
        set_operation: set_expression.node_id.clone(),
        set_receiver: set_receiver.value.node_id.clone(),
        set_result_binding: set_binding.node_id.clone(),
        set_result_local: set_result_local.clone(),
        set_result_type: set_expression.ty.clone(),
        set_ok_arm: set_ok.node_id.clone(),
        set_err_arm: set_err.node_id.clone(),
        updated_root_pattern: updated_root_pattern.node_id.clone(),
        updated_root_local: updated_root_local.clone(),
        updated_root_type: updated_root_type.clone(),
        updated_root: resource_identity(updated_root_local),
        set_error_pattern: set_error_pattern.node_id.clone(),
        set_error_local: set_error_local.clone(),
        set_error_result: direct_arm_result(set_err)?.node_id.clone(),
        old_root_set_ok_drop: set_success_root_drop.node_id.clone(),
        old_root_set_err_drop: set_error_root_drop.node_id.clone(),
        get_operation: get_expression.node_id.clone(),
        get_receiver: get_receiver.value.node_id.clone(),
        get_receiver_local: updated_root_local.clone(),
        get_key: get_key.value.node_id.clone(),
        get_key_type: get_key.value.ty.clone(),
        get_key_value: "answer".into(),
        get_result_binding: get_binding.node_id.clone(),
        get_result_local: get_result_local.clone(),
        get_result_type: get_expression.ty.clone(),
        get_match: get_match.node_id.clone(),
        get_scrutinee: get_scrutinee.node_id.clone(),
        get_ok_arm: get_ok.node_id.clone(),
        get_err_arm: get_err.node_id.clone(),
        get_ok_pattern: get_ok_pattern.node_id.clone(),
        get_ok_local: get_ok_local.clone(),
        get_ok_result: get_ok_body.result.as_deref()?.node_id.clone(),
        get_err_pattern: get_err_pattern.node_id.clone(),
        get_err_local: get_err_local.clone(),
        get_err_result: get_err_body.result.as_deref()?.node_id.clone(),
        updated_root_get_ok_drop: get_ok_root_drop.node_id.clone(),
        updated_root_get_err_drop: get_err_root_drop.node_id.clone(),
        entry: outer_dispatch.clone(),
        cfg: ManagedAnyCfgReceipt::from_cfg(cfg, body),
    })
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
        Err("ordinary Map Set/Get i32 lineage receipt disagrees with exact Checker source/CFG evidence".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!(
        "../../../../tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_i32_lineage.mimi"
    );

    fn checked(source: &str) -> Result<crate::core::CheckedProgram, Vec<Diagnostic>> {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file)
    }

    #[test]
    fn set_get_i32_receipt_binds_the_updated_root_key_arms_and_cleanup() {
        let program = checked(FIXTURE).expect("Set/Get source checks");
        let callable = program
            .callable(&NodeId("function:main".into()))
            .expect("main callable");
        let receipt = callable
            .resources
            .ordinary_map_owned_any_v2_set_get_i32_lineage_receipt
            .as_ref()
            .expect("exact Set/Get Checker receipt");
        validate(
            receipt,
            &callable.cfg,
            &callable.body,
            &callable.signature,
            program.resolved_types(),
        )
        .expect("exact receipt replay");
        assert_eq!(receipt.profile_version, Version::V1);
        assert_eq!(receipt.get_key_value, "answer");
        assert_eq!(
            program.resolved_types().get(&receipt.get_key_type),
            Some(&ResolvedType::Primitive(PrimitiveType::String))
        );
        assert_ne!(receipt.initial_root_local, receipt.updated_root_local);
        assert_eq!(
            receipt.updated_root,
            resource_identity(&receipt.updated_root_local)
        );
        assert_eq!(receipt.get_receiver_local, receipt.updated_root_local);
        assert_ne!(receipt.get_ok_arm, receipt.get_err_arm);
        assert_ne!(receipt.get_ok_pattern, receipt.get_err_pattern);
        assert_ne!(receipt.get_ok_local, receipt.get_err_local);
        assert_ne!(
            receipt.updated_root_get_ok_drop,
            receipt.updated_root_get_err_drop
        );
        assert_ne!(receipt.old_root_set_ok_drop, receipt.old_root_set_err_drop);
        assert_eq!(receipt.cfg.reachable_blocks.len(), 10);
        assert_eq!(receipt.cfg.reachable_edges.len(), 12);
        assert!(
            callable
                .resources
                .ordinary_map_owned_any_v2_set_lineage_receipt
                .is_none(),
            "the existing SetResult profile remains closed over its original map_size tail"
        );
        assert!(
            callable
                .resources
                .ordinary_map_owned_any_v2_new_result_receipt
                .is_none(),
            "a Set/Get proof is not a New-only Result proof"
        );
    }

    #[test]
    fn set_get_i32_receipt_accepts_only_leading_callable_contracts() {
        let leading = FIXTURE.replacen(
            "func main() -> i32 {",
            "func main() -> i32 {\n    ensures: result == 42 || result == 2",
            1,
        );
        let program = checked(&leading).expect("leading ensures source checks");
        let callable = program
            .callable(&NodeId("function:main".into()))
            .expect("main callable");
        let receipt = callable
            .resources
            .ordinary_map_owned_any_v2_set_get_i32_lineage_receipt
            .as_ref()
            .expect("leading function contract preserves exact Set/Get receipt");
        validate(
            receipt,
            &callable.cfg,
            &callable.body,
            &callable.signature,
            program.resolved_types(),
        )
        .expect("leading contract receipt replays against exact Checker source");
        let contracts = callable.body.contract_proof_roots();
        assert_eq!(contracts.len(), 1);
        assert_eq!(contracts[0].kind, crate::core::ir::ContractKind::Ensures);
        assert_eq!(
            contracts[0].node_id,
            callable.body.root.statements[0].node_id
        );

        let near_misses = [
            (
                "interleaved contract",
                FIXTURE.replace(
                    "    let created = map_new()",
                    "    let created = map_new()\n    ensures: result == 42",
                ),
            ),
            (
                "nested contract",
                FIXTURE.replace(
                    "        Ok(root) => {\n            let changed = map_set(root, \"answer\", 42)",
                    "        Ok(root) => {\n            ensures: result == 42\n            let changed = map_set(root, \"answer\", 42)",
                ),
            ),
            (
                "leading invariant",
                FIXTURE.replacen(
                    "func main() -> i32 {",
                    "func main() -> i32 {\n    invariant: result == 42",
                    1,
                ),
            ),
        ];
        for (label, source) in near_misses {
            assert_ne!(source, FIXTURE, "the {label} mutation must apply");
            if let Ok(program) = checked(&source) {
                let callable = program
                    .callable(&NodeId("function:main".into()))
                    .expect("near-miss main callable");
                assert!(
                    callable
                        .resources
                        .ordinary_map_owned_any_v2_set_get_i32_lineage_receipt
                        .is_none(),
                    "{label} must withhold the exact Set/Get receipt"
                );
            }
        }
    }

    #[test]
    fn set_get_i32_receipt_rejects_other_roots_keys_arms_and_cleanup() {
        let mutations = [
            (
                "pre-Set root",
                FIXTURE.replace(
                    "                    drop(root)\n                    let lookup = map_get_i32(updated, \"answer\")",
                    "                    let lookup = map_get_i32(root, \"answer\")\n                    drop(root)",
                ),
            ),
            (
                "different key",
                FIXTURE.replace("map_get_i32(updated, \"answer\")", "map_get_i32(updated, \"other\")"),
            ),
            (
                "non-propagating Err arm",
                FIXTURE.replace(
                    "Err(status) => {\n                            drop(updated)\n                            status",
                    "Err(status) => {\n                            drop(updated)\n                            0",
                ),
            ),
            (
                "missing Err cleanup",
                FIXTURE.replace("Err(status) => {\n                            drop(updated)", "Err(status) => {"),
            ),
            (
                "additional Get operation",
                FIXTURE.replace(
                    "                    let lookup = map_get_i32(updated, \"answer\")",
                    "                    let extra = map_get_i32(updated, \"answer\")\n                    let lookup = map_get_i32(updated, \"answer\")",
                ),
            ),
            (
                "additional Set operation",
                FIXTURE.replace(
                    "            let changed = map_set(root, \"answer\", 42)",
                    "            let extra = map_set(root, \"answer\", 42)\n            let changed = map_set(root, \"answer\", 42)",
                ),
            ),
            (
                "wrong Ok payload propagation",
                FIXTURE.replace(
                    "Ok(value) => {\n                            drop(updated)\n                            value",
                    "Ok(value) => {\n                            drop(updated)\n                            value + 1",
                ),
            ),
        ];
        for (label, source) in mutations {
            assert_ne!(source, FIXTURE, "the {label} mutation must apply");
            if let Ok(program) = checked(&source) {
                let callable = program
                    .callable(&NodeId("function:main".into()))
                    .expect("mutated main callable");
                assert!(
                    callable
                        .resources
                        .ordinary_map_owned_any_v2_set_get_i32_lineage_receipt
                        .is_none(),
                    "{label} must withhold the exact Set/Get receipt"
                );
            }
        }
    }
}
