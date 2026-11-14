//! Exact Checker source/CFG authority for the Result-valued ordinary Map
//! `map_get_any` API. This is intentionally independent of
//! `set_get_any_i32_lineage`, whose legacy `map_get` returns a bare
//! `(bool, Any)` tuple and has no Get Result error arm.

use super::*;
use crate::core::cfg::{BasicBlockId, CfgPointKind, EdgeId, EdgeKind, Terminator};
use crate::core::ir::{CheckedConversionKind, PrimitiveType, ResolvedBinaryOp, ResolvedProjection};
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2Attempt,
    OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind, OrdinaryMapOwnedAnyV2GetApi as GetApi,
    OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageProfileVersion as Version,
    OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageReceipt as Receipt,
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
        "ordinary-map-owned-any-v2-set-get-any-result-i32-lineage-v1:{}",
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

#[derive(Default)]
struct EnsuresCfgExpectation {
    blocks: BTreeMap<BasicBlockId, NodeId>,
    edges: BTreeMap<EdgeId, (BasicBlockId, BasicBlockId, EdgeKind, NodeId)>,
    terminators: BTreeMap<BasicBlockId, Terminator>,
    synthetic_point_nodes: BTreeSet<NodeId>,
}

fn derived_block(node: &NodeId, role: &str) -> BasicBlockId {
    BasicBlockId(NodeId(format!("{}/cfg:block:{role}", node.0)))
}

fn derived_edge(node: &NodeId, role: &str) -> EdgeId {
    EdgeId(NodeId(format!("{}/cfg:edge:{role}", node.0)))
}

fn insert_ensures_edge(
    expected: &mut EnsuresCfgExpectation,
    node: &NodeId,
    role: &str,
    from: BasicBlockId,
    to: BasicBlockId,
    kind: EdgeKind,
    source: &NodeId,
) -> Option<EdgeId> {
    let id = derived_edge(node, role);
    expected
        .edges
        .insert(id.clone(), (from, to, kind, source.clone()))
        .is_none()
        .then_some(id)
}

fn insert_ensures_terminator(
    expected: &mut EnsuresCfgExpectation,
    block: BasicBlockId,
    terminator: Terminator,
) -> Option<()> {
    expected
        .terminators
        .insert(block, terminator)
        .is_none()
        .then_some(())
}

/// Derive the only additional CFG shape admitted for a direct result-domain
/// Ensures. Its expression is a pure tree of `result == literal` comparisons
/// joined with `||`; retain and check every exact branch, shortcut, and join
/// identity so the proof receipt cannot conceal unrelated control flow.
pub(super) fn validates_direct_result_domain_ensures_cfg(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    condition: &ResolvedExpr,
    business_blocks: &BTreeSet<BasicBlockId>,
    business_edges: &BTreeSet<EdgeId>,
) -> Option<BTreeSet<NodeId>> {
    fn lower_or_tree(
        expression: &ResolvedExpr,
        current: BasicBlockId,
        expected: &mut EnsuresCfgExpectation,
    ) -> Option<BasicBlockId> {
        let ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::LogicalOr,
            left,
            right,
        } = &expression.kind
        else {
            return Some(current);
        };

        let condition_end = lower_or_tree(left, current, expected)?;
        let shortcut_node = NodeId(format!("{}/short-circuit", expression.node_id.0));
        let then_entry = derived_block(&shortcut_node, "if-then");
        let else_entry = derived_block(&right.node_id, "if-else");
        let join = derived_block(&expression.node_id, "if-join");
        expected
            .blocks
            .insert(then_entry.clone(), shortcut_node.clone())
            .is_none()
            .then_some(())?;
        expected
            .blocks
            .insert(else_entry.clone(), right.node_id.clone())
            .is_none()
            .then_some(())?;
        expected
            .blocks
            .insert(join.clone(), expression.node_id.clone())
            .is_none()
            .then_some(())?;
        expected.synthetic_point_nodes.insert(shortcut_node.clone());

        let then_edge = insert_ensures_edge(
            expected,
            &expression.node_id,
            "if-then",
            condition_end.clone(),
            then_entry.clone(),
            EdgeKind::Then,
            &expression.node_id,
        )?;
        let else_edge = insert_ensures_edge(
            expected,
            &expression.node_id,
            "if-else",
            condition_end.clone(),
            else_entry.clone(),
            EdgeKind::Else,
            &expression.node_id,
        )?;
        insert_ensures_terminator(
            expected,
            condition_end,
            Terminator::Branch {
                condition: left.node_id.clone(),
                then_edge,
                else_edge,
            },
        )?;

        let then_join_edge = insert_ensures_edge(
            expected,
            &shortcut_node,
            "if-then-join",
            then_entry.clone(),
            join.clone(),
            EdgeKind::Fallthrough,
            &shortcut_node,
        )?;
        insert_ensures_terminator(
            expected,
            then_entry,
            Terminator::Goto {
                edge: then_join_edge,
            },
        )?;

        let right_end = lower_or_tree(right, else_entry, expected)?;
        let else_join_edge = insert_ensures_edge(
            expected,
            &right.node_id,
            "if-else-join",
            right_end.clone(),
            join.clone(),
            EdgeKind::Fallthrough,
            &right.node_id,
        )?;
        insert_ensures_terminator(
            expected,
            right_end,
            Terminator::Goto {
                edge: else_join_edge,
            },
        )?;
        Some(join)
    }

    let entry = derived_block(&body.root.node_id, "entry");
    let mut expected = EnsuresCfgExpectation::default();
    expected
        .blocks
        .insert(entry.clone(), body.root.node_id.clone());
    let exit = lower_or_tree(condition, entry.clone(), &mut expected)?;
    if !business_blocks.contains(&exit) {
        return None;
    }

    let expected_blocks = business_blocks
        .iter()
        .cloned()
        .chain(expected.blocks.keys().cloned())
        .collect::<BTreeSet<_>>();
    let expected_edges = business_edges
        .iter()
        .cloned()
        .chain(expected.edges.keys().cloned())
        .collect::<BTreeSet<_>>();
    if cfg.entry != entry
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != expected_blocks
        || cfg.edges.keys().cloned().collect::<BTreeSet<_>>() != expected_edges
        || expected
            .edges
            .keys()
            .any(|edge| business_edges.contains(edge))
    {
        return None;
    }
    for (block, source) in &expected.blocks {
        if !cfg
            .blocks
            .get(block)
            .is_some_and(|observed| observed.source.node == *source)
        {
            return None;
        }
    }
    for (block, terminator) in &expected.terminators {
        if !cfg
            .blocks
            .get(block)
            .is_some_and(|observed| &observed.terminator == terminator)
        {
            return None;
        }
    }
    for (id, (from, to, kind, source)) in &expected.edges {
        if !cfg.edges.get(id).is_some_and(|observed| {
            observed.from == *from
                && observed.to == *to
                && observed.kind == *kind
                && observed.source.node == *source
        }) {
            return None;
        }
    }
    Some(expected.synthetic_point_nodes)
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

fn local_is(body: &ResolvedBody, local: &ResolvedLocalId, ty: &ResolvedTypeId) -> bool {
    body.locals
        .get(local)
        .is_some_and(|binding| binding.ty == *ty && !binding.mutable)
}

pub(super) fn collect(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
    checker_operations: &BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
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

    let (proof_ensures, proof_ensures_expression, new_binding) =
        match body.root.statements.as_slice() {
            [new_binding] => (None, None, new_binding),
            [ensures, new_binding] => {
                let ResolvedStmtKind::Contract {
                    kind: crate::core::ir::ContractKind::Ensures,
                    condition,
                } = &ensures.kind
                else {
                    return None;
                };
                (
                    Some(ensures.node_id.clone()),
                    Some(condition.node_id.clone()),
                    new_binding,
                )
            }
            _ => return None,
        };
    let contract_result =
        ResolvedLocalId(NodeId(format!("{}/contract-result/local", body.owner.0)));
    if proof_ensures.is_some() != body.locals.contains_key(&contract_result)
        || (proof_ensures.is_some() && !local_is(body, &contract_result, &i32_type))
    {
        return None;
    };
    let (new_result_local, new_expression) = immutable_binding(new_binding, body)?;
    builtin_call(new_expression, "map_new", 0)?;
    let (initial_root_type, new_error_type) = result_parts(types, &new_expression.ty)?;
    if new_error_type != &i32_type || !record_type(types, initial_root_type) {
        return None;
    }
    let new_match = body.root.result.as_deref()?;
    let ResolvedExprKind::Match {
        scrutinee: new_scrutinee,
        arms: new_arms,
    } = &new_match.kind
    else {
        return None;
    };
    if new_match.ty != i32_type
        || !load(new_scrutinee, new_result_local, &new_expression.ty)
        || new_arms.len() != 2
        || new_arms
            .iter()
            .any(|arm| arm.guard.is_some() || arm.body.ty != i32_type)
    {
        return None;
    }
    let new_ok_arm = variant_arm(new_arms, "Ok")?;
    let new_err_arm = variant_arm(new_arms, "Err")?;
    let (initial_root_local, initial_root_pattern) = payload(
        new_ok_arm,
        "Ok",
        &new_expression.ty,
        initial_root_type,
        body,
    )?;
    let (new_error_local, new_error_pattern) =
        payload(new_err_arm, "Err", &new_expression.ty, &i32_type, body)?;
    if BTreeSet::from([
        new_result_local.clone(),
        initial_root_local.clone(),
        new_error_local.clone(),
    ])
    .len()
        != 3
        || !load(direct_arm_result(new_err_arm)?, new_error_local, &i32_type)
    {
        return None;
    }
    if matches!(&new_err_arm.body.kind, ResolvedExprKind::Block(block)
        if !block.statements.is_empty())
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
    let set_match = new_ok_body.result.as_deref()?;
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
    let old_root_set_err_drop = only_drop(set_err_arm)?;
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
        || !load(direct_arm_result(set_err_arm)?, set_error_local, &i32_type)
        || !drop_local(old_root_set_err_drop, initial_root_local)
    {
        return None;
    }

    let ResolvedExprKind::Block(set_ok_body) = &set_ok_arm.body.kind else {
        return None;
    };
    let [old_root_set_ok_drop, get_binding] = set_ok_body.statements.as_slice() else {
        return None;
    };
    if !drop_local(old_root_set_ok_drop, initial_root_local)
        || !drop_local(only_drop(set_err_arm)?, initial_root_local)
    {
        return None;
    }
    let (get_result_local, get_expression) = immutable_binding(get_binding, body)?;
    let get_call = builtin_call(get_expression, "map_get_any", 2)?;
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
    let (pair_type, get_error_type) = result_parts(types, &get_expression.ty)?;
    if get_error_type != &i32_type {
        return None;
    }
    let ResolvedType::Tuple(pair_elements) = types.get(pair_type)? else {
        return None;
    };
    let [found_type, any_type] = pair_elements.as_slice() else {
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
    let get_match = set_ok_body.result.as_deref()?;
    let ResolvedExprKind::Match {
        scrutinee: get_scrutinee,
        arms: get_arms,
    } = &get_match.kind
    else {
        return None;
    };
    if get_match.ty != i32_type
        || !load(get_scrutinee, get_result_local, &get_expression.ty)
        || get_arms.len() != 2
        || get_arms
            .iter()
            .any(|arm| arm.guard.is_some() || arm.body.ty != i32_type)
    {
        return None;
    }
    let get_ok_arm = variant_arm(get_arms, "Ok")?;
    let get_err_arm = variant_arm(get_arms, "Err")?;
    let (get_ok_pair_local, get_ok_pair_pattern) =
        payload(get_ok_arm, "Ok", &get_expression.ty, pair_type, body)?;
    let (get_err_local, get_err_pattern) =
        payload(get_err_arm, "Err", &get_expression.ty, &i32_type, body)?;
    if BTreeSet::from([
        get_result_local.clone(),
        get_ok_pair_local.clone(),
        get_err_local.clone(),
    ])
    .len()
        != 3
        || !local_is(body, get_ok_pair_local, pair_type)
        || !load(direct_arm_result(get_err_arm)?, get_err_local, &i32_type)
    {
        return None;
    }
    let ResolvedExprKind::Block(get_ok_body) = &get_ok_arm.body.kind else {
        return None;
    };
    let [found_binding, unpack_binding, pair_drop, get_ok_root_drop] =
        get_ok_body.statements.as_slice()
    else {
        return None;
    };
    if !drop_local(pair_drop, get_ok_pair_local)
        || !drop_local(get_ok_root_drop, updated_root_local)
    {
        return None;
    }
    let ResolvedExprKind::Block(get_err_body) = &get_err_arm.body.kind else {
        return None;
    };
    let [get_err_root_drop] = get_err_body.statements.as_slice() else {
        return None;
    };
    if !drop_local(get_err_root_drop, updated_root_local)
        || !load(direct_arm_result(get_err_arm)?, get_err_local, &i32_type)
    {
        return None;
    }
    if updated_root_local == get_err_local {
        return None;
    }
    let (found_local, found_expression) = immutable_binding(found_binding, body)?;
    if !tuple_projection(found_expression, get_ok_pair_local, 0, &bool_type)
        || !local_is(body, found_local, &bool_type)
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
        || !tuple_projection(unpack_argument, get_ok_pair_local, 1, any_type)
        || unpack_argument.ty != *any_type
        || !local_is(body, unpack_result_local, &i32_type)
        || BTreeSet::from([found_local.clone(), unpack_result_local.clone()]).len() != 2
    {
        return None;
    }

    let get_ok_result = get_ok_body.result.as_deref()?;
    let ResolvedExprKind::If {
        condition,
        then_block,
        else_block,
    } = &get_ok_result.kind
    else {
        return None;
    };
    if get_ok_result.ty != i32_type
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
        || !matches!(
            &else_block.result.as_deref()?.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
        )
    {
        return None;
    }
    if !load(direct_arm_result(set_err_arm)?, set_error_local, &i32_type)
        || matches!(&set_err_arm.body.kind, ResolvedExprKind::Block(block)
            if block.statements.len() != 1)
    {
        return None;
    }
    let expected_checker_operations = BTreeSet::from([
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
    if checker_operations != &expected_checker_operations {
        return None;
    }
    let operations = expected_checker_operations.clone();

    // Only the exact direct Ensures prelude already bound above may augment
    // this source shape. Contract condition semantics are checked against the
    // selected query key and result local below; other contracts/effects stay
    // outside the profile.
    let expected_locals = BTreeSet::from([
        new_result_local.clone(),
        initial_root_local.clone(),
        new_error_local.clone(),
        set_result_local.clone(),
        updated_root_local.clone(),
        set_error_local.clone(),
        get_result_local.clone(),
        get_ok_pair_local.clone(),
        get_err_local.clone(),
        found_local.clone(),
        unpack_result_local.clone(),
    ]);
    let expected_locals = if proof_ensures.is_some() {
        expected_locals
            .into_iter()
            .chain([contract_result.clone()])
            .collect()
    } else {
        expected_locals
    };
    if expected_locals.len() != if proof_ensures.is_some() { 12 } else { 11 }
        || body.locals.keys().cloned().collect::<BTreeSet<_>>() != expected_locals
        || cfg.reachable.len() != cfg.blocks.len()
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != cfg.reachable
        || crate::core::cfg::validate_cfg(cfg).is_err()
    {
        return None;
    }
    let mut new_dispatches = cfg.blocks.iter().filter_map(|(block, basic_block)| {
        matches!(
            &basic_block.terminator,
            Terminator::Match { scrutinee, .. } if scrutinee == &new_scrutinee.node_id
        )
        .then_some(block.clone())
    });
    let new_dispatch = new_dispatches.next()?;
    if new_dispatches.next().is_some() {
        return None;
    }
    let (new_ok_edge, new_ok_block) = edge_for_arm(cfg, &new_dispatch, new_ok_arm)?;
    let (new_err_edge, new_err_block) = edge_for_arm(cfg, &new_dispatch, new_err_arm)?;
    if new_ok_block == new_err_block {
        return None;
    }
    let set_dispatch = new_ok_block;
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
    let get_dispatch = set_ok_block;
    let Terminator::Match {
        scrutinee: cfg_get_scrutinee,
        ..
    } = &cfg.blocks.get(get_dispatch)?.terminator
    else {
        return None;
    };
    if cfg_get_scrutinee != &get_scrutinee.node_id {
        return None;
    }
    let (get_ok_edge, get_ok_block) = edge_for_arm(cfg, get_dispatch, get_ok_arm)?;
    let (get_err_edge, get_err_block) = edge_for_arm(cfg, get_dispatch, get_err_arm)?;
    if get_ok_block == get_err_block {
        return None;
    }
    let found_dispatch = get_ok_block;
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
    let (get_err_join_edge, get_join_from_get_err) = goto_edge(cfg, get_err_block)?;
    let (found_to_get_join_edge, get_join_from_found) = goto_edge(cfg, found_join)?;
    let (set_err_join_edge, set_join_from_set_err) = goto_edge(cfg, set_err_block)?;
    if get_join_from_get_err != get_join_from_found {
        return None;
    }
    let get_join = get_join_from_found;
    // Get outcomes join inside Set's Ok arm. That nested match then joins the
    // Set Err arm at the Set match join before the New Ok arm exits.
    let (get_to_set_match_join_edge, set_match_join) = goto_edge(cfg, get_join)?;
    if set_join_from_set_err != set_match_join {
        return None;
    }
    let (set_match_to_outer_join_edge, outer_join) = goto_edge(cfg, set_match_join)?;
    let (new_err_join_edge, outer_join_from_new_err) = goto_edge(cfg, new_err_block)?;
    if outer_join_from_new_err != outer_join
        || cfg.blocks.get(outer_join)?.terminator
            != (Terminator::Return {
                value: Some(new_match.node_id.clone()),
                implicit: true,
            })
    {
        return None;
    }

    let expected_blocks = BTreeSet::from([
        new_dispatch.clone(),
        new_err_block.clone(),
        set_dispatch.clone(),
        set_err_block.clone(),
        get_dispatch.clone(),
        get_ok_block.clone(),
        get_err_block.clone(),
        found_true_block.clone(),
        found_false_block.clone(),
        found_join.clone(),
        get_join.clone(),
        set_match_join.clone(),
        outer_join.clone(),
    ]);
    let actual_blocks = cfg.blocks.keys().cloned().collect::<BTreeSet<_>>();
    if expected_blocks.len() != 13 || !actual_blocks.is_superset(&expected_blocks) {
        return None;
    }
    let expected_edges = BTreeSet::from([
        new_ok_edge.clone(),
        new_err_edge.clone(),
        set_ok_edge.clone(),
        set_err_edge.clone(),
        get_ok_edge.clone(),
        get_err_edge.clone(),
        found_true_edge_id.clone(),
        found_false_edge_id.clone(),
        found_true_join_edge.clone(),
        found_false_join_edge.clone(),
        get_err_join_edge.clone(),
        found_to_get_join_edge.clone(),
        set_err_join_edge.clone(),
        get_to_set_match_join_edge.clone(),
        set_match_to_outer_join_edge.clone(),
        new_err_join_edge.clone(),
    ]);
    let actual_edges = cfg.edges.keys().cloned().collect::<BTreeSet<_>>();
    if expected_edges.len() != 16 || !actual_edges.is_superset(&expected_edges) {
        return None;
    }

    let contract_shortcut_nodes = if let Some(ensures) = body
        .root
        .statements
        .first()
        .filter(|statement| proof_ensures.as_ref() == Some(&statement.node_id))
    {
        let ResolvedStmtKind::Contract { condition, .. } = &ensures.kind else {
            return None;
        };
        if !crate::core::ir::set_get_any_result_i32_contract_is_direct_result_domain(
            condition,
            &body.owner,
        ) {
            return None;
        }
        validates_direct_result_domain_ensures_cfg(
            cfg,
            body,
            condition,
            &expected_blocks,
            &expected_edges,
        )?
    } else {
        if actual_blocks != expected_blocks || actual_edges != expected_edges {
            return None;
        }
        BTreeSet::new()
    };

    let mut source_nodes = new_match
        .eager_source_nodes()
        .into_iter()
        .collect::<BTreeSet<_>>();
    source_nodes.extend(new_expression.eager_source_nodes());
    source_nodes.extend(set_expression.eager_source_nodes());
    source_nodes.extend(set_match.eager_source_nodes());
    source_nodes.extend(get_expression.eager_source_nodes());
    source_nodes.extend(get_match.eager_source_nodes());
    source_nodes.extend(found_expression.eager_source_nodes());
    source_nodes.extend(unpack_expression.eager_source_nodes());
    source_nodes.extend(get_ok_result.eager_source_nodes());
    if let Some(ensures) = body
        .root
        .statements
        .first()
        .filter(|statement| proof_ensures.as_ref() == Some(&statement.node_id))
    {
        source_nodes.insert(ensures.node_id.clone());
        let ResolvedStmtKind::Contract { condition, .. } = &ensures.kind else {
            return None;
        };
        source_nodes.extend(condition.eager_source_nodes());
    }
    source_nodes.extend(contract_shortcut_nodes);
    source_nodes.extend([
        new_match.node_id.clone(),
        new_expression.node_id.clone(),
        set_expression.node_id.clone(),
        set_match.node_id.clone(),
        get_expression.node_id.clone(),
        get_match.node_id.clone(),
        found_expression.node_id.clone(),
        unpack_expression.node_id.clone(),
        unpack_argument.node_id.clone(),
        condition.node_id.clone(),
        then_block.node_id.clone(),
        else_block.node_id.clone(),
        initial_root_pattern.node_id.clone(),
        new_error_pattern.node_id.clone(),
        updated_root_pattern.node_id.clone(),
        set_error_pattern.node_id.clone(),
        get_ok_pair_pattern.node_id.clone(),
        get_err_pattern.node_id.clone(),
    ]);
    for statement in [
        new_binding,
        set_binding,
        get_binding,
        found_binding,
        unpack_binding,
        old_root_set_ok_drop,
        old_root_set_err_drop,
        pair_drop,
        get_ok_root_drop,
        get_err_root_drop,
    ] {
        source_nodes.insert(statement.node_id.clone());
        if let ResolvedStmtKind::Bind { pattern, .. } = &statement.kind {
            source_nodes.insert(pattern.node_id.clone());
        }
    }
    for arm in [
        new_ok_arm,
        new_err_arm,
        set_ok_arm,
        set_err_arm,
        get_ok_arm,
        get_err_arm,
    ] {
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
            &new_dispatch,
            &new_expression.node_id,
            CfgPointKind::Expression,
        )
        || !has_point(
            cfg,
            &new_dispatch,
            &new_binding.node_id,
            CfgPointKind::Binding,
        )
        || !has_local_load_point(
            cfg,
            body,
            &new_dispatch,
            new_scrutinee,
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
            get_dispatch,
            &get_expression.node_id,
            CfgPointKind::Expression,
        )
        || !has_point(
            cfg,
            get_dispatch,
            &get_binding.node_id,
            CfgPointKind::Binding,
        )
        || !has_drop_point(
            cfg,
            body,
            get_dispatch,
            old_root_set_ok_drop,
            initial_root_local,
        )
        || !has_local_load_point(
            cfg,
            body,
            get_dispatch,
            get_scrutinee,
            get_result_local,
            CfgPointKind::Condition,
        )
        || !has_point(
            cfg,
            found_dispatch,
            &found_binding.node_id,
            CfgPointKind::Binding,
        )
        || !has_point(
            cfg,
            found_dispatch,
            &unpack_binding.node_id,
            CfgPointKind::Binding,
        )
        || !has_point(
            cfg,
            found_dispatch,
            &unpack_expression.node_id,
            CfgPointKind::Expression,
        )
        || !has_drop_point(cfg, body, found_dispatch, pair_drop, get_ok_pair_local)
        || !has_drop_point(
            cfg,
            body,
            found_dispatch,
            get_ok_root_drop,
            updated_root_local,
        )
        || !has_drop_point(
            cfg,
            body,
            get_err_block,
            get_err_root_drop,
            updated_root_local,
        )
        || !has_drop_point(
            cfg,
            body,
            set_err_block,
            only_drop(set_err_arm)?,
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
        get_api: GetApi::MapGetAnyResult,
        owner: body.owner.clone(),
        proof_ensures,
        proof_ensures_expression,
        new_operation: new_expression.node_id.clone(),
        new_result_binding: new_binding.node_id.clone(),
        new_result_local: new_result_local.clone(),
        new_result_type: new_expression.ty.clone(),
        new_match: new_match.node_id.clone(),
        new_scrutinee: new_scrutinee.node_id.clone(),
        new_ok_arm: new_ok_arm.node_id.clone(),
        new_err_arm: new_err_arm.node_id.clone(),
        initial_root_pattern: initial_root_pattern.node_id.clone(),
        initial_root_local: initial_root_local.clone(),
        initial_root_type: initial_root_type.clone(),
        initial_root: resource_identity(initial_root_local),
        new_error_pattern: new_error_pattern.node_id.clone(),
        new_error_local: new_error_local.clone(),
        new_error_result: direct_arm_result(new_err_arm)?.node_id.clone(),
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
        get_match: get_match.node_id.clone(),
        get_scrutinee: get_scrutinee.node_id.clone(),
        get_ok_arm: get_ok_arm.node_id.clone(),
        get_err_arm: get_err_arm.node_id.clone(),
        get_ok_pair_pattern: get_ok_pair_pattern.node_id.clone(),
        get_ok_pair_local: get_ok_pair_local.clone(),
        get_ok_pair_type: pair_type.clone(),
        get_err_pattern: get_err_pattern.node_id.clone(),
        get_err_local: get_err_local.clone(),
        get_err_result: direct_arm_result(get_err_arm)?.node_id.clone(),
        get_pair_found_projection: found_expression.node_id.clone(),
        get_found_binding: found_binding.node_id.clone(),
        get_found_local: found_local.clone(),
        get_pair_payload_projection: unpack_argument.node_id.clone(),
        get_unpack_operation: unpack_expression.node_id.clone(),
        get_unpack_argument: unpack_argument.node_id.clone(),
        get_unpack_conversion_kind: unpack_conversion.kind,
        get_unpack_source_type: unpack_conversion.from.clone(),
        get_unpack_target_type: unpack_conversion.to.clone(),
        get_unpack_result_local: unpack_result_local.clone(),
        get_ok_pair_payload_settlement: pair_drop.node_id.clone(),
        get_ok_updated_root_drop: get_ok_root_drop.node_id.clone(),
        get_err_updated_root_drop: get_err_root_drop.node_id.clone(),
        get_found_result: get_ok_result.node_id.clone(),
        get_found_condition: condition.node_id.clone(),
        get_found_true_arm: then_block.node_id.clone(),
        get_found_false_arm: else_block.node_id.clone(),
        entry: new_dispatch,
        operations,
        cfg: ManagedAnyCfgReceipt::from_cfg(cfg, body),
    })
}

fn only_drop(arm: &MatchArm) -> Option<&ResolvedStmt> {
    let ResolvedExprKind::Block(block) = &arm.body.kind else {
        return None;
    };
    let [drop] = block.statements.as_slice() else {
        return None;
    };
    matches!(&drop.kind, ResolvedStmtKind::Drop(_)).then_some(drop)
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
        Err("ordinary Map Result-valued map_get_any i32 receipt disagrees with exact Checker source/CFG/operation attempts".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
func main() -> i32 {
    let created = map_new()
    match created {
        Ok(root) => {
            let changed = map_set(root, "answer", 42)
            match changed {
                Ok(updated) => {
                    drop(root)
                    let got = map_get_any(updated, "answer")
                    match got {
                        Ok(pair) => {
                            let found = pair.0
                            let decoded = unpack_i32(pair.1)
                            drop(pair)
                            drop(updated)
                            if found { decoded } else { 0 }
                        },
                        Err(status) => {
                            drop(updated)
                            status
                        },
                    }
                },
                Err(status) => {
                    drop(root)
                    status
                },
            }
        },
        Err(status) => status,
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

    fn main_receipt(program: &crate::core::CheckedProgram) -> Option<&Receipt> {
        program
            .callable(&NodeId("function:main".into()))?
            .resources
            .ordinary_map_set_get_any_result_i32_lineage_receipt()
    }

    #[test]
    fn result_get_any_receipt_is_independent_and_binds_both_result_arms_and_cleanup() {
        let program = checked(FIXTURE).expect("Result-valued map_get_any source checks");
        let callable = program
            .callable(&NodeId("function:main".into()))
            .expect("main callable");
        let receipt = main_receipt(&program).expect("dedicated Result-valued GetAny receipt");
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
        .expect("receipt replay binds exact source, CFG, and operation attempts");
        assert_eq!(receipt.profile_version, Version::V1);
        assert_eq!(receipt.get_api, GetApi::MapGetAnyResult);
        assert_eq!(receipt.operations.len(), 3);
        let get_attempt = receipt
            .operations
            .iter()
            .find(|attempt| attempt.point == receipt.get_operation)
            .expect("the exact map_get_any attempt is included in this receipt");
        assert_eq!(get_attempt.kind, AttemptKind::GetAny);
        assert_eq!(
            get_attempt.receiver_local,
            Some(receipt.updated_root_local.clone())
        );
        assert_eq!(
            get_attempt.result_local,
            Some(receipt.get_result_local.clone())
        );
        let (pair_type, error_type) =
            result_parts(program.resolved_types(), &receipt.get_result_type)
                .expect("GetAny result has a checked Result type");
        assert_eq!(
            *error_type,
            crate::core::ir::primitive_type_id(PrimitiveType::I32)
        );
        let ResolvedType::Tuple(pair_types) = program
            .resolved_types()
            .get(pair_type)
            .expect("GetAny Ok payload type")
        else {
            panic!("GetAny Ok payload must be a tuple");
        };
        assert_eq!(pair_types.len(), 2);
        assert_eq!(
            pair_types[0],
            crate::core::ir::primitive_type_id(PrimitiveType::Bool)
        );
        assert!(matches!(
            program.resolved_types().get(&pair_types[1]),
            Some(ResolvedType::DynamicAny { .. })
        ));
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
            receipt.get_ok_pair_payload_settlement,
            receipt.get_ok_updated_root_drop
        );
        assert_ne!(
            receipt.get_ok_pair_payload_settlement,
            receipt.get_err_updated_root_drop
        );
        assert_eq!(
            receipt.get_unpack_conversion_kind,
            CheckedConversionKind::DynamicAnyUnpack
        );
        assert_eq!(
            receipt.get_unpack_target_type,
            crate::core::ir::primitive_type_id(PrimitiveType::I32)
        );
        assert_ne!(
            receipt.get_ok_updated_root_drop,
            receipt.get_err_updated_root_drop
        );
        assert_eq!(receipt.cfg.reachable_blocks.len(), 13);
        assert_eq!(receipt.cfg.reachable_edges.len(), 16);
        assert!(callable
            .resources
            .ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt
            .is_none());
    }

    #[test]
    fn result_get_any_receipt_binds_one_direct_result_domain_ensures() {
        let source = FIXTURE.replacen(
            "func main() -> i32 {",
            "func main() -> i32 {\n    ensures: result == 42 || result == 0 || result == 2 || result == 3 || result == 4",
            1,
        );
        let program = checked(&source).expect("exact direct L2 postcondition checks");
        let callable = program
            .callable(&NodeId("function:main".into()))
            .expect("main callable");
        let receipt = main_receipt(&program).expect("L1 receipt includes the exact Ensures");
        let ensure = callable
            .body
            .root
            .statements
            .iter()
            .find(|statement| matches!(&statement.kind, ResolvedStmtKind::Contract { .. }))
            .expect("one direct Ensures statement");
        let ResolvedStmtKind::Contract { condition, .. } = &ensure.kind else {
            unreachable!();
        };
        assert_eq!(receipt.proof_ensures.as_ref(), Some(&ensure.node_id));
        assert_eq!(
            receipt.proof_ensures_expression.as_ref(),
            Some(&condition.node_id)
        );
        assert!(callable.body.locals.contains_key(&ResolvedLocalId(NodeId(
            "function:main/contract-result/local".into()
        ))));
    }

    #[test]
    fn result_get_any_receipt_keeps_a_distinct_static_miss_key() {
        let miss = FIXTURE.replace(
            "map_get_any(updated, \"answer\")",
            "map_get_any(updated, \"absent\")",
        );
        assert_ne!(miss, FIXTURE, "the miss-key mutation must apply");
        let program = checked(&miss).expect("static miss-key source checks");
        let receipt = main_receipt(&program).expect("miss path has the same exact Result receipt");
        assert_eq!(receipt.set_key_value, "answer");
        assert_eq!(receipt.get_key_value, "absent");
    }

    #[test]
    fn result_get_any_receipt_fails_closed_on_cleanup_status_unpack_and_api_near_misses() {
        let mutations = [
            (
                "Get Err skips updated-root cleanup",
                FIXTURE.replace("Err(status) => {\n                            drop(updated)", "Err(status) => {"),
            ),
            (
                "Get Ok skips pair/payload settlement",
                FIXTURE.replace("                            drop(pair)\n", ""),
            ),
            (
                "Get Ok settles pair twice",
                FIXTURE.replace("                            drop(pair)\n", "                            drop(pair)\n                            drop(pair)\n"),
            ),
            (
                "Get Ok skips updated-root cleanup",
                FIXTURE.replace("drop(pair)\n                            drop(updated)", "drop(pair)"),
            ),
            (
                "Get Err settles updated root twice",
                FIXTURE.replace(
                    "Err(status) => {\n                            drop(updated)",
                    "Err(status) => {\n                            drop(updated)\n                            drop(updated)",
                ),
            ),
            (
                "Set Ok skips old-root cleanup",
                FIXTURE.replace(
                    "Ok(updated) => {\n                    drop(root)",
                    "Ok(updated) => {",
                ),
            ),
            (
                "Get Err returns the wrong status",
                FIXTURE.replace("                            status\n", "                            0\n"),
            ),
            (
                "Set Err returns the wrong status",
                FIXTURE.replace(
                    "Err(status) => {\n                    drop(root)\n                    status",
                    "Err(status) => {\n                    drop(root)\n                    0",
                ),
            ),
            (
                "Set Err skips old-root cleanup",
                FIXTURE.replace("                Err(status) => {\n                    drop(root)", "                Err(status) => {"),
            ),
            (
                "New Err returns the wrong status",
                FIXTURE.replace("        Err(status) => status,", "        Err(status) => 0,"),
            ),
            (
                "uses legacy tuple-valued map_get",
                FIXTURE.replace("map_get_any(updated, \"answer\")", "map_get(updated, \"answer\")"),
            ),
            (
                "same callable contains an extra Map attempt",
                FIXTURE.replace(
                    "Ok(updated) => {\n                    drop(root)",
                    "Ok(updated) => {\n                    map_size(root)\n                    drop(root)",
                ),
            ),
            (
                "uses a mismatched Get key path",
                FIXTURE.replace("map_get_any(updated, \"answer\")", "map_get_any(root, \"answer\")"),
            ),
            (
                "constructs a mismatched Get Result arm",
                FIXTURE.replace(
                    "Err(status) => {\n                            drop(updated)",
                    "Ok(status) => {\n                            drop(updated)",
                ),
            ),
            (
                "uses a non-i32 Any unpack",
                FIXTURE.replace("unpack_i32(pair.1)", "unpack_string(pair.1)"),
            ),
        ];
        for (label, source) in mutations {
            assert_ne!(source, FIXTURE, "the {label} mutation must apply");
            if let Ok(program) = checked(&source) {
                assert!(
                    main_receipt(&program).is_none(),
                    "{label} must withhold the receipt"
                );
            }
        }
    }
}
