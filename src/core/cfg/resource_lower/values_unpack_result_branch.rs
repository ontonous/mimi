//! Exact read-only source/CFG receipt for the broad heterogeneous Values
//! unpack candidate. This recognizer grants inspection evidence only; the
//! generic Map/Any production route remains behind its existing readiness
//! gate.

use super::*;
use crate::core::cfg::{CfgPointKind, EdgeKind, Terminator};
use crate::core::ir::CheckedConversionKind;
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchDrop as BranchDrop,
    OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchPath as BranchPath,
    OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchPathKind as PathKind,
    OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchProfileVersion as ProfileVersion,
    OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchReceipt as BranchReceipt,
    OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchSet as SetReceipt,
};
use crate::core::{PrimitiveType, ResolvedType};

pub(super) struct Collected {
    pub(super) receipt: BranchReceipt,
    pub(super) actions: Vec<OrdinaryMapOwnedAnyV2Action>,
    pub(super) attempts: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    pub(super) root_loads: Vec<(NodeId, ResolvedLocalId)>,
}

struct SetView<'a> {
    expression: &'a ResolvedExpr,
    call_expression: &'a ResolvedExpr,
    call: &'a ResolvedCall,
    input_root: ResolvedLocalId,
    key: &'a ResolvedExpr,
    payload: &'a ResolvedExpr,
    ok_arm: &'a MatchArm,
    err_arm: &'a MatchArm,
    output_root: ResolvedLocalId,
    output_pattern: &'a ResolvedPattern,
    error_status: ResolvedLocalId,
    error_pattern: &'a ResolvedPattern,
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

fn builtin_call<'a>(expression: &'a ResolvedExpr, name: &str) -> Option<&'a ResolvedCall> {
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return None;
    };
    matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == name)
        .then_some(call)
}

fn direct_load(expression: &ResolvedExpr, local: &ResolvedLocalId, ty: &ResolvedTypeId) -> bool {
    expression.ty == *ty
        && matches!(&expression.kind, ResolvedExprKind::Load(place)
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
    let local_info = body.locals.get(local)?;
    (!local_info.mutable && local_info.ty == pattern.ty && local_info.ty == initializer.ty)
        .then_some((local, pattern, initializer))
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

fn parse_set<'a>(
    expression: &'a ResolvedExpr,
    input_root: &ResolvedLocalId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    i32_type: &ResolvedTypeId,
) -> Option<SetView<'a>> {
    let (call_expression, arms) = match_expression(expression)?;
    let call = builtin_call(call_expression, "map_set")?;
    let (root_type, error_type) = result_parts(types, &call_expression.ty)?;
    let [source, key, payload] = call.arguments.as_slice() else {
        return None;
    };
    if !call.type_arguments.is_empty()
        || call.permission.is_some()
        || !call.effects.is_empty()
        || !call.session.is_empty()
        || call.result != call_expression.ty
        || !super::ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(call_expression, call)
        || !is_map_root(types, root_type)
        || !is_map_root(types, &source.value.ty)
        || source.value.ty != *root_type
        || error_type != i32_type
        || !direct_load(&source.value, input_root, root_type)
        || !exact_identity_argument(source)
        || !exact_identity_argument(key)
        || !exact_identity_argument(payload)
        || arms.len() != 2
    {
        return None;
    }
    let ok_arm = variant_arm(arms, "Ok")?;
    let err_arm = variant_arm(arms, "Err")?;
    let (output_root, output_pattern) =
        payload_binding(ok_arm, "Ok", &call_expression.ty, root_type, body)?;
    let (error_status, error_pattern) =
        payload_binding(err_arm, "Err", &call_expression.ty, i32_type, body)?;
    if output_root == input_root
        || output_root == error_status
        || body.locals.get(output_root)?.mutable
        || body.locals.get(output_root)?.ty != *root_type
        || body.locals.get(error_status)?.mutable
    {
        return None;
    }
    Some(SetView {
        expression,
        call_expression,
        call,
        input_root: input_root.clone(),
        key: &key.value,
        payload: &payload.value,
        ok_arm,
        err_arm,
        output_root: output_root.clone(),
        output_pattern,
        error_status: error_status.clone(),
        error_pattern,
    })
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
        .collect::<Vec<_>>();
    let mut visited = 0usize;
    while let Some(block_id) = ready.pop() {
        visited += 1;
        let Some(block) = cfg.blocks.get(&block_id) else {
            return false;
        };
        let edges: Vec<_> = match &block.terminator {
            Terminator::Goto { edge } => vec![edge],
            Terminator::Match { arms, .. } => arms.iter().collect(),
            Terminator::Return { .. } => Vec::new(),
            _ => return false,
        };
        for edge_id in edges {
            let Some(edge) = cfg.edges.get(edge_id) else {
                return false;
            };
            let Some(count) = incoming.get_mut(&edge.to) else {
                return false;
            };
            *count -= 1;
            if *count == 0 {
                ready.push(edge.to.clone());
            }
        }
    }
    visited == cfg.reachable.len()
}

fn validate_cfg_shape(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    matches: &[(&ResolvedExpr, &MatchArm, &MatchArm)],
    result: &ResolvedExpr,
) -> Option<ManagedAnyCfgReceipt> {
    if cfg.owner != body.owner
        || crate::core::cfg::validate_cfg(cfg).is_err()
        || cfg.reachable.len() != cfg.blocks.len()
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != cfg.reachable
        || !cfg_is_acyclic(cfg)
    {
        return None;
    }
    let expected = matches
        .iter()
        .map(|(expression, _, _)| expression.node_id.clone())
        .collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    let mut returns = 0usize;
    let mut seen_points = BTreeSet::new();
    for (block_id, block) in &cfg.blocks {
        match &block.terminator {
            Terminator::Match { scrutinee, arms } => {
                let Some((_, ok_arm, err_arm)) = matches
                    .iter()
                    .find(|(expression, _, _)| expression.node_id == *scrutinee)
                else {
                    return None;
                };
                if !observed.insert(scrutinee.clone()) || arms.len() != 2 {
                    return None;
                }
                let mut source_arms = BTreeSet::new();
                for edge_id in arms {
                    let edge = cfg.edges.get(edge_id)?;
                    let target = cfg.blocks.get(&edge.to)?;
                    if edge.from != *block_id
                        || edge.kind != EdgeKind::MatchArm
                        || target.source.node != edge.source.node
                        || ![ok_arm.node_id.clone(), err_arm.node_id.clone()]
                            .contains(&edge.source.node)
                        || !source_arms.insert(edge.source.node.clone())
                    {
                        return None;
                    }
                }
                if source_arms != BTreeSet::from([ok_arm.node_id.clone(), err_arm.node_id.clone()])
                {
                    return None;
                }
            }
            Terminator::Goto { edge } => {
                let edge = cfg.edges.get(edge)?;
                if edge.from != *block_id || edge.kind != EdgeKind::Fallthrough {
                    return None;
                }
            }
            Terminator::Return {
                value: Some(value),
                implicit: true,
            } if value == &result.node_id => returns += 1,
            _ => return None,
        }
        for point in &block.points {
            if !seen_points.insert(point.source.node.clone()) {
                return None;
            }
        }
    }
    if observed != expected || returns != 1 {
        return None;
    }
    Some(ManagedAnyCfgReceipt::from_cfg(cfg, body))
}

fn parse_unpack<'a>(
    statement: &'a ResolvedStmt,
    body: &ResolvedBody,
    snapshot: &ResolvedLocalId,
    snapshot_type: &ResolvedTypeId,
    index_kind: usize,
    expected_target: &ResolvedTypeId,
) -> Option<(
    &'a ResolvedLocalId,
    &'a ResolvedPattern,
    &'a ResolvedExpr,
    &'a ResolvedExpr,
)> {
    let (local, pattern, initializer) = immutable_binding(statement, body)?;
    let ResolvedExprKind::Cast { value, conversion } = &initializer.kind else {
        return None;
    };
    let ResolvedExprKind::Project {
        value: list,
        projection: ResolvedValueProjection::Index(index),
    } = &value.kind
    else {
        return None;
    };
    let index_shape_matches = match (&index.kind, index_kind) {
        (ResolvedExprKind::Literal(ResolvedLiteral::Int(value)), expected) if expected < 3 => {
            usize::try_from(*value).ok() == Some(expected)
        }
        (ResolvedExprKind::Load(place), 3) => place.projections.is_empty(),
        _ => false,
    };
    let snapshot_load_matches = direct_load(list, snapshot, snapshot_type);
    let any_type = value.ty.clone();
    if !index_shape_matches
        || !snapshot_load_matches
        || conversion.kind != CheckedConversionKind::DynamicAnyUnpack
        || conversion.from != any_type
        || conversion.to != *expected_target
        || initializer.ty != *expected_target
        || !matches!(
            body.locals.get(local),
            Some(local_info) if local_info.ty == *expected_target && !local_info.mutable
        )
    {
        return None;
    }
    Some((local, pattern, initializer, index))
}

pub(super) fn collect(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
    checked_record_field_types: &BTreeMap<NodeId, Vec<(String, ResolvedTypeId)>>,
    checked_record_field_ids: &BTreeMap<NodeId, BTreeMap<String, NodeId>>,
) -> Option<Collected> {
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
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
        || body.root.statements.len() != 1
        || body.root.statements.iter().any(|statement| {
            !matches!(
                &statement.kind,
                ResolvedStmtKind::Bind {
                    pattern: ResolvedPattern {
                        kind: ResolvedPatternKind::Binding {
                            by_reference: None,
                            ..
                        },
                        ..
                    },
                    initializer: Some(_),
                }
            )
        })
    {
        return None;
    }

    let label_statement = body.root.statements.first()?;
    let (label_local, _, label_initializer) = immutable_binding(label_statement, body)?;
    let label_type = label_initializer.ty.clone();
    let ResolvedType::Nominal {
        item: label_item,
        arguments,
        ..
    } = types.get(&label_type)?
    else {
        return None;
    };
    let label_fields = checked_record_field_types.get(&NodeId(label_item.as_str().to_owned()))?;
    let label_field_ids = checked_record_field_ids.get(&NodeId(label_item.as_str().to_owned()))?;
    let ResolvedExprKind::Record {
        fields, rest: None, ..
    } = &label_initializer.kind
    else {
        return None;
    };
    if label_item.as_str() != "type:Label"
        || !arguments.is_empty()
        || label_fields.as_slice() != [("text".to_string(), string_type.clone())]
        || fields.len() != 1
        || label_field_ids.get("text") != Some(&fields[0].field)
        || fields[0].value.ty != string_type
        || !matches!(
            &fields[0].value.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::String(value))
                if value == "route discriminator"
        )
    {
        return None;
    }

    let root_result = body.root.result.as_deref()?;
    let (new_call_expression, new_arms) = match_expression(root_result)?;
    let new_call = builtin_call(new_call_expression, "map_new")?;
    let (root_type, new_error_type) = result_parts(types, &new_call_expression.ty)?;
    if !new_call.arguments.is_empty()
        || !new_call.type_arguments.is_empty()
        || new_call.permission.is_some()
        || !new_call.effects.is_empty()
        || !new_call.session.is_empty()
        || new_call.result != new_call_expression.ty
        || !super::ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(
            new_call_expression,
            new_call,
        )
        || !is_map_root(types, root_type)
        || new_error_type != &i32_type
        || new_arms.len() != 2
    {
        return None;
    }
    let new_ok = variant_arm(new_arms, "Ok")?;
    let new_err = variant_arm(new_arms, "Err")?;
    let (initial_root, _) =
        payload_binding(new_ok, "Ok", &new_call_expression.ty, root_type, body)?;
    let (new_error_status, _) =
        payload_binding(new_err, "Err", &new_call_expression.ty, &i32_type, body)?;
    if body.locals.get(initial_root)?.mutable
        || block(new_err)?.statements.len() != 0
        || !direct_load(block_result(new_err)?, new_error_status, &i32_type)
    {
        return None;
    }

    let expected_keys = ["a-number", "b-text", "z-label"];
    let set_payload_types = [i32_type.clone(), string_type.clone(), label_type.clone()];
    let mut set_views = Vec::new();
    let mut prior_roots = vec![initial_root.clone()];
    let mut next_expression = block_result(new_ok)?;
    for index in 0..3 {
        let view = parse_set(next_expression, prior_roots.last()?, body, types, &i32_type)?;
        if !matches!(
            &view.key.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::String(value))
                if value == expected_keys[index]
        ) || view.payload.ty != set_payload_types[index]
        {
            return None;
        }
        match index {
            0 if !matches!(
                &view.payload.kind,
                ResolvedExprKind::Literal(ResolvedLiteral::Int(41))
            ) =>
            {
                return None
            }
            1 if !matches!(
                &view.payload.kind,
                ResolvedExprKind::Literal(ResolvedLiteral::String(value))
                    if value == "route text discriminator"
            ) =>
            {
                return None
            }
            2 if !direct_load(view.payload, label_local, &label_type) => return None,
            _ => {}
        }
        if index < 2 {
            let ok_block = block(view.ok_arm)?;
            if !ok_block.statements.is_empty() {
                return None;
            }
            next_expression = ok_block.result.as_deref()?;
        }
        prior_roots.push(view.output_root.clone());
        set_views.push(view);
    }

    let [last_set] = set_views.get(2..)? else {
        return None;
    };
    let success = block(last_set.ok_arm)?;
    let [snapshot_statement, index_statement, literal_statement, dynamic_statement, text_statement, label_copy_statement, snapshot_drop_statement, label_copy_drop_statement, empty_drop_statement, number_drop_statement, text_root_drop_statement, root_drop_statement, label_drop_statement] =
        success.statements.as_slice()
    else {
        return None;
    };
    let (snapshot_local, _, values_expression) = immutable_binding(snapshot_statement, body)?;
    let values_call = builtin_call(values_expression, "values")?;
    let [values_source] = values_call.arguments.as_slice() else {
        return None;
    };
    if !values_call.type_arguments.is_empty()
        || values_call.permission.is_some()
        || !values_call.effects.is_empty()
        || !values_call.session.is_empty()
        || values_call.result != values_expression.ty
        || !super::ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(
            values_expression,
            values_call,
        )
        || !direct_load(&values_source.value, &last_set.output_root, root_type)
        || !exact_identity_argument(values_source)
    {
        return None;
    }
    let ResolvedType::Nominal {
        item: list_item,
        arguments: list_arguments,
        ..
    } = types.get(&values_expression.ty)?
    else {
        return None;
    };
    if list_item.as_str() != "builtin:type:List"
        || list_arguments.len() != 1
        || !matches!(
            types.get(&list_arguments[0]),
            Some(ResolvedType::DynamicAny { capability })
                if capability == "type.dynamic_value"
        )
    {
        return None;
    }
    let snapshot_type = values_expression.ty.clone();
    let (index_local, _, index_initializer) = immutable_binding(index_statement, body)?;
    if index_initializer.ty != i32_type
        || !matches!(
            &index_initializer.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
        )
        || body.locals.get(index_local)?.ty != i32_type
    {
        return None;
    }

    let (literal_local, _, literal_cast, _) = parse_unpack(
        literal_statement,
        body,
        snapshot_local,
        &snapshot_type,
        0,
        &i32_type,
    )?;
    let (dynamic_local, _, dynamic_cast, dynamic_index) = parse_unpack(
        dynamic_statement,
        body,
        snapshot_local,
        &snapshot_type,
        3,
        &i32_type,
    )?;
    let (_, _, text_cast, _) = parse_unpack(
        text_statement,
        body,
        snapshot_local,
        &snapshot_type,
        1,
        &string_type,
    )?;
    let (label_unpack_local, _, label_cast, _) = parse_unpack(
        label_copy_statement,
        body,
        snapshot_local,
        &snapshot_type,
        2,
        &label_type,
    )?;
    if !direct_load(dynamic_index, index_local, &i32_type) {
        return None;
    }
    let local_index_census = super::ordinary_map_owned_any_v2_local_source_uses(body, index_local);
    if local_index_census.has_nested_callable
        || !local_index_census.captures.is_empty()
        || !local_index_census.has_unique_load_nodes()
        || local_index_census.loads.len() != 1
        || local_index_census.loads[0].node != dynamic_index.node_id
        || local_index_census.loads[0].direct_field.is_some()
        || !local_index_census.loads[0].unprojected
        || !local_index_census.places.is_empty()
    {
        return None;
    }
    let snapshot_census = super::ordinary_map_owned_any_v2_local_source_uses(body, snapshot_local);
    if snapshot_census.has_nested_callable
        || !snapshot_census.captures.is_empty()
        || !snapshot_census.has_unique_load_nodes()
        || snapshot_census.loads.len() != 4
        || !snapshot_census
            .loads
            .iter()
            .all(|load| load.direct_field.is_none() && load.unprojected)
        || snapshot_census.places.len() != 1
        || snapshot_census.places[0].statement != snapshot_drop_statement.node_id
        || snapshot_census.places[0].place_index != 0
        || !snapshot_census.places[0].unprojected
    {
        return None;
    }
    let label_census = super::ordinary_map_owned_any_v2_local_source_uses(body, label_local);
    if label_census.has_nested_callable
        || !label_census.captures.is_empty()
        || !label_census.has_unique_load_nodes()
        || label_census.loads.len() != 1
        || label_census.loads[0].node != set_views[2].payload.node_id
        || !label_census.loads[0].unprojected
        || label_census.places.len() != 1
        || label_census.places[0].statement != label_drop_statement.node_id
        || label_census.places[0].place_index != 0
        || !label_census.places[0].unprojected
    {
        return None;
    }

    let snapshot_drop = direct_drop(
        snapshot_drop_statement,
        snapshot_local,
        &snapshot_statement.node_id,
        &snapshot_type,
    )?;
    let label_copy_drop = direct_drop(
        label_copy_drop_statement,
        label_unpack_local,
        &label_copy_statement.node_id,
        &label_type,
    )?;
    let mut error_path_drops = Vec::new();
    for (index, view) in set_views.iter().enumerate() {
        let err_block = block(view.err_arm)?;
        if err_block.statements.len() != index + 1
            || !direct_load(block_result(view.err_arm)?, &view.error_status, &i32_type)
        {
            return None;
        }
        let mut drops = Vec::new();
        for (drop_index, statement) in err_block.statements.iter().enumerate() {
            drops.push(direct_drop(
                statement,
                &prior_roots[drop_index],
                if drop_index == 0 {
                    &new_ok.pattern.node_id
                } else {
                    &set_views[drop_index - 1].output_pattern.node_id
                },
                root_type,
            )?);
        }
        error_path_drops.push(drops);
    }

    let root_success_drops = [
        direct_drop(
            empty_drop_statement,
            &prior_roots[0],
            &new_ok.pattern.node_id,
            root_type,
        )?,
        direct_drop(
            number_drop_statement,
            &prior_roots[1],
            &set_views[0].output_pattern.node_id,
            root_type,
        )?,
        direct_drop(
            text_root_drop_statement,
            &prior_roots[2],
            &set_views[1].output_pattern.node_id,
            root_type,
        )?,
        direct_drop(
            root_drop_statement,
            &prior_roots[3],
            &set_views[2].output_pattern.node_id,
            root_type,
        )?,
    ];
    let original_label_drop = direct_drop(
        label_drop_statement,
        label_local,
        &label_statement.node_id,
        &label_type,
    )?;

    let result = success.result.as_deref()?;
    let ResolvedExprKind::Binary {
        op: ResolvedBinaryOp::Subtract,
        left,
        right,
    } = &result.kind
    else {
        return None;
    };
    let ResolvedExprKind::Binary {
        op: ResolvedBinaryOp::Add,
        left: add_left,
        right: add_right,
    } = &left.kind
    else {
        return None;
    };
    if !direct_load(add_left, literal_local, &i32_type)
        || !direct_load(add_right, dynamic_local, &i32_type)
        || !matches!(
            &right.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(82))
        )
        || result.ty != i32_type
    {
        return None;
    }

    // The immutable String unpack is intentionally compiler-owned and is
    // settled at the callable epilogue; the two i32 copies are Copy values.
    // The nominal Label unpack has one explicit whole-owner Drop in this
    // exact success block. Bind its source proof to that branch-local point.
    let success_result = result.node_id.clone();
    let ordered_success_nodes = vec![
        last_set.ok_arm.pattern.node_id.clone(),
        values_source.value.node_id.clone(),
        values_expression.node_id.clone(),
        snapshot_statement.node_id.clone(),
        index_initializer.node_id.clone(),
        index_statement.node_id.clone(),
        literal_cast.node_id.clone(),
        literal_statement.node_id.clone(),
        dynamic_cast.node_id.clone(),
        dynamic_statement.node_id.clone(),
        text_cast.node_id.clone(),
        text_statement.node_id.clone(),
        label_cast.node_id.clone(),
        label_copy_statement.node_id.clone(),
        snapshot_drop_statement.node_id.clone(),
        label_copy_drop_statement.node_id.clone(),
        empty_drop_statement.node_id.clone(),
        number_drop_statement.node_id.clone(),
        text_root_drop_statement.node_id.clone(),
        root_drop_statement.node_id.clone(),
        label_drop_statement.node_id.clone(),
        success_result.clone(),
    ];
    let mut matches = vec![(new_call_expression, new_ok, new_err)];
    for view in &set_views {
        matches.push((view.call_expression, view.ok_arm, view.err_arm));
    }
    let cfg_receipt = validate_cfg_shape(cfg, body, &matches, root_result)?;
    let point_kind_exact = |point: &NodeId, kind: CfgPointKind| {
        let matches = cfg
            .blocks
            .values()
            .flat_map(|block| &block.points)
            .filter(|candidate| candidate.source.node == *point)
            .collect::<Vec<_>>();
        matches.len() == 1 && matches[0].kind == kind
    };
    if !ordered_success_nodes.iter().all(|node| {
        point_kind_exact(node, CfgPointKind::Expression)
            || point_kind_exact(node, CfgPointKind::Binding)
            || point_kind_exact(node, CfgPointKind::ResourceAction)
    }) || !crate::core::cfg::validate_cfg(cfg).is_ok()
    {
        return None;
    }
    let success_block = cfg.blocks.values().find(|block| {
        block
            .points
            .iter()
            .any(|point| point.source.node == snapshot_drop.settlement_point)
    })?;
    let success_order = success_block
        .points
        .iter()
        .map(|point| point.source.node.clone())
        .collect::<Vec<_>>();
    let mut previous = None;
    for source in &ordered_success_nodes {
        let position = success_order.iter().position(|point| point == source)?;
        if previous.is_some_and(|old| position <= old) {
            return None;
        }
        previous = Some(position);
    }

    let label_unpack_census =
        super::ordinary_map_owned_any_v2_local_source_uses(body, label_unpack_local);
    if label_unpack_census.has_nested_callable
        || !label_unpack_census.captures.is_empty()
        || !label_unpack_census.has_unique_load_nodes()
        || !label_unpack_census.loads.is_empty()
        || label_unpack_census.places.len() != 1
        || label_unpack_census.places[0].statement != label_copy_drop.settlement_point
        || label_unpack_census.places[0].kind != OrdinaryMapOwnedAnyV2LocalPlaceUseKind::Drop
        || !label_unpack_census.places[0].unprojected
        || !ordinary_map_owned_any_v2_cfg_points_strictly_ordered_in_same_block(
            cfg,
            &[
                &label_copy_statement.node_id,
                &label_copy_drop.settlement_point,
            ],
        )
    {
        return None;
    }

    let new_action = {
        let mut action = OrdinaryMapOwnedAnyV2Checker::build_action(
            OrdinaryMapOwnedAnyV2ActionKind::New,
            new_call_expression.node_id.clone(),
            initial_root.clone(),
            None,
            None,
            None,
            None,
            false,
            None,
        );
        action.source_node = Some(new_call_expression.node_id.clone());
        action.source_type = Some(new_call_expression.ty.clone());
        action
    };
    let mut actions = vec![new_action];
    let mut root_loads = Vec::new();
    let mut attempts = BTreeSet::new();
    attempts.insert(attempt(
        OrdinaryMapOwnedAnyV2AttemptKind::New,
        &new_call_expression.node_id,
        &[],
        None,
        Some(initial_root),
    ));
    for view in &set_views {
        let mut action = OrdinaryMapOwnedAnyV2Checker::build_action(
            OrdinaryMapOwnedAnyV2ActionKind::Set,
            view.call_expression.node_id.clone(),
            view.output_root.clone(),
            Some(view.key.node_id.clone()),
            Some(view.key.ty.clone()),
            Some(view.payload.node_id.clone()),
            Some(view.payload.ty.clone()),
            true,
            Some(view.input_root.clone()),
        );
        action.source_node = Some(view.call.arguments[0].value.node_id.clone());
        action.source_type = Some(view.call.arguments[0].value.ty.clone());
        action.key_provenance =
            Some(crate::core::OrdinaryMapOwnedAnyV2KeyProvenance::StringLiteral);
        let ResolvedExprKind::Literal(ResolvedLiteral::String(key)) = &view.key.kind else {
            return None;
        };
        action.key_static_digest = Some(blake3::hash(key.as_bytes()).to_hex().to_string());
        root_loads.push((
            view.call.arguments[0].value.node_id.clone(),
            view.input_root.clone(),
        ));
        attempts.insert(attempt(
            OrdinaryMapOwnedAnyV2AttemptKind::Set,
            &view.call_expression.node_id,
            &view
                .call
                .arguments
                .iter()
                .map(|arg| arg.value.node_id.clone())
                .collect::<Vec<_>>(),
            Some(&view.input_root),
            Some(&view.output_root),
        ));
        actions.push(action);
    }
    let mut values_action = OrdinaryMapOwnedAnyV2Checker::build_action(
        OrdinaryMapOwnedAnyV2ActionKind::Values,
        values_expression.node_id.clone(),
        last_set.output_root.clone(),
        None,
        None,
        None,
        None,
        false,
        Some(last_set.output_root.clone()),
    );
    values_action.source_node = Some(values_source.value.node_id.clone());
    values_action.source_type = Some(root_type.clone());
    values_action.result_type = Some(snapshot_type.clone());
    values_action.result_local = Some(snapshot_local.clone());
    actions.push(values_action);
    root_loads.push((
        values_source.value.node_id.clone(),
        last_set.output_root.clone(),
    ));
    attempts.insert(attempt(
        OrdinaryMapOwnedAnyV2AttemptKind::Values,
        &values_expression.node_id,
        std::slice::from_ref(&values_source.value.node_id),
        Some(&last_set.output_root),
        Some(snapshot_local),
    ));

    let success_drops = [
        snapshot_drop.clone(),
        label_copy_drop.clone(),
        root_success_drops[0].clone(),
        root_success_drops[1].clone(),
        root_success_drops[2].clone(),
        root_success_drops[3].clone(),
        original_label_drop.clone(),
    ];
    let root_drops = error_path_drops
        .iter()
        .flatten()
        .cloned()
        .chain(root_success_drops.iter().cloned())
        .collect::<Vec<_>>();
    for drop in &root_drops {
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
        attempts.insert(attempt(
            OrdinaryMapOwnedAnyV2AttemptKind::Drop,
            &drop.settlement_point,
            std::slice::from_ref(&drop.local.0),
            Some(&drop.local),
            None,
        ));
    }
    if actions.len() != 15 || attempts.len() != 15 || root_drops.len() != 10 {
        return None;
    }

    let cfg_order = OrdinaryMapOwnedAnyV2Checker::acyclic_v2_cfg_order(cfg)?;
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
    let ordered_action_points = actions.iter().map(|action| action.point.clone()).collect();

    let mut paths = vec![BranchPath {
        arm_path: vec![new_err.node_id.clone()],
        result: block_result(new_err)?.node_id.clone(),
        kind: PathKind::MapNewErr,
        drops: Vec::new(),
    }];
    for index in 0..3 {
        let view = &set_views[index];
        let drops = error_path_drops[index].clone();
        paths.push(BranchPath {
            arm_path: std::iter::once(new_ok.node_id.clone())
                .chain(
                    set_views[..index]
                        .iter()
                        .map(|set| set.ok_arm.node_id.clone()),
                )
                .chain(std::iter::once(view.err_arm.node_id.clone()))
                .collect(),
            result: block_result(view.err_arm)?.node_id.clone(),
            kind: match index {
                0 => PathKind::MapSetErr0,
                1 => PathKind::MapSetErr1,
                _ => PathKind::MapSetErr2,
            },
            drops,
        });
    }
    paths.push(BranchPath {
        arm_path: std::iter::once(new_ok.node_id.clone())
            .chain(set_views.iter().map(|set| set.ok_arm.node_id.clone()))
            .collect(),
        result: success_result,
        kind: PathKind::Success,
        drops: success_drops.to_vec(),
    });

    let sets = set_views
        .iter()
        .map(|view| {
            let ResolvedExprKind::Literal(ResolvedLiteral::String(key_literal)) = &view.key.kind
            else {
                return None;
            };
            Some(SetReceipt {
                operation: view.call_expression.node_id.clone(),
                match_node: view.expression.node_id.clone(),
                ok_arm: view.ok_arm.node_id.clone(),
                err_arm: view.err_arm.node_id.clone(),
                ok_pattern: view.output_pattern.node_id.clone(),
                err_pattern: view.error_pattern.node_id.clone(),
                input_root: view.input_root.clone(),
                output_root: view.output_root.clone(),
                error_status: view.error_status.clone(),
                key_node: view.key.node_id.clone(),
                key_literal: key_literal.clone(),
                payload_node: view.payload.node_id.clone(),
                payload_type: view.payload.ty.clone(),
            })
        })
        .collect::<Option<Vec<_>>>()?;

    Some(Collected {
        receipt: BranchReceipt {
            profile_version: ProfileVersion::V1,
            owner: body.owner.clone(),
            map_new_operation: new_call_expression.node_id.clone(),
            map_new_match: root_result.node_id.clone(),
            map_new_ok_arm: new_ok.node_id.clone(),
            map_new_err_arm: new_err.node_id.clone(),
            initial_root: initial_root.clone(),
            map_new_error_status: new_error_status.clone(),
            sets,
            values_operation: values_expression.node_id.clone(),
            values_root: last_set.output_root.clone(),
            snapshot_binding_point: snapshot_statement.node_id.clone(),
            snapshot_local: snapshot_local.clone(),
            snapshot_type,
            index_binding_point: index_statement.node_id.clone(),
            index_local: index_local.clone(),
            index_initializer: index_initializer.node_id.clone(),
            index_expression: dynamic_index.node_id.clone(),
            unpack_casts: vec![
                literal_cast.node_id.clone(),
                dynamic_cast.node_id.clone(),
                text_cast.node_id.clone(),
                label_cast.node_id.clone(),
            ],
            label_binding_point: label_statement.node_id.clone(),
            label_local: label_local.clone(),
            label_type: label_type.clone(),
            label_unpack_local: label_unpack_local.clone(),
            label_unpack_binding_point: label_copy_statement.node_id.clone(),
            label_unpack_drop: label_copy_drop.settlement_point.clone(),
            snapshot_drop: snapshot_drop.settlement_point.clone(),
            result: success.result.as_deref()?.node_id.clone(),
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
    checked_record_field_types: &BTreeMap<NodeId, Vec<(String, ResolvedTypeId)>>,
    checked_record_field_ids: &BTreeMap<NodeId, BTreeMap<String, NodeId>>,
    actions: &[OrdinaryMapOwnedAnyV2Action],
    attempts: &BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
) -> Result<(), String> {
    let collected = collect(
        cfg,
        body,
        signature,
        types,
        checked_record_field_types,
        checked_record_field_ids,
    )
    .ok_or_else(|| {
        "broad Values-unpack Result source or CFG no longer matches its exact inspection profile"
            .to_string()
    })?;
    if &collected.receipt != receipt {
        return Err("broad Values-unpack Result receipt differs from Checker replay".into());
    }
    if collected.actions != actions || collected.attempts != *attempts {
        return Err(
            "broad Values-unpack Result actions or attempts differ from Checker replay".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked_fixture(source: &str) -> crate::core::CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("broad Values-unpack fixture lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("broad Values-unpack fixture parses");
        crate::core::check_program(&file)
            .unwrap_or_else(|errors| panic!("broad Values-unpack fixture checks: {errors:?}"))
    }

    #[test]
    fn broad_values_unpack_result_receipt_is_source_bound_and_exact() {
        let source = include_str!(
            "../../../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_unpack.mimi"
        );
        let checked = checked_fixture(source);
        let owner = NodeId("function:main".into());
        let callable = checked.callable(&owner).expect("main callable");
        let analysis = checked.resource_analysis(&owner).expect("main resources");
        let cfg_receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("broad Values-unpack source owns a V2 CFG receipt");
        let receipt = cfg_receipt
            .values_unpack_result_branch_receipt
            .as_ref()
            .expect("exact broad Values-unpack inspection receipt");
        assert_eq!(receipt.sets.len(), 3);
        assert_eq!(receipt.paths.len(), 5);
        assert_eq!(cfg_receipt.values_unpack_receipts.len(), 4);
        assert_eq!(cfg_receipt.actions.len(), 15);
        assert_eq!(cfg_receipt.attempts.len(), 15);
        assert_eq!(
            receipt
                .sets
                .iter()
                .map(|set| set.key_literal.as_str())
                .collect::<Vec<_>>(),
            ["a-number", "b-text", "z-label"]
        );
        let ResolvedType::Nominal { item, .. } = checked
            .resolved_types()
            .get(&receipt.label_type)
            .expect("label nominal type")
        else {
            panic!("label payload is not nominal")
        };
        let checked_record_field_types = BTreeMap::from([(
            NodeId(item.as_str().to_owned()),
            vec![(
                "text".to_string(),
                crate::core::ir::primitive_type_id(PrimitiveType::String),
            )],
        )]);
        let definition = checked
            .type_defs()
            .get(&NodeId(item.as_str().to_owned()))
            .expect("Label checked declaration");
        let checked_record_field_ids = BTreeMap::from([(
            NodeId(item.as_str().to_owned()),
            definition.field_ids.clone(),
        )]);
        assert!(validate(
            receipt,
            &callable.cfg,
            &callable.body,
            &callable.signature,
            checked.resolved_types(),
            &checked_record_field_types,
            &checked_record_field_ids,
            &cfg_receipt.actions,
            &cfg_receipt.attempts,
        )
        .is_ok());

        let mut forged_body = callable.body.clone();
        let ResolvedStmtKind::Bind {
            initializer: Some(initializer),
            ..
        } = &mut forged_body.root.statements[0].kind
        else {
            panic!("Label binding has an initializer")
        };
        let ResolvedExprKind::Record { fields, .. } = &mut initializer.kind else {
            panic!("Label initializer is a record")
        };
        fields[0].field = NodeId("forged:Label/field:text".into());
        assert!(
            collect(
                &callable.cfg,
                &forged_body,
                &callable.signature,
                checked.resolved_types(),
                &checked_record_field_types,
                &checked_record_field_ids,
            )
            .is_none(),
            "a forged resolved field identity must not receive the receipt"
        );
    }

    #[test]
    fn broad_values_unpack_result_receipt_rejects_near_miss_source_shapes() {
        let source = include_str!(
            "../../../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_unpack.mimi"
        );
        let near_misses = [
            source
                .replace("type Label", "type Other")
                .replace("Label {", "Other {")
                .replace("as Label", "as Other"),
            source.replace("\"z-label\"", "\"a-label\""),
            source.replace(
                "drop(with_number)\n        drop(with_text)",
                "drop(with_text)",
            ),
            source.replace("let index = 0", "let index = 1"),
            source.replace("drop(label_copy)", "drop(text_copy)"),
            source.replace("snapshot[2] as Label", "snapshot[2] as string"),
            source.replace(
                "literal_value + dynamic_value - 82",
                "literal_value + dynamic_value - 81",
            ),
        ];
        for (index, source) in near_misses.iter().enumerate() {
            let tokens = crate::lexer::Lexer::new(source)
                .tokenize()
                .expect("near-miss Values fixture lexes");
            let file = crate::parser::Parser::new(tokens)
                .parse_file()
                .expect("near-miss Values fixture parses");
            let Ok(checked) = crate::core::check_program(&file) else {
                continue;
            };
            let owner = NodeId("function:main".into());
            let callable = checked.callable(&owner).expect("main callable");
            let Some(analysis) = checked.resource_analysis(&owner) else {
                continue;
            };
            let accepted = analysis
                .ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()
                .and_then(|cfg| cfg.values_unpack_result_branch_receipt.as_ref())
                .is_some();
            assert!(
                !accepted,
                "near miss {index} acquired a broad Values receipt"
            );
            assert!(
                callable
                    .resources
                    .ordinary_map_owned_any_v2_cfg_receipt
                    .is_none()
                    || callable
                        .resources
                        .ordinary_map_owned_any_v2_cfg_receipt
                        .as_ref()
                        .is_none_or(|cfg| cfg.values_unpack_result_branch_receipt.is_none()),
                "near miss {index} retained the dedicated broad Values receipt"
            );
        }
    }
}
