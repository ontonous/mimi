//! Exact L1 source/CFG authority for the nested-Result ordinary Map direct
//! print fixture. This proof is intentionally narrower than the general V2
//! visitor: it accepts one `map_new`, one persistent `map_set`, one tuple
//! `map_get`, and exactly `println(pair.1)` with branch-local root settlement.

use super::*;
use crate::core::cfg::{BasicBlockId, CfgPointKind, EdgeId};
use crate::core::ir::{CheckedConversionKind, PrimitiveType};
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2ActionKind as ActionKind,
    OrdinaryMapOwnedAnyV2Attempt as Attempt, OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
    OrdinaryMapOwnedAnyV2DirectPrintResultProfileVersion as Version,
    OrdinaryMapOwnedAnyV2DirectPrintResultReceipt as Receipt,
    OrdinaryMapOwnedAnyV2DirectPrintResultShape as Shape,
    OrdinaryMapOwnedAnyV2DirectPrintSetPayload as SetPayload,
    OrdinaryMapOwnedAnyV2PrintlnObservation, OrdinaryMapOwnedAnyV2ProfileVersion,
    OrdinaryMapOwnedAnyV2ResultReceipt, OrdinaryMapOwnedAnyV2ResultSettlementKind,
};
use crate::core::LocalId;

fn builtin_call<'a>(expression: &'a ResolvedExpr, name: &str) -> Option<&'a ResolvedCall> {
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return None;
    };
    (matches!(&call.callee, ResolvedCallee::Builtin(id) if id.as_str() == name)
        && call.result == expression.ty
        && call.permission.is_none()
        && call.type_arguments.is_empty()
        && call.session.is_empty())
    .then_some(call)
}

fn identity_argument(argument: &crate::core::ir::ResolvedArgument, ty: &ResolvedTypeId) -> bool {
    argument.value.ty == *ty
        && argument.conversion.kind == CheckedConversionKind::Identity
        && argument.conversion.from == *ty
        && argument.conversion.to == *ty
}

fn finite_effects(effects: &[crate::core::ir::EffectId]) -> bool {
    effects.is_empty() || (effects.len() == 1 && effects[0].as_str() == "shared.modify")
}

fn set_payload(
    expression: &ResolvedExpr,
    i32_type: &ResolvedTypeId,
    string_type: &ResolvedTypeId,
) -> Option<(ResolvedTypeId, SetPayload)> {
    match (&expression.kind, &expression.ty) {
        (ResolvedExprKind::Literal(ResolvedLiteral::Int(42)), ty) if ty == i32_type => {
            Some((ty.clone(), SetPayload::I32(42)))
        }
        (ResolvedExprKind::Literal(ResolvedLiteral::String(value)), ty)
            if ty == string_type && value == "borrowed String λ" =>
        {
            Some((ty.clone(), SetPayload::String(value.clone())))
        }
        _ => None,
    }
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
            && matches!(&arm.pattern.kind, ResolvedPatternKind::Constructor { variant, .. }
                if variant.0 == expected)
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

fn direct_result(arm: &MatchArm) -> Option<&ResolvedExpr> {
    match &arm.body.kind {
        ResolvedExprKind::Block(block) if block.statements.is_empty() => block.result.as_deref(),
        ResolvedExprKind::Block(_) => None,
        _ => Some(&arm.body),
    }
}

fn exact_load(expression: &ResolvedExpr, local: &ResolvedLocalId, ty: &ResolvedTypeId) -> bool {
    expression.ty == *ty
        && matches!(&expression.kind, ResolvedExprKind::Load(place)
            if place.base == *local && place.projections.is_empty())
}

fn drop_local(statement: &ResolvedStmt, local: &ResolvedLocalId) -> bool {
    matches!(&statement.kind, ResolvedStmtKind::Drop(places)
        if places.len() == 1
            && places[0].base == *local
            && places[0].projections.is_empty())
}

fn binding<'a>(
    statement: &'a ResolvedStmt,
    body: &ResolvedBody,
) -> Option<(&'a ResolvedLocalId, &'a ResolvedExpr)> {
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
        .then_some((local, initializer))
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

fn match_dispatch(cfg: &CallableCfg, scrutinee: &NodeId) -> Option<BasicBlockId> {
    let mut found = cfg.blocks.iter().filter_map(|(id, block)| {
        matches!(&block.terminator, Terminator::Match { scrutinee: node, arms }
            if node == scrutinee && arms.len() == 2)
        .then_some(id.clone())
    });
    let id = found.next()?;
    found.next().is_none().then_some(id)
}

fn point_location<'a>(cfg: &'a CallableCfg, node: &NodeId) -> Option<&'a BasicBlockId> {
    let mut found = cfg.blocks.iter().filter_map(|(id, block)| {
        block
            .points
            .iter()
            .any(|point| point.source.node == *node)
            .then_some(id)
    });
    let block = found.next()?;
    found.next().is_none().then_some(block)
}

fn point_index(cfg: &CallableCfg, block: &BasicBlockId, node: &NodeId) -> Option<usize> {
    cfg.blocks
        .get(block)?
        .points
        .iter()
        .position(|point| point.source.node == *node)
}

fn has_point(cfg: &CallableCfg, block: &BasicBlockId, node: &NodeId, kind: CfgPointKind) -> bool {
    cfg.blocks.get(block).is_some_and(|basic_block| {
        let mut points = basic_block
            .points
            .iter()
            .filter(|point| point.source.node == *node);
        points.next().is_some_and(|point| point.kind == kind) && points.next().is_none()
    })
}

fn has_unique_point(cfg: &CallableCfg, node: &NodeId) -> bool {
    point_location(cfg, node).is_some()
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
                && point.read_places[0].base == LocalId(local.0.clone())
                && point.read_places[0].projections.is_empty()
                && point.write_places.is_empty()
                && points.next().is_none()
        })
    })
}

fn path_reaches_return(cfg: &CallableCfg, start: &BasicBlockId, function_result: &NodeId) -> bool {
    let mut current = start.clone();
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return false;
        }
        let Some(block) = cfg.blocks.get(&current) else {
            return false;
        };
        match &block.terminator {
            Terminator::Return {
                value: Some(value),
                implicit: true,
            } => return value == function_result,
            Terminator::Goto { edge } => {
                let Some(edge) = cfg.edges.get(edge) else {
                    return false;
                };
                if edge.from != current || edge.kind != EdgeKind::Fallthrough {
                    return false;
                }
                current = edge.to.clone();
            }
            _ => return false,
        }
    }
}

fn tuple_payload_projection(
    expression: &ResolvedExpr,
    pair: &ResolvedLocalId,
    any_type: &ResolvedTypeId,
) -> bool {
    expression.ty == *any_type
        && matches!(&expression.kind, ResolvedExprKind::Load(place)
            if place.base == *pair
                && matches!(place.projections.as_slice(), [ResolvedProjection::Tuple { index: 1, ty }]
                    if ty == any_type))
}

pub(super) fn collect(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
    operations: &BTreeSet<Attempt>,
) -> Option<Receipt> {
    let owner = NodeId("function:main".into());
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    let unit_type = crate::core::ir::primitive_type_id(PrimitiveType::Unit);
    if cfg.owner != owner
        || body.owner != owner
        || signature.owner != owner
        || signature.result != i32_type
        || body.root.ty != i32_type
        || !signature.parameters.is_empty()
        || !signature.generic_parameters.is_empty()
        || !body.parameters.is_empty()
        || !body.captures.is_empty()
        || !body.place_inputs.is_empty()
        || !body.default_values.is_empty()
        || !body.root.statements.is_empty()
        || signature.effects.len() != 1
        || signature.effects[0].as_str() != "shared.modify"
    {
        return None;
    }

    let root_expression = body.root.result.as_deref()?;
    let ResolvedExprKind::Match {
        scrutinee: new_expression,
        arms: new_arms,
    } = &root_expression.kind
    else {
        return None;
    };
    let new_call = builtin_call(new_expression, "map_new")?;
    if !new_call.arguments.is_empty()
        || !new_expression.effects.is_empty()
        || !new_call.effects.is_empty()
        || root_expression.ty != i32_type
        || new_arms.len() != 2
    {
        return None;
    }
    let (initial_root_type, new_status_type) = result_parts(types, &new_expression.ty)?;
    if new_status_type != &i32_type || !record_type(types, initial_root_type) {
        return None;
    }
    let new_ok_arm = variant_arm(new_arms, "Ok")?;
    let new_error_arm = variant_arm(new_arms, "Err")?;
    let (initial_root_local, new_ok_pattern) = payload(
        new_ok_arm,
        "Ok",
        &new_expression.ty,
        initial_root_type,
        body,
    )?;
    let (new_error_local, new_error_pattern) =
        payload(new_error_arm, "Err", &new_expression.ty, &i32_type, body)?;
    if initial_root_local == new_error_local
        || !matches!(
            direct_result(new_error_arm)?.kind,
            ResolvedExprKind::Load(_)
        )
        || !exact_load(direct_result(new_error_arm)?, new_error_local, &i32_type)
    {
        return None;
    }

    let new_ok_block = match &new_ok_arm.body.kind {
        ResolvedExprKind::Block(block) if block.statements.is_empty() => block,
        _ => return None,
    };
    let set_match_expression = new_ok_block.result.as_deref()?;
    let ResolvedExprKind::Match {
        scrutinee: set_expression,
        arms: set_arms,
    } = &set_match_expression.kind
    else {
        return None;
    };
    let set_call = builtin_call(set_expression, "map_set")?;
    let [set_receiver, set_key, set_value] = set_call.arguments.as_slice() else {
        return None;
    };
    let set_key_value = match &set_key.value.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::String(value)) => value.clone(),
        _ => return None,
    };
    let (set_payload_type, set_payload) = set_payload(&set_value.value, &i32_type, &string_type)?;
    let (set_root_type, set_status_type) = result_parts(types, &set_expression.ty)?;
    if set_status_type != &i32_type
        || set_root_type != initial_root_type
        || !record_type(types, set_root_type)
        || set_expression.ty != new_expression.ty
        || set_arms.len() != 2
        || !finite_effects(&set_call.effects)
        || !finite_effects(&set_expression.effects)
        || !identity_argument(set_receiver, initial_root_type)
        || !exact_load(&set_receiver.value, initial_root_local, initial_root_type)
        || set_key.value.ty != string_type
        || set_key.conversion.kind != CheckedConversionKind::Identity
        || set_key.conversion.from != string_type
        || set_key.conversion.to != string_type
        || set_key_value != "answer"
        || set_value.value.ty != set_payload_type
        || set_value.conversion.kind != CheckedConversionKind::Identity
        || set_value.conversion.from != set_payload_type
        || set_value.conversion.to != set_payload_type
    {
        return None;
    }
    let set_ok_arm = variant_arm(set_arms, "Ok")?;
    let set_error_arm = variant_arm(set_arms, "Err")?;
    let (updated_root_local, set_ok_pattern) =
        payload(set_ok_arm, "Ok", &set_expression.ty, set_root_type, body)?;
    let (set_error_local, set_error_pattern) =
        payload(set_error_arm, "Err", &set_expression.ty, &i32_type, body)?;
    if updated_root_local == initial_root_local
        || updated_root_local == set_error_local
        || set_error_local == initial_root_local
    {
        return None;
    }

    let set_ok_block = match &set_ok_arm.body.kind {
        ResolvedExprKind::Block(block) => block,
        _ => return None,
    };
    let (
        pair_binding_statement,
        print_statement,
        set_ok_old_root_drop,
        root_get_drop,
        pair_drop,
        size_binding_statement,
    ) = match set_ok_block.statements.as_slice() {
        [pair_binding_statement, old_root_drop, root_drop, print_statement, pair_drop] => (
            pair_binding_statement,
            print_statement,
            old_root_drop,
            root_drop,
            pair_drop,
            None,
        ),
        [pair_binding_statement, print_statement, size_binding_statement, pair_drop, old_root_drop, root_drop] => {
            (
                pair_binding_statement,
                print_statement,
                old_root_drop,
                root_drop,
                pair_drop,
                Some(size_binding_statement),
            )
        }
        _ => return None,
    };
    let (pair_local, get_expression) = binding(pair_binding_statement, body)?;
    let get_call = builtin_call(get_expression, "map_get")?;
    let [get_receiver, get_key] = get_call.arguments.as_slice() else {
        return None;
    };
    let ResolvedType::Tuple(pair_elements) = types.get(&get_expression.ty)? else {
        return None;
    };
    let [found_type, any_type] = pair_elements.as_slice() else {
        return None;
    };
    let get_key_value = match &get_key.value.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::String(value)) => value.clone(),
        _ => return None,
    };
    if found_type != &bool_type
        || !matches!(types.get(any_type), Some(ResolvedType::DynamicAny { .. }))
        || body
            .locals
            .get(pair_local)
            .is_none_or(|local| local.ty != get_expression.ty || local.mutable)
        || !identity_argument(get_receiver, set_root_type)
        || !exact_load(&get_receiver.value, updated_root_local, set_root_type)
        || get_key.value.ty != string_type
        || get_key.conversion.kind != CheckedConversionKind::Identity
        || get_key.conversion.from != string_type
        || get_key.conversion.to != string_type
        || get_expression.ty != get_call.result
        || !get_expression.effects.is_empty()
        || !get_call.effects.is_empty()
        || !drop_local(set_ok_old_root_drop, initial_root_local)
        || !drop_local(root_get_drop, updated_root_local)
        || !drop_local(pair_drop, pair_local)
    {
        return None;
    }
    let (size_operation, size_receiver, size_binding, size_local) =
        if let Some(size_statement) = size_binding_statement {
            let (size_local, size_expression) = binding(size_statement, body)?;
            let size_call = builtin_call(size_expression, "map_size")?;
            let [size_argument] = size_call.arguments.as_slice() else {
                return None;
            };
            if size_local == pair_local
                || body
                    .locals
                    .get(size_local)
                    .is_none_or(|local| local.ty != i32_type || local.mutable)
                || size_expression.ty != i32_type
                || size_call.result != i32_type
                || !size_expression.effects.is_empty()
                || !size_call.effects.is_empty()
                || !identity_argument(size_argument, set_root_type)
                || !exact_load(&size_argument.value, updated_root_local, set_root_type)
            {
                return None;
            }
            (
                Some(size_expression),
                Some(size_argument.value.node_id.clone()),
                Some(size_statement),
                Some(size_local),
            )
        } else {
            (None, None, None, None)
        };
    let shape = match (&set_payload, get_key_value.as_str(), size_local.is_some()) {
        (SetPayload::I32(42), "answer", false) => Shape::HitI32,
        (SetPayload::String(_), "answer", false) => Shape::HitString,
        (SetPayload::I32(42), "missing", false) => Shape::MissI32,
        (SetPayload::I32(42), "answer", true) => Shape::SameRootMapSizeI32,
        _ => return None,
    };
    let ResolvedStmtKind::Expr(print_expression) = &print_statement.kind else {
        return None;
    };
    let print_call = builtin_call(print_expression, "println")?;
    let [print_argument] = print_call.arguments.as_slice() else {
        return None;
    };
    if print_call.result != unit_type
        || print_expression.ty != unit_type
        || !finite_effects(&print_call.effects)
        || !finite_effects(&print_expression.effects)
        || print_argument.parameter.0 != NodeId("builtin:println/parameter:0".into())
        || print_argument.value.ty != *any_type
        || print_argument.conversion.kind != CheckedConversionKind::Identity
        || print_argument.conversion.from != *any_type
        || print_argument.conversion.to != *any_type
        || !tuple_payload_projection(&print_argument.value, pair_local, any_type)
    {
        return None;
    }
    let success_result = set_ok_block.result.as_deref()?;
    let success_result_matches = match &size_local {
        Some(size_local) => exact_load(success_result, size_local, &i32_type),
        None => {
            success_result.ty == i32_type
                && matches!(
                    &success_result.kind,
                    ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
                )
        }
    };
    if !success_result_matches {
        return None;
    }

    let set_error_block = match &set_error_arm.body.kind {
        ResolvedExprKind::Block(block) => block,
        _ => return None,
    };
    let [set_error_input_drop] = set_error_block.statements.as_slice() else {
        return None;
    };
    let set_error_return = set_error_block.result.as_deref()?;
    if !drop_local(set_error_input_drop, initial_root_local)
        || !exact_load(set_error_return, set_error_local, &i32_type)
    {
        return None;
    }
    let new_error_block = match &new_error_arm.body.kind {
        ResolvedExprKind::Block(block) if block.statements.is_empty() => block,
        _ => return None,
    };
    let new_error_return = new_error_block.result.as_deref()?;
    if !exact_load(new_error_return, new_error_local, &i32_type) {
        return None;
    }

    let mut expected_locals = BTreeSet::from([
        initial_root_local.clone(),
        new_error_local.clone(),
        updated_root_local.clone(),
        set_error_local.clone(),
        pair_local.clone(),
    ]);
    if let Some(size_local) = &size_local {
        if !expected_locals.insert((*size_local).clone()) {
            return None;
        }
    }
    if body.locals.keys().cloned().collect::<BTreeSet<_>>() != expected_locals {
        return None;
    }

    let mut expected_operations = BTreeSet::from([
        Attempt {
            kind: AttemptKind::New,
            point: new_expression.node_id.clone(),
            arguments: Vec::new(),
            receiver_local: None,
            result_local: None,
        },
        Attempt {
            kind: AttemptKind::Set,
            point: set_expression.node_id.clone(),
            arguments: vec![
                set_receiver.value.node_id.clone(),
                set_key.value.node_id.clone(),
                set_value.value.node_id.clone(),
            ],
            receiver_local: Some(initial_root_local.clone()),
            result_local: None,
        },
        Attempt {
            kind: AttemptKind::GetAny,
            point: get_expression.node_id.clone(),
            arguments: vec![
                get_receiver.value.node_id.clone(),
                get_key.value.node_id.clone(),
            ],
            receiver_local: Some(updated_root_local.clone()),
            result_local: Some(pair_local.clone()),
        },
    ]);
    if let (Some(size_operation), Some(size_receiver)) = (&size_operation, &size_receiver) {
        expected_operations.insert(Attempt {
            kind: AttemptKind::Size,
            point: size_operation.node_id.clone(),
            arguments: vec![(*size_receiver).clone()],
            receiver_local: Some(updated_root_local.clone()),
            result_local: None,
        });
    }
    if operations != &expected_operations {
        return None;
    }

    if cfg.reachable.len() != cfg.blocks.len()
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != cfg.reachable
        || crate::core::cfg::validate_cfg(cfg).is_err()
        || cfg.blocks.values().any(|block| {
            !matches!(
                block.terminator,
                Terminator::Match { .. } | Terminator::Goto { .. } | Terminator::Return { .. }
            )
        })
    {
        return None;
    }
    let source_nodes = root_expression
        .eager_source_nodes()
        .into_iter()
        .collect::<BTreeSet<_>>();
    let points = cfg
        .blocks
        .values()
        .flat_map(|block| block.points.iter())
        .collect::<Vec<_>>();
    let point_nodes = points
        .iter()
        .map(|point| point.source.node.clone())
        .collect::<BTreeSet<_>>();
    if point_nodes.len() != points.len()
        || points
            .iter()
            .any(|point| !source_nodes.contains(&point.source.node))
        || cfg
            .blocks
            .values()
            .filter(|block| matches!(block.terminator, Terminator::Match { .. }))
            .count()
            != 2
    {
        return None;
    }

    let new_dispatch = match_dispatch(cfg, &new_expression.node_id)?;
    let set_dispatch = match_dispatch(cfg, &set_expression.node_id)?;
    if !has_point(
        cfg,
        &new_dispatch,
        &new_expression.node_id,
        CfgPointKind::Condition,
    ) || !has_point(
        cfg,
        &set_dispatch,
        &set_expression.node_id,
        CfgPointKind::Condition,
    ) {
        return None;
    }
    let (new_ok_edge, new_ok_block_id) = edge_for_arm(cfg, &new_dispatch, new_ok_arm)?;
    let (new_error_edge, new_error_block_id) = edge_for_arm(cfg, &new_dispatch, new_error_arm)?;
    let (set_ok_edge, set_ok_block_id) = edge_for_arm(cfg, &set_dispatch, set_ok_arm)?;
    let (set_error_edge, set_error_block_id) = edge_for_arm(cfg, &set_dispatch, set_error_arm)?;
    if new_ok_block_id != &set_dispatch
        || !has_drop_point(
            cfg,
            set_ok_block_id,
            set_ok_old_root_drop,
            initial_root_local,
        )
        || !has_drop_point(cfg, set_ok_block_id, root_get_drop, updated_root_local)
        || !has_drop_point(cfg, set_ok_block_id, pair_drop, pair_local)
        || !has_drop_point(
            cfg,
            set_error_block_id,
            set_error_input_drop,
            initial_root_local,
        )
        || [
            &get_expression.node_id,
            &pair_binding_statement.node_id,
            &set_ok_old_root_drop.node_id,
            &root_get_drop.node_id,
            &print_expression.node_id,
            &pair_drop.node_id,
        ]
        .into_iter()
        .any(|node| point_location(cfg, node) != Some(set_ok_block_id))
        || point_location(cfg, &set_error_input_drop.node_id) != Some(set_error_block_id)
        || !has_unique_point(cfg, &new_expression.node_id)
        || !has_unique_point(cfg, &set_expression.node_id)
        || !has_unique_point(cfg, &print_expression.node_id)
        || size_operation.is_some_and(|operation| {
            !has_point(
                cfg,
                set_ok_block_id,
                &operation.node_id,
                CfgPointKind::Expression,
            ) || point_location(cfg, &operation.node_id) != Some(set_ok_block_id)
        })
        || size_binding.is_some_and(|statement| {
            point_location(cfg, &statement.node_id) != Some(set_ok_block_id)
        })
        || !path_reaches_return(cfg, new_error_block_id, &root_expression.node_id)
        || !path_reaches_return(cfg, set_error_block_id, &root_expression.node_id)
        || !path_reaches_return(cfg, set_ok_block_id, &root_expression.node_id)
    {
        return None;
    }

    let get_rank = point_index(cfg, set_ok_block_id, &get_expression.node_id)?;
    let pair_binding_rank = point_index(cfg, set_ok_block_id, &pair_binding_statement.node_id)?;
    let old_root_drop_rank = point_index(cfg, set_ok_block_id, &set_ok_old_root_drop.node_id)?;
    let root_drop_rank = point_index(cfg, set_ok_block_id, &root_get_drop.node_id)?;
    let print_rank = point_index(cfg, set_ok_block_id, &print_expression.node_id)?;
    let pair_drop_rank = point_index(cfg, set_ok_block_id, &pair_drop.node_id)?;
    let ordered = match (&size_operation, &size_binding) {
        (None, None) => {
            get_rank < pair_binding_rank
                && pair_binding_rank < old_root_drop_rank
                && old_root_drop_rank < root_drop_rank
                && root_drop_rank < print_rank
                && print_rank < pair_drop_rank
        }
        (Some(size_operation), Some(size_binding)) => {
            let size_rank = point_index(cfg, set_ok_block_id, &size_operation.node_id)?;
            let size_binding_rank = point_index(cfg, set_ok_block_id, &size_binding.node_id)?;
            get_rank < pair_binding_rank
                && pair_binding_rank < print_rank
                && print_rank < size_rank
                && size_rank < size_binding_rank
                && size_binding_rank < pair_drop_rank
                && pair_drop_rank < old_root_drop_rank
                && old_root_drop_rank < root_drop_rank
        }
        _ => return None,
    };
    if !ordered {
        return None;
    }

    let result_receipt = OrdinaryMapOwnedAnyV2ResultReceipt {
        profile_version: OrdinaryMapOwnedAnyV2ProfileVersion::V2,
        operation: get_expression.node_id.clone(),
        kind: ActionKind::GetAny,
        source_local: updated_root_local.clone(),
        source: crate::core::ResourceId(NodeId(format!(
            "ordinary-map-owned-any-v2:{}",
            updated_root_local.0 .0
        ))),
        source_node: get_receiver.value.node_id.clone(),
        source_type: set_root_type.clone(),
        key: Some(get_key.value.node_id.clone()),
        key_type: Some(string_type.clone()),
        key_local: None,
        result: get_expression.node_id.clone(),
        result_type: get_expression.ty.clone(),
        binding: pair_local.clone(),
        binding_point: pair_binding_statement.node_id.clone(),
        payload_println: Some(OrdinaryMapOwnedAnyV2PrintlnObservation {
            projection: print_argument.value.node_id.clone(),
            call: print_expression.node_id.clone(),
            checker_payload_type: any_type.clone(),
            result_type: unit_type.clone(),
        }),
        settlement: OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop,
        settlement_point: Some(pair_drop.node_id.clone()),
        computed_string_key_owner_chain: None,
    };

    Some(Receipt {
        profile_version: Version::V1,
        shape,
        owner: owner.clone(),
        source_body_commitment: format!("{:#?}", body.root),
        new_operation: new_expression.node_id.clone(),
        new_result_type: new_expression.ty.clone(),
        new_match: root_expression.node_id.clone(),
        new_dispatch: new_dispatch.clone(),
        new_ok_arm: new_ok_arm.node_id.clone(),
        new_ok_edge: (*new_ok_edge).clone(),
        new_ok_target: (*new_ok_block_id).clone(),
        new_ok_pattern: new_ok_pattern.node_id.clone(),
        initial_root_local: initial_root_local.clone(),
        initial_root_type: initial_root_type.clone(),
        new_error_arm: new_error_arm.node_id.clone(),
        new_error_edge: (*new_error_edge).clone(),
        new_error_target: (*new_error_block_id).clone(),
        new_error_pattern: new_error_pattern.node_id.clone(),
        new_error_local: new_error_local.clone(),
        new_error_return: new_error_return.node_id.clone(),
        set_operation: set_expression.node_id.clone(),
        set_result_type: set_expression.ty.clone(),
        set_key_value,
        set_payload_type,
        set_payload,
        set_match: set_match_expression.node_id.clone(),
        set_dispatch: set_dispatch.clone(),
        set_ok_arm: set_ok_arm.node_id.clone(),
        set_ok_edge: (*set_ok_edge).clone(),
        set_ok_target: (*set_ok_block_id).clone(),
        set_ok_pattern: set_ok_pattern.node_id.clone(),
        updated_root_local: updated_root_local.clone(),
        set_error_arm: set_error_arm.node_id.clone(),
        set_error_edge: (*set_error_edge).clone(),
        set_error_target: (*set_error_block_id).clone(),
        set_error_pattern: set_error_pattern.node_id.clone(),
        set_error_local: set_error_local.clone(),
        set_error_return: set_error_return.node_id.clone(),
        set_ok_input_drop: set_ok_old_root_drop.node_id.clone(),
        set_error_input_drop: set_error_input_drop.node_id.clone(),
        get_operation: get_expression.node_id.clone(),
        get_receiver: get_receiver.value.node_id.clone(),
        get_key: get_key.value.node_id.clone(),
        get_key_value,
        pair_binding: pair_binding_statement.node_id.clone(),
        pair_local: pair_local.clone(),
        pair_type: get_expression.ty.clone(),
        result_receipt,
        root_get_drop: root_get_drop.node_id.clone(),
        payload_projection: print_argument.value.node_id.clone(),
        print_operation: print_expression.node_id.clone(),
        pair_drop: pair_drop.node_id.clone(),
        size_operation: size_operation.map(|operation| operation.node_id.clone()),
        size_receiver,
        size_binding: size_binding.map(|statement| statement.node_id.clone()),
        size_local: size_local.cloned(),
        success_return: success_result.node_id.clone(),
        operations: expected_operations,
        cfg: ManagedAnyCfgReceipt::from_cfg(cfg, body),
    })
}

pub(super) fn validate(
    receipt: &Receipt,
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
    operations: &BTreeSet<Attempt>,
) -> Result<(), String> {
    if collect(cfg, body, signature, types, operations).as_ref() == Some(receipt) {
        Ok(())
    } else {
        Err("ordinary Map nested Result-match direct-print L1 receipt disagrees with Checker source, exact branch cleanup, operation attempts, or CFG edges".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIRECT_PRINT_SOURCE: &str = include_str!(
        "../../../../tests/fixtures/mir_ordinary_map_any_owner_v2_direct_print_hit_i32.mimi"
    );
    const DIRECT_PRINT_HIT_STRING_SOURCE: &str = include_str!(
        "../../../../tests/fixtures/mir_ordinary_map_any_owner_v2_direct_print_hit_string.mimi"
    );
    const DIRECT_PRINT_MISS_I32_SOURCE: &str = include_str!(
        "../../../../tests/fixtures/mir_ordinary_map_any_owner_v2_direct_print_miss_i32.mimi"
    );
    const DIRECT_PRINT_SAME_ROOT_MAP_SIZE_SOURCE: &str = include_str!(
        "../../../../tests/fixtures/mir_ordinary_map_any_owner_v2_direct_print_same_root_map_size_i32.mimi"
    );

    fn checked(source: &str) -> Result<crate::core::CheckedProgram, Vec<Diagnostic>> {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file)
    }

    fn receipt(program: &crate::core::CheckedProgram) -> Option<&Receipt> {
        program
            .resource_analysis(&NodeId("function:main".into()))?
            .ordinary_map_owned_any_v2_direct_print_result_receipt
            .as_ref()
    }

    fn assert_receipt_withheld(source: &str, label: &str) {
        match checked(source) {
            Ok(program) => assert!(
                receipt(&program).is_none(),
                "{label} must withhold the L1 receipt"
            ),
            Err(diagnostics) => assert!(
                !diagnostics.is_empty(),
                "{label} may fail earlier only with a real Checker diagnostic"
            ),
        }
    }

    #[test]
    fn nested_result_direct_print_receipts_replay_checker_cfg_for_all_four_shapes() {
        for (source, expected_shape, expected_payload, expected_key, expected_operations) in [
            (
                DIRECT_PRINT_SOURCE,
                Shape::HitI32,
                SetPayload::I32(42),
                "answer",
                3,
            ),
            (
                DIRECT_PRINT_HIT_STRING_SOURCE,
                Shape::HitString,
                SetPayload::String("borrowed String λ".to_owned()),
                "answer",
                3,
            ),
            (
                DIRECT_PRINT_MISS_I32_SOURCE,
                Shape::MissI32,
                SetPayload::I32(42),
                "missing",
                3,
            ),
            (
                DIRECT_PRINT_SAME_ROOT_MAP_SIZE_SOURCE,
                Shape::SameRootMapSizeI32,
                SetPayload::I32(42),
                "answer",
                4,
            ),
        ] {
            let program = checked(source).expect("direct-print source checks");
            let owner = NodeId("function:main".into());
            let callable = program.callable(&owner).expect("main callable");
            let receipt = receipt(&program).expect("exact nested Result direct-print receipt");
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
            .expect("receipt replays source, attempts, and CFG");
            assert_eq!(receipt.profile_version, Version::V1);
            assert_eq!(receipt.shape, expected_shape);
            assert_eq!(receipt.set_key_value, "answer");
            assert_eq!(receipt.set_payload, expected_payload);
            let expected_payload_type = match &expected_payload {
                SetPayload::I32(_) => crate::core::ir::primitive_type_id(PrimitiveType::I32),
                SetPayload::String(_) => crate::core::ir::primitive_type_id(PrimitiveType::String),
            };
            assert_eq!(receipt.set_payload_type, expected_payload_type);
            assert_eq!(receipt.get_key_value, expected_key);
            assert_eq!(receipt.owner, owner);
            assert_eq!(receipt.operations.len(), expected_operations);
            assert_eq!(
                receipt.shape == Shape::SameRootMapSizeI32,
                receipt.size_operation.is_some()
                    && receipt.size_receiver.is_some()
                    && receipt.size_binding.is_some()
                    && receipt.size_local.is_some()
            );
            assert!(!receipt.cfg.reachable_blocks.is_empty());
            assert_eq!(receipt.result_receipt.operation, receipt.get_operation);
            assert_eq!(receipt.result_receipt.binding, receipt.pair_local);
            assert_eq!(
                receipt.result_receipt.source_local,
                receipt.updated_root_local
            );
            assert_eq!(receipt.result_receipt.source_node, receipt.get_receiver);
            assert_eq!(receipt.result_receipt.kind, ActionKind::GetAny);
            assert_eq!(
                receipt.result_receipt.settlement_point.as_ref(),
                Some(&receipt.pair_drop)
            );
            assert_eq!(
                receipt
                    .result_receipt
                    .payload_println
                    .as_ref()
                    .map(|observation| (&observation.projection, &observation.call)),
                Some((&receipt.payload_projection, &receipt.print_operation))
            );
            for (edge_id, from, to, arm) in [
                (
                    &receipt.new_ok_edge,
                    &receipt.new_dispatch,
                    &receipt.new_ok_target,
                    &receipt.new_ok_arm,
                ),
                (
                    &receipt.new_error_edge,
                    &receipt.new_dispatch,
                    &receipt.new_error_target,
                    &receipt.new_error_arm,
                ),
                (
                    &receipt.set_ok_edge,
                    &receipt.set_dispatch,
                    &receipt.set_ok_target,
                    &receipt.set_ok_arm,
                ),
                (
                    &receipt.set_error_edge,
                    &receipt.set_dispatch,
                    &receipt.set_error_target,
                    &receipt.set_error_arm,
                ),
            ] {
                let edge = receipt
                    .cfg
                    .reachable_edges
                    .get(edge_id)
                    .expect("exact source match edge is committed");
                assert_eq!(&edge.from, from);
                assert_eq!(&edge.to, to);
                assert_eq!(edge.kind, EdgeKind::MatchArm);
                assert_eq!(&edge.source.node, arm);
            }
        }
    }

    #[test]
    fn nested_result_direct_print_rejects_missing_wrong_or_extra_owner_operations() {
        let mutations = [
            (
                "Set Err input root Drop is missing",
                DIRECT_PRINT_SOURCE.replace(
                    "Err(status) => {\n            drop(empty)\n            status",
                    "Err(status) => {\n            status",
                ),
            ),
            (
                "successful updated-root Drop is missing",
                DIRECT_PRINT_SOURCE.replace("        drop(root)\n        println", "        println"),
            ),
            (
                "pair Drop is missing",
                DIRECT_PRINT_SOURCE.replace("        drop(pair)\n        0", "        0"),
            ),
            (
                "println observes the found flag instead of tuple payload",
                DIRECT_PRINT_SOURCE.replace("println(pair.1)", "println(pair.0)"),
            ),
            (
                "additional output call is present",
                DIRECT_PRINT_SOURCE.replace("        println(pair.1)", "        println(pair.1)\n        println(7)"),
            ),
            (
                "Result Ok arm carries a guard",
                DIRECT_PRINT_SOURCE.replace("Ok(root) =>", "Ok(root) if true =>"),
            ),
            (
                "a second Result-matched Set adds another arm-local Map root",
                DIRECT_PRINT_SOURCE
                    .replace(
                        "        match map_set(empty, \"answer\", 42) {\n",
                        "        match map_set(empty, \"extra\", 11) {\n            Ok(extra) => {\n            drop(extra)\n            match map_set(empty, \"answer\", 42) {\n",
                    )
                    .replace(
                        "        }\n        },\n        Err(status) => {",
                        "        }\n            },\n            Err(extra_status) => {\n            drop(empty)\n            extra_status\n            },\n        }\n        },\n        Err(status) => {",
                    ),
            ),
            (
                "an extra Map-root Drop is inserted in the successful arm",
                DIRECT_PRINT_SOURCE.replace(
                    "        drop(empty)\n        drop(root)",
                    "        drop(empty)\n        drop(empty)\n        drop(root)",
                ),
            ),
            (
                "the New match carries an extra Result payload arm",
                DIRECT_PRINT_SOURCE.replace(
                    "        Err(status) => {\n        status\n        },\n    }\n}",
                    "        Err(status) => {\n        status\n        },\n        Ok(extra) => {\n        drop(extra)\n        0\n        },\n    }\n}",
                ),
            ),
            (
                "New Err returns a value unrelated to its bound status",
                DIRECT_PRINT_SOURCE.replace(
                    "        Err(status) => {\n        status\n        },\n    }\n}",
                    "        Err(status) => {\n        0\n        },\n    }\n}",
                ),
            ),
            (
                "Set Err returns a value unrelated to its bound status",
                DIRECT_PRINT_SOURCE.replace(
                    "            drop(empty)\n            status\n            },",
                    "            drop(empty)\n            0\n            },",
                ),
            ),
        ];
        for (label, source) in mutations {
            assert_ne!(source, DIRECT_PRINT_SOURCE, "{label} mutation applies");
            assert_receipt_withheld(&source, label);
        }
    }

    #[test]
    fn nested_result_direct_print_withholds_receipts_for_each_shape_mutation() {
        let mutations = [
            (
                "HitI32 stores a different integer payload",
                DIRECT_PRINT_SOURCE.replace(
                    "map_set(empty, \"answer\", 42)",
                    "map_set(empty, \"answer\", 43)",
                ),
                DIRECT_PRINT_SOURCE,
            ),
            (
                "HitString payload changes outside the exact direct-print contract",
                DIRECT_PRINT_HIT_STRING_SOURCE.replace("borrowed String λ", "different String"),
                DIRECT_PRINT_HIT_STRING_SOURCE,
            ),
            (
                "MissI32 get key changes to an unsupported miss key",
                DIRECT_PRINT_MISS_I32_SOURCE
                    .replace("map_get(root, \"missing\")", "map_get(root, \"absent\")"),
                DIRECT_PRINT_MISS_I32_SOURCE,
            ),
            (
                "same-root size observes a different root",
                DIRECT_PRINT_SAME_ROOT_MAP_SIZE_SOURCE.replace("map_size(root)", "map_size(empty)"),
                DIRECT_PRINT_SAME_ROOT_MAP_SIZE_SOURCE,
            ),
            (
                "same-root size is returned through a different value",
                DIRECT_PRINT_SAME_ROOT_MAP_SIZE_SOURCE.replace("        size\n", "        0\n"),
                DIRECT_PRINT_SAME_ROOT_MAP_SIZE_SOURCE,
            ),
            (
                "same-root shape adds a second map_size operation",
                DIRECT_PRINT_SAME_ROOT_MAP_SIZE_SOURCE.replace(
                    "        let size = map_size(root)",
                    "        let size = map_size(root)\n        let extra_size = map_size(root)",
                ),
                DIRECT_PRINT_SAME_ROOT_MAP_SIZE_SOURCE,
            ),
        ];
        for (label, mutated_source, original_source) in mutations {
            assert_ne!(mutated_source, original_source, "{label} mutation applies");
            assert_receipt_withheld(&mutated_source, label);
        }
    }
}
