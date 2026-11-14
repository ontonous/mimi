//! Exact Checker source/CFG receipt for the original fallible Map lifecycle
//! whose reads use the legacy tuple-valued `map_get -> (bool, Any)` API.
//!
//! This source receipt is intentionally independent from the V2
//! Result-valued `map_get_any` lineage. It covers one fixed two-root source
//! shape only and is not consumed by production route selection in this L1
//! slice.

use super::*;
use crate::core::cfg::{EdgeKind, Terminator};
use crate::core::ir::{CheckedConversionKind, PrimitiveType, ResolvedProjection};
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2Action, OrdinaryMapOwnedAnyV2Attempt,
    OrdinaryMapOwnedAnyV2AttemptKind as V2Attempt,
    OrdinaryMapOwnedGetAnyTupleResultV1Action as Action,
    OrdinaryMapOwnedGetAnyTupleResultV1MapActionKind as MapAction,
    OrdinaryMapOwnedGetAnyTupleResultV1OwnerChain as OwnerChain,
    OrdinaryMapOwnedGetAnyTupleResultV1ProfileVersion as Version,
    OrdinaryMapOwnedGetAnyTupleResultV1Receipt as Receipt,
    OrdinaryMapOwnedGetAnyTupleResultV1ResultBranch as ResultBranch,
    OrdinaryMapOwnedGetI32StringV1Action, OrdinaryMapOwnedGetI32StringV1Attempt,
    OrdinaryMapOwnedGetI32StringV1AttemptKind as V1Attempt,
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
        && call.permission.is_none()
        && call.session.is_empty())
    .then_some(call)
}

fn direct_load(expression: &ResolvedExpr, local: &ResolvedLocalId, ty: &ResolvedTypeId) -> bool {
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

fn block_expr(expression: &ResolvedExpr) -> Option<&ResolvedBlock> {
    match &expression.kind {
        ResolvedExprKind::Block(block) => Some(block),
        _ => None,
    }
}

fn match_parts(expression: &ResolvedExpr) -> Option<(&ResolvedExpr, &[MatchArm])> {
    let ResolvedExprKind::Match { scrutinee, arms } = &expression.kind else {
        return None;
    };
    Some((scrutinee, arms))
}

fn arm_result(arm: &MatchArm) -> Option<&ResolvedExpr> {
    block_expr(&arm.body)?.result.as_deref()
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

fn is_record(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(types.get(ty), Some(ResolvedType::Nominal {
        item, arguments, is_linear: false,
    }) if item.as_str() == "builtin:type:Record" && arguments.is_empty())
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

fn result_payload<'a>(
    arm: &'a MatchArm,
    variant_name: &str,
    result_type: &ResolvedTypeId,
    payload_type: &ResolvedTypeId,
    body: &ResolvedBody,
) -> Option<&'a ResolvedLocalId> {
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
        .then_some(local)
}

fn drop_local(statement: &ResolvedStmt, local: &ResolvedLocalId) -> bool {
    matches!(&statement.kind, ResolvedStmtKind::Drop(places)
        if places.len() == 1
            && places[0].base == *local
            && places[0].projections.is_empty())
}

fn statement_at(block: &ResolvedBlock, index: usize) -> Option<&ResolvedStmt> {
    block.statements.get(index)
}

fn binding_at<'a>(
    block: &'a ResolvedBlock,
    index: usize,
    body: &'a ResolvedBody,
) -> Option<(&'a ResolvedLocalId, &'a ResolvedExpr)> {
    immutable_binding(statement_at(block, index)?, body)
}

fn one_drop_at(block: &ResolvedBlock, index: usize, local: &ResolvedLocalId) -> Option<NodeId> {
    let statement = statement_at(block, index)?;
    drop_local(statement, local).then(|| statement.node_id.clone())
}

fn arm_block(arm: &MatchArm) -> Option<&ResolvedBlock> {
    block_expr(&arm.body)
}

fn is_literal_string(expression: &ResolvedExpr, value: &str) -> bool {
    matches!(&expression.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(actual))
        if actual == value)
}

fn is_literal_i32(expression: &ResolvedExpr, value: i64, i32_type: &ResolvedTypeId) -> bool {
    expression.ty == *i32_type
        && matches!(&expression.kind, ResolvedExprKind::Literal(ResolvedLiteral::Int(actual))
            if *actual == value)
}

fn is_i32_zero_literal_coerced_to_any(
    expression: &ResolvedExpr,
    i32_type: &ResolvedTypeId,
    any_type: &ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> bool {
    let ResolvedExprKind::Cast { value, conversion } = &expression.kind else {
        return false;
    };
    expression.ty == *any_type
        && matches!(types.get(any_type), Some(ResolvedType::DynamicAny { .. }))
        && conversion.kind == CheckedConversionKind::DynamicAnyPack
        && conversion.from == *i32_type
        && conversion.to == *any_type
        && is_literal_i32(value, 0, i32_type)
}

fn call_statement<'a>(
    statement: &'a ResolvedStmt,
    name: &str,
    arity: usize,
) -> Option<(&'a ResolvedExpr, &'a ResolvedCall)> {
    let ResolvedStmtKind::Expr(expression) = &statement.kind else {
        return None;
    };
    Some((expression, builtin_call(expression, name, arity)?))
}

fn callable_call_for_local<'a>(
    expression: &'a ResolvedExpr,
    callee: &NodeId,
    argument_local: &ResolvedLocalId,
    argument_type: &ResolvedTypeId,
) -> Option<&'a ResolvedCall> {
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return None;
    };
    let [argument] = call.arguments.as_slice() else {
        return None;
    };
    (call.callee == ResolvedCallee::Function(callee.clone())
        && call.result == expression.ty
        && call.type_arguments.as_slice() == [argument_type.clone()]
        && direct_load(&argument.value, argument_local, argument_type)
        && argument.conversion.kind == CheckedConversionKind::Identity
        && argument.conversion.from == *argument_type
        && argument.conversion.to == *argument_type
        && call.permission.is_none()
        && call.session.is_empty())
    .then_some(call)
}

#[derive(Debug)]
struct MapCall<'a> {
    expression: &'a ResolvedExpr,
    call: &'a ResolvedCall,
    binding: Option<ResolvedLocalId>,
}

fn is_map_builtin(name: &str) -> bool {
    matches!(
        name,
        "map_new"
            | "map_set"
            | "map_get"
            | "map_get_any"
            | "map_get_i32"
            | "map_get_string"
            | "map_remove"
            | "map_size"
            | "has_key"
            | "is_empty"
            | "keys"
            | "values"
            | "to_list"
            | "map_from_list"
    )
}

fn collect_expr_calls<'a>(expression: &'a ResolvedExpr, calls: &mut Vec<MapCall<'a>>) {
    match &expression.kind {
        ResolvedExprKind::FString(parts) => {
            for part in parts {
                if let ResolvedFStringPart::Interpolation(value) = part {
                    collect_expr_calls(value, calls);
                }
            }
        }
        ResolvedExprKind::Project { value, projection } => {
            collect_expr_calls(value, calls);
            if let ResolvedValueProjection::Index(index) = projection {
                collect_expr_calls(index, calls);
            }
        }
        ResolvedExprKind::Binary { left, right, .. }
        | ResolvedExprKind::Range {
            start: left,
            end: right,
        } => {
            collect_expr_calls(left, calls);
            collect_expr_calls(right, calls);
        }
        ResolvedExprKind::Unary { operand, .. }
        | ResolvedExprKind::OptionalChain {
            receiver: operand, ..
        }
        | ResolvedExprKind::TypeOf(operand)
        | ResolvedExprKind::Old(operand)
        | ResolvedExprKind::Try { value: operand, .. }
        | ResolvedExprKind::Cast { value: operand, .. }
        | ResolvedExprKind::Spawn(operand)
        | ResolvedExprKind::Await(operand) => collect_expr_calls(operand, calls),
        ResolvedExprKind::Call(call) => {
            if matches!(&call.callee, ResolvedCallee::Builtin(name) if is_map_builtin(name.as_str()))
            {
                calls.push(MapCall {
                    expression,
                    call,
                    binding: None,
                });
            }
            for argument in &call.arguments {
                collect_expr_calls(&argument.value, calls);
            }
        }
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => {
            for value in values {
                collect_expr_calls(value, calls);
            }
        }
        ResolvedExprKind::Map(entries) => {
            for (key, value) in entries {
                collect_expr_calls(key, calls);
                collect_expr_calls(value, calls);
            }
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            collect_expr_calls(value, calls);
            collect_expr_calls(iterable, calls);
            if let Some(guard) = guard {
                collect_expr_calls(guard, calls);
            }
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            for field in fields {
                collect_expr_calls(&field.value, calls);
            }
            if let Some(rest) = rest {
                collect_expr_calls(rest, calls);
            }
        }
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => collect_block_calls(block, calls),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            collect_expr_calls(condition, calls);
            collect_block_calls(then_block, calls);
            collect_block_calls(else_block, calls);
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            collect_expr_calls(scrutinee, calls);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_expr_calls(guard, calls);
                }
                collect_expr_calls(&arm.body, calls);
            }
        }
        ResolvedExprKind::Slice { target, start, end } => {
            collect_expr_calls(target, calls);
            if let Some(start) = start {
                collect_expr_calls(start, calls);
            }
            if let Some(end) = end {
                collect_expr_calls(end, calls);
            }
        }
        ResolvedExprKind::Load(_)
        | ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::Lambda(_)
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => {}
    }
}

fn collect_block_calls<'a>(block: &'a ResolvedBlock, calls: &mut Vec<MapCall<'a>>) {
    for statement in &block.statements {
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer,
            } => {
                if let Some(expression) = initializer {
                    if let ResolvedExprKind::Call(call) = &expression.kind {
                        if matches!(&call.callee, ResolvedCallee::Builtin(name) if is_map_builtin(name.as_str()))
                        {
                            let binding = match &pattern.kind {
                                ResolvedPatternKind::Binding { local, .. } => Some(local.clone()),
                                _ => None,
                            };
                            calls.push(MapCall {
                                expression,
                                call,
                                binding,
                            });
                            // Record the enclosing call with its result
                            // binding above, then census any nested calls in
                            // its arguments without duplicating this one.
                            for argument in &call.arguments {
                                collect_expr_calls(&argument.value, calls);
                            }
                        } else {
                            collect_expr_calls(expression, calls);
                        }
                    } else {
                        collect_expr_calls(expression, calls);
                    }
                }
            }
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => collect_expr_calls(value, calls),
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    collect_expr_calls(value, calls);
                }
            }
            ResolvedStmtKind::While { condition, body } => {
                collect_expr_calls(condition, calls);
                collect_block_calls(body, calls);
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            }
            | ResolvedStmtKind::For {
                iterable: initializer,
                body,
                ..
            } => {
                collect_expr_calls(initializer, calls);
                collect_block_calls(body, calls);
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                collect_expr_calls(initializer, calls);
                collect_block_calls(then_block, calls);
                if let Some(block) = else_block {
                    collect_block_calls(block, calls);
                }
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                collect_block_calls(body, calls)
            }
            ResolvedStmtKind::Pinned { value, body, .. } => {
                collect_expr_calls(value, calls);
                collect_block_calls(body, calls);
            }
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_)
            | ResolvedStmtKind::Math(_) => {}
        }
    }
    if let Some(result) = &block.result {
        collect_expr_calls(result, calls);
    }
}

fn cfg_has_match(cfg: &CallableCfg, scrutinee: &NodeId, arms: &[MatchArm]) -> bool {
    let mut found = cfg.blocks.iter().filter(|(_, block)| {
        matches!(&block.terminator, Terminator::Match { scrutinee: actual, .. } if actual == scrutinee)
    });
    let Some((dispatch, block)) = found.next() else {
        return false;
    };
    if found.next().is_some() {
        return false;
    }
    let Terminator::Match { arms: edges, .. } = &block.terminator else {
        return false;
    };
    edges.len() == arms.len()
        && arms.iter().all(|arm| {
            let mut matching = edges.iter().filter_map(|edge_id| {
                let edge = cfg.edges.get(edge_id)?;
                (edge.from == *dispatch
                    && edge.kind == EdgeKind::MatchArm
                    && edge.source.node == arm.node_id)
                    .then_some(edge)
            });
            matching.next().is_some() && matching.next().is_none()
        })
}

fn cfg_has_if(
    cfg: &CallableCfg,
    expression: &ResolvedExpr,
    condition: &ResolvedExpr,
    then_block: &ResolvedBlock,
    else_block: &ResolvedBlock,
) -> bool {
    let mut found = cfg.blocks.iter().filter(|(_, block)| {
        matches!(&block.terminator, Terminator::Branch { condition: actual, .. }
            if actual == &condition.node_id)
    });
    let Some((dispatch, block)) = found.next() else {
        return false;
    };
    if found.next().is_some() {
        return false;
    }
    let Terminator::Branch {
        then_edge,
        else_edge,
        ..
    } = &block.terminator
    else {
        return false;
    };
    let Some(then_edge) = cfg.edges.get(then_edge) else {
        return false;
    };
    let Some(else_edge) = cfg.edges.get(else_edge) else {
        return false;
    };
    then_edge.from == *dispatch
        && then_edge.kind == EdgeKind::Then
        && then_edge.source.node == expression.node_id
        && cfg
            .blocks
            .get(&then_edge.to)
            .is_some_and(|block| block.source.node == then_block.node_id)
        && else_edge.from == *dispatch
        && else_edge.kind == EdgeKind::Else
        && else_edge.source.node == expression.node_id
        && cfg
            .blocks
            .get(&else_edge.to)
            .is_some_and(|block| block.source.node == else_block.node_id)
}

fn cfg_point_count(cfg: &CallableCfg, node: &NodeId) -> usize {
    cfg.reachable
        .iter()
        .filter_map(|id| cfg.blocks.get(id))
        .flat_map(|block| &block.points)
        .filter(|point| point.source.node == *node)
        .count()
}

fn cfg_has_point(cfg: &CallableCfg, node: &NodeId) -> bool {
    cfg_point_count(cfg, node) == 1
}

struct ResultDispatch<'a> {
    expression: &'a ResolvedExpr,
    scrutinee: &'a ResolvedExpr,
    arms: &'a [MatchArm],
    ok: &'a MatchArm,
    err: &'a MatchArm,
}

fn result_dispatch(expression: &ResolvedExpr) -> Option<ResultDispatch<'_>> {
    let (scrutinee, arms) = match_parts(expression)?;
    if arms.len() != 2 || arms.iter().any(|arm| arm.guard.is_some()) {
        return None;
    }
    let ok = variant_arm(arms, "Ok")?;
    let err = variant_arm(arms, "Err")?;
    Some(ResultDispatch {
        expression,
        scrutinee,
        arms,
        ok,
        err,
    })
}

fn identity_argument(argument: &crate::core::ir::ResolvedArgument) -> bool {
    argument.conversion.kind == CheckedConversionKind::Identity
        && argument.conversion.from == argument.value.ty
        && argument.conversion.to == argument.value.ty
}

fn direct_result_load(
    expression: &ResolvedExpr,
    local: &ResolvedLocalId,
    ty: &ResolvedTypeId,
) -> bool {
    direct_load(expression, local, ty)
}

fn make_result_branch(
    operation_kind: MapAction,
    dispatch: &ResultDispatch<'_>,
    result_type: &ResolvedTypeId,
    ok_local: &ResolvedLocalId,
    err_local: &ResolvedLocalId,
    source_root: Option<&ResolvedLocalId>,
    ok_root_drop: Option<NodeId>,
    err_root_drop: Option<NodeId>,
    ok_result: &ResolvedExpr,
    err_result: &ResolvedExpr,
) -> ResultBranch {
    ResultBranch {
        operation_kind,
        operation: dispatch.scrutinee.node_id.clone(),
        result_type: result_type.clone(),
        match_node: dispatch.expression.node_id.clone(),
        scrutinee: dispatch.scrutinee.node_id.clone(),
        ok_arm: dispatch.ok.node_id.clone(),
        err_arm: dispatch.err.node_id.clone(),
        ok_local: ok_local.clone(),
        err_local: err_local.clone(),
        source_root: source_root.cloned(),
        // The built-in contract has no persistent root on New::Err and no
        // published updated root on Set::Err. Set Err retains and therefore
        // must explicitly consume its source root.
        err_creates_root: false,
        err_publishes_updated_root: false,
        err_consumes_source_root: source_root.is_some(),
        ok_root_drop,
        err_root_drop,
        ok_result: ok_result.node_id.clone(),
        err_result: err_result.node_id.clone(),
    }
}

fn expected_owned_get_attempts(
    calls: &[MapCall<'_>],
    pair_locals: &[ResolvedLocalId; 2],
) -> Option<BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>> {
    let expected_names = [
        "map_new", "map_set", "map_get", "map_new", "map_set", "map_get",
    ];
    if calls.len() != expected_names.len()
        || calls.iter().zip(expected_names).any(|(call, expected)| {
            !matches!(&call.call.callee, ResolvedCallee::Builtin(name) if name.as_str() == expected)
        })
    {
        return None;
    }
    let mut attempts = BTreeSet::new();
    let mut get_index = 0usize;
    for call in calls {
        let ResolvedCallee::Builtin(name) = &call.call.callee else {
            return None;
        };
        let (kind, receiver_local, result_local, arity) = match name.as_str() {
            "map_new" if call.call.arguments.is_empty() && call.binding.is_none() => {
                (V1Attempt::New, None, None, 0)
            }
            "map_set" if call.call.arguments.len() == 3 && call.binding.is_none() => (
                V1Attempt::Set,
                direct_receiver(&call.call.arguments[0].value),
                None,
                3,
            ),
            "map_get" if call.call.arguments.len() == 2 => {
                let local = call.binding.clone()?;
                if pair_locals.get(get_index) != Some(&local) {
                    return None;
                }
                get_index += 1;
                (
                    V1Attempt::UntypedGet,
                    direct_receiver(&call.call.arguments[0].value),
                    Some(local),
                    2,
                )
            }
            _ => return None,
        };
        if call.call.arguments.len() != arity
            || (kind != V1Attempt::New && receiver_local.is_none())
        {
            return None;
        }
        attempts.insert(OrdinaryMapOwnedGetI32StringV1Attempt {
            kind,
            point: call.expression.node_id.clone(),
            arguments: call
                .call
                .arguments
                .iter()
                .map(|argument| argument.value.node_id.clone())
                .collect(),
            receiver_local,
            result_local,
        });
    }
    (attempts.len() == 6 && get_index == 2).then_some(attempts)
}

fn expected_any_v2_attempts(
    owned_attempts: &BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
) -> BTreeSet<OrdinaryMapOwnedAnyV2Attempt> {
    owned_attempts
        .iter()
        .map(|attempt| OrdinaryMapOwnedAnyV2Attempt {
            kind: match attempt.kind {
                V1Attempt::New => V2Attempt::New,
                V1Attempt::Set => V2Attempt::Set,
                V1Attempt::UntypedGet => V2Attempt::GetAny,
                _ => unreachable!("tuple-result receipt has only New/Set/Get attempts"),
            },
            point: attempt.point.clone(),
            arguments: attempt.arguments.clone(),
            receiver_local: attempt.receiver_local.clone(),
            result_local: attempt.result_local.clone(),
        })
        .collect()
}

fn direct_receiver(expression: &ResolvedExpr) -> Option<ResolvedLocalId> {
    match &expression.kind {
        ResolvedExprKind::Load(place) if place.projections.is_empty() => Some(place.base.clone()),
        _ => None,
    }
}

fn drop_action(
    kind: MapAction,
    statement: &ResolvedStmt,
    operation: &ResolvedExpr,
    local: &ResolvedLocalId,
    value_type: &ResolvedTypeId,
) -> Action {
    Action {
        kind,
        point: statement.node_id.clone(),
        operation: Some(operation.node_id.clone()),
        local: Some(local.clone()),
        related_local: None,
        value: None,
        value_type: Some(value_type.clone()),
    }
}

fn operation_action(
    kind: MapAction,
    operation: &ResolvedExpr,
    dispatch: &ResultDispatch<'_>,
    ok_local: &ResolvedLocalId,
    err_local: &ResolvedLocalId,
) -> Action {
    Action {
        kind,
        point: operation.node_id.clone(),
        operation: Some(operation.node_id.clone()),
        local: Some(ok_local.clone()),
        related_local: Some(err_local.clone()),
        value: Some(dispatch.expression.node_id.clone()),
        value_type: Some(operation.ty.clone()),
    }
}

fn make_owner_chain(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    block: &ResolvedBlock,
    source_root_local: &ResolvedLocalId,
    root_local: &ResolvedLocalId,
    expected_key: &str,
    carry_callee: &NodeId,
    i32_type: &ResolvedTypeId,
    bool_type: &ResolvedTypeId,
    string_type: &ResolvedTypeId,
) -> Option<(OwnerChain, Vec<Action>, NodeId)> {
    if block.statements.len() != 12 {
        return None;
    }
    let (pair_local, get_expression) = binding_at(block, 0, body)?;
    let get_call = builtin_call(get_expression, "map_get", 2)?;
    if !get_call.type_arguments.is_empty()
        || get_call.permission.is_some()
        || !get_call.session.is_empty()
    {
        return None;
    }
    let [receiver, key] = get_call.arguments.as_slice() else {
        return None;
    };
    let root_type = body.locals.get(root_local)?.ty.clone();
    if !direct_load(&receiver.value, root_local, &root_type)
        || !identity_argument(receiver)
        || !is_literal_string(&key.value, expected_key)
        || key.value.ty != *string_type
        || !identity_argument(key)
    {
        return None;
    }
    let pair_type = get_expression.ty.clone();
    let ResolvedType::Tuple(elements) = types.get(&pair_type)? else {
        return None;
    };
    let [found_type, payload_type] = elements.as_slice() else {
        return None;
    };
    if found_type != bool_type
        || !matches!(
            types.get(payload_type),
            Some(ResolvedType::DynamicAny { .. })
        )
        || body.locals.get(pair_local)?.ty != pair_type
    {
        return None;
    }
    let source_root_drop = statement_at(block, 1)?;
    if !drop_local(source_root_drop, source_root_local) {
        return None;
    }
    let root_drop_statement = statement_at(block, 2)?;
    if !drop_local(root_drop_statement, root_local) {
        return None;
    }
    let (found_local, found_expression) = binding_at(block, 3, body)?;
    if !tuple_projection(found_expression, pair_local, 0, bool_type) {
        return None;
    }
    let (payload_local, payload_expression) = binding_at(block, 4, body)?;
    if !tuple_projection(payload_expression, pair_local, 1, payload_type) {
        return None;
    }
    let (carried_local, carry_expression) = binding_at(block, 5, body)?;
    let Some(carry_call) =
        callable_call_for_local(carry_expression, carry_callee, payload_local, payload_type)
    else {
        return None;
    };
    if carry_expression.ty != *payload_type
        || body.locals.get(carried_local)?.ty != *payload_type
        || body.resolved_calls.get(&carry_expression.node_id)
            != Some(&ResolvedCallee::Function(carry_callee.clone()))
    {
        return None;
    }

    let (escape_local, escape_expression) = binding_at(block, 6, body)?;
    let observation = if expected_key == "k\0🧪" {
        if escape_expression.ty != *bool_type {
            return None;
        }
        let ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } = &escape_expression.kind
        else {
            return None;
        };
        if !direct_load(condition, found_local, bool_type)
            || then_block.statements.len() != 0
            || else_block.statements.len() != 0
            || !matches!(
                else_block.result.as_deref().map(|e| &e.kind),
                Some(ResolvedExprKind::Literal(ResolvedLiteral::Bool(false)))
            )
        {
            return None;
        }
        let equal = then_block.result.as_deref()?;
        let ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::Equal,
            left,
            right,
        } = &equal.kind
        else {
            return None;
        };
        if !direct_load(left, carried_local, payload_type)
            || !is_literal_string(right, "owned\0🧪值")
            || right.ty != *string_type
        {
            return None;
        }
        if !cfg_has_if(cfg, escape_expression, condition, then_block, else_block) {
            return None;
        }
        equal.node_id.clone()
    } else {
        if escape_expression.ty != *payload_type {
            return None;
        }
        let ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } = &escape_expression.kind
        else {
            return None;
        };
        let condition_is_found = direct_load(condition, found_local, bool_type);
        let then_is_carried =
            direct_load(then_block.result.as_deref()?, carried_local, payload_type);
        let else_is_zero = is_i32_zero_literal_coerced_to_any(
            else_block.result.as_deref()?,
            i32_type,
            payload_type,
            types,
        );
        let cfg_matches_if = cfg_has_if(cfg, escape_expression, condition, then_block, else_block);
        if !condition_is_found
            || then_block.statements.len() != 0
            || else_block.statements.len() != 0
            || !then_is_carried
            || !else_is_zero
            || !cfg_matches_if
        {
            return None;
        }
        escape_expression.node_id.clone()
    };
    let first_print = call_statement(statement_at(block, 7)?, "println", 1)?;
    let second_print = call_statement(statement_at(block, 8)?, "println", 1)?;
    let first_print_arg = first_print.1.arguments.first()?;
    let second_print_arg = second_print.1.arguments.first()?;
    if !direct_load(&first_print_arg.value, found_local, bool_type)
        || !identity_argument(first_print_arg)
        || !direct_load(&second_print_arg.value, escape_local, &escape_expression.ty)
        || !identity_argument(second_print_arg)
    {
        return None;
    }

    let pair_drop = one_drop_at(block, 9, pair_local)?;
    let payload_drop = one_drop_at(block, 10, payload_local)?;
    let carried_drop = one_drop_at(block, 11, carried_local)?;
    let updated_root_drop = root_drop_statement;
    if !drop_local(updated_root_drop, root_local) {
        return None;
    }
    // The map_get receiver is the Set-produced root, while the source root is
    // separately recorded on the Set Result branch.
    let any_type = payload_type.clone();
    let actions = vec![
        Action {
            kind: MapAction::GetTuple,
            point: get_expression.node_id.clone(),
            operation: Some(get_expression.node_id.clone()),
            local: Some(root_local.clone()),
            related_local: Some(pair_local.clone()),
            value: Some(key.value.node_id.clone()),
            value_type: Some(key.value.ty.clone()),
        },
        drop_action(
            MapAction::RootDrop,
            updated_root_drop,
            get_expression,
            root_local,
            &root_type,
        ),
        drop_action(
            MapAction::PairDrop,
            statement_at(block, 9)?,
            get_expression,
            pair_local,
            &pair_type,
        ),
        drop_action(
            MapAction::PayloadDrop,
            statement_at(block, 10)?,
            get_expression,
            payload_local,
            &any_type,
        ),
        drop_action(
            MapAction::CarriedDrop,
            statement_at(block, 11)?,
            get_expression,
            carried_local,
            &any_type,
        ),
    ];
    if [get_expression, first_print.0, second_print.0]
        .iter()
        .any(|expression| !cfg_has_point(cfg, &expression.node_id))
        || [
            source_root_drop.node_id.clone(),
            pair_drop.clone(),
            payload_drop.clone(),
            carried_drop.clone(),
        ]
        .iter()
        .any(|point| !cfg_has_point(cfg, point))
    {
        return None;
    }
    Some((
        OwnerChain {
            get_operation: get_expression.node_id.clone(),
            root_local: root_local.clone(),
            key_node: key.value.node_id.clone(),
            key_value: expected_key.to_string(),
            pair_local: pair_local.clone(),
            pair_type,
            found_projection: found_expression.node_id.clone(),
            found_local: found_local.clone(),
            payload_projection: payload_expression.node_id.clone(),
            payload_local: payload_local.clone(),
            payload_type: any_type,
            carry_operation: carry_expression.node_id.clone(),
            carry_callee: match &carry_call.callee {
                ResolvedCallee::Function(callee) => callee.clone(),
                _ => return None,
            },
            carried_local: carried_local.clone(),
            observation_points: vec![
                observation.clone(),
                first_print.0.node_id.clone(),
                second_print.0.node_id.clone(),
            ],
            root_drop: updated_root_drop.node_id.clone(),
            pair_drop,
            payload_drop,
            carried_drop,
        },
        actions,
        second_print.0.node_id.clone(),
    ))
}

fn parse_new_dispatch<'a>(
    expression: &'a ResolvedExpr,
    types: &ResolvedTypeTable,
    body: &'a ResolvedBody,
    i32_type: &ResolvedTypeId,
) -> Option<(
    ResultDispatch<'a>,
    ResolvedLocalId,
    ResolvedLocalId,
    &'a ResolvedExpr,
    &'a ResolvedExpr,
)> {
    let dispatch = result_dispatch(expression)?;
    let call = builtin_call(dispatch.scrutinee, "map_new", 0)?;
    if !call.type_arguments.is_empty() || call.permission.is_some() || !call.session.is_empty() {
        return None;
    }
    let (ok_type, err_type) = result_parts(types, &dispatch.scrutinee.ty)?;
    if !is_record(types, ok_type) || err_type != i32_type {
        return None;
    }
    let ok_local =
        result_payload(dispatch.ok, "Ok", &dispatch.scrutinee.ty, ok_type, body)?.clone();
    let err_local =
        result_payload(dispatch.err, "Err", &dispatch.scrutinee.ty, err_type, body)?.clone();
    let ok_result = arm_result(dispatch.ok)?;
    if !arm_block(dispatch.err)?.statements.is_empty() {
        return None;
    }
    let err_result = arm_result(dispatch.err)?;
    if !direct_result_load(err_result, &err_local, i32_type) {
        return None;
    }
    Some((dispatch, ok_local, err_local, ok_result, err_result))
}

fn parse_set_dispatch<'a>(
    expression: &'a ResolvedExpr,
    source_root: &ResolvedLocalId,
    key_value: &str,
    value_shape: Option<(&ResolvedTypeId, i64)>,
    types: &ResolvedTypeTable,
    body: &'a ResolvedBody,
    i32_type: &ResolvedTypeId,
    string_type: &ResolvedTypeId,
) -> Option<(
    ResultDispatch<'a>,
    ResolvedLocalId,
    ResolvedLocalId,
    &'a ResolvedBlock,
    &'a ResolvedExpr,
    &'a ResolvedExpr,
)> {
    let dispatch = result_dispatch(expression)?;
    let call = builtin_call(dispatch.scrutinee, "map_set", 3)?;
    if !call.type_arguments.is_empty() || call.permission.is_some() || !call.session.is_empty() {
        return None;
    }
    let [root, key, value] = call.arguments.as_slice() else {
        return None;
    };
    let root_type = body.locals.get(source_root)?.ty.clone();
    if !direct_load(&root.value, source_root, &root_type)
        || !identity_argument(root)
        || !is_literal_string(&key.value, key_value)
        || key.value.ty != *string_type
        || !identity_argument(key)
    {
        return None;
    }
    match value_shape {
        Some((expected_type, expected)) => {
            if value.value.ty != *expected_type
                || !is_literal_i32(&value.value, expected, expected_type)
                || !identity_argument(value)
            {
                return None;
            }
        }
        None => {
            if value.value.ty != *string_type
                || !is_literal_string(&value.value, "owned\0🧪值")
                || !identity_argument(value)
            {
                return None;
            }
        }
    }
    let (ok_type, err_type) = result_parts(types, &dispatch.scrutinee.ty)?;
    if !is_record(types, ok_type) || err_type != i32_type {
        return None;
    }
    let ok_local =
        result_payload(dispatch.ok, "Ok", &dispatch.scrutinee.ty, ok_type, body)?.clone();
    let err_local =
        result_payload(dispatch.err, "Err", &dispatch.scrutinee.ty, err_type, body)?.clone();
    let ok_block = arm_block(dispatch.ok)?;
    let err_block = arm_block(dispatch.err)?;
    if err_block.statements.len() != 1 || !drop_local(&err_block.statements[0], source_root) {
        return None;
    }
    let err_result = err_block.result.as_deref()?;
    if !direct_result_load(err_result, &err_local, i32_type) {
        return None;
    }
    let ok_result = ok_block.result.as_deref()?;
    Some((
        dispatch, ok_local, err_local, ok_block, ok_result, err_result,
    ))
}

pub(super) fn collect(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
    owned_get_attempts: &BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
    owned_get_actions: &[OrdinaryMapOwnedGetI32StringV1Action],
    any_v2_attempts: &BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    any_v2_actions: &[OrdinaryMapOwnedAnyV2Action],
) -> Option<Receipt> {
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    if body.owner != NodeId("function:main".into())
        || signature.owner != body.owner
        || !signature.parameters.is_empty()
        || !signature.generic_parameters.is_empty()
        || signature.result != i32_type
        || body.root.ty != i32_type
        || !body.parameters.is_empty()
        || !body.captures.is_empty()
        || !body.place_inputs.is_empty()
        || !body.default_values.is_empty()
        || !body.root.statements.is_empty()
        || cfg.owner != body.owner
        || cfg.reachable.len() != cfg.blocks.len()
        || cfg
            .edges
            .values()
            .any(|edge| !cfg.reachable.contains(&edge.from) || !cfg.reachable.contains(&edge.to))
    {
        return None;
    }
    let outer_expression = body.root.result.as_deref()?;
    let (outer_new, empty_local, outer_status, first_set_expression, outer_err_result) =
        parse_new_dispatch(outer_expression, types, body, &i32_type)?;
    let (
        first_set,
        first_root_local,
        first_set_status,
        first_ok_block,
        second_new_expression,
        first_set_err_result,
    ) = parse_set_dispatch(
        first_set_expression,
        &empty_local,
        "k\0🧪",
        None,
        types,
        body,
        &i32_type,
        &string_type,
    )?;
    let first_set_source_drop = statement_at(first_ok_block, 1)?;
    let first_set_new_root_drop = statement_at(first_ok_block, 2)?;
    if !drop_local(first_set_source_drop, &empty_local)
        || !drop_local(first_set_new_root_drop, &first_root_local)
    {
        return None;
    }
    let (first_chain, mut map_actions, _first_print) = make_owner_chain(
        cfg,
        body,
        types,
        first_ok_block,
        &empty_local,
        &first_root_local,
        "k\0🧪",
        &NodeId("function:carry".into()),
        &i32_type,
        &bool_type,
        &string_type,
    )?;
    let (
        second_new,
        number_empty_local,
        second_new_status,
        second_set_expression,
        second_new_err_result,
    ) = parse_new_dispatch(second_new_expression, types, body, &i32_type)?;
    let (
        second_set,
        number_root_local,
        second_set_status,
        second_ok_block,
        zero_result,
        second_set_err_result,
    ) = parse_set_dispatch(
        second_set_expression,
        &number_empty_local,
        "zero",
        Some((&i32_type, 0)),
        types,
        body,
        &i32_type,
        &string_type,
    )?;
    if second_ok_block.statements.len() != 12 || !is_literal_i32(zero_result, 0, &i32_type) {
        return None;
    }
    let second_set_source_drop = statement_at(second_ok_block, 1)?;
    let second_set_new_root_drop = statement_at(second_ok_block, 2)?;
    if !drop_local(second_set_source_drop, &number_empty_local)
        || !drop_local(second_set_new_root_drop, &number_root_local)
    {
        return None;
    }
    let (second_chain, second_chain_actions, _second_print) = make_owner_chain(
        cfg,
        body,
        types,
        second_ok_block,
        &number_empty_local,
        &number_root_local,
        "zero",
        &NodeId("function:carry".into()),
        &i32_type,
        &bool_type,
        &string_type,
    )?;
    map_actions.extend(second_chain_actions);
    let set_error_drop_nodes = [
        one_drop_at(arm_block(first_set.err)?, 0, &empty_local)?,
        one_drop_at(arm_block(second_set.err)?, 0, &number_empty_local)?,
    ];

    let outer_new_branch = make_result_branch(
        MapAction::NewResult,
        &outer_new,
        &outer_new.scrutinee.ty,
        &empty_local,
        &outer_status,
        None,
        None,
        None,
        first_set_expression,
        outer_err_result,
    );
    let second_new_branch = make_result_branch(
        MapAction::NewResult,
        &second_new,
        &second_new.scrutinee.ty,
        &number_empty_local,
        &second_new_status,
        None,
        None,
        None,
        second_set_expression,
        second_new_err_result,
    );
    let first_set_branch = make_result_branch(
        MapAction::SetResult,
        &first_set,
        &first_set.scrutinee.ty,
        &first_root_local,
        &first_set_status,
        Some(&empty_local),
        Some(first_set_source_drop.node_id.clone()),
        Some(set_error_drop_nodes[0].clone()),
        second_new_expression,
        first_set_err_result,
    );
    let second_set_branch = make_result_branch(
        MapAction::SetResult,
        &second_set,
        &second_set.scrutinee.ty,
        &number_root_local,
        &second_set_status,
        Some(&number_empty_local),
        Some(second_set_source_drop.node_id.clone()),
        Some(set_error_drop_nodes[1].clone()),
        zero_result,
        second_set_err_result,
    );

    let branches = vec![
        outer_new_branch,
        first_set_branch,
        second_new_branch,
        second_set_branch,
    ];
    let dispatches = vec![&outer_new, &first_set, &second_new, &second_set];
    for dispatch in &dispatches {
        if !cfg_has_match(cfg, &dispatch.scrutinee.node_id, dispatch.arms)
            || !cfg_has_point(cfg, &dispatch.scrutinee.node_id)
            || !cfg_has_point(cfg, &dispatch.expression.node_id)
        {
            return None;
        }
    }
    let get_pairs = [
        first_chain.pair_local.clone(),
        second_chain.pair_local.clone(),
    ];
    let mut map_calls = Vec::new();
    collect_block_calls(&body.root, &mut map_calls);
    let expected_v1_attempts = expected_owned_get_attempts(&map_calls, &get_pairs)?;
    let expected_v2_attempts = expected_any_v2_attempts(&expected_v1_attempts);
    if owned_get_attempts != &expected_v1_attempts || any_v2_attempts != &expected_v2_attempts {
        return None;
    }
    for call in &map_calls {
        if !cfg_has_point(cfg, &call.expression.node_id) {
            return None;
        }
    }
    let branch_actions = branches
        .iter()
        .zip([&outer_new, &first_set, &second_new, &second_set])
        .map(|(branch, dispatch)| {
            operation_action(
                branch.operation_kind,
                dispatch.scrutinee,
                dispatch,
                &branch.ok_local,
                &branch.err_local,
            )
        })
        .collect::<Vec<_>>();
    map_actions.extend(branch_actions);
    for (dispatch, source_root, ok_drop, err_arm, branch_index) in [
        (
            &first_set,
            &empty_local,
            first_set_source_drop,
            first_set.err,
            1usize,
        ),
        (
            &second_set,
            &number_empty_local,
            second_set_source_drop,
            second_set.err,
            3usize,
        ),
    ] {
        let err_drop = arm_block(err_arm)?.statements.first()?;
        let branch = branches.get(branch_index)?;
        if branch.operation_kind != MapAction::SetResult
            || branch.source_root.as_ref() != Some(source_root)
            || branch.ok_root_drop.as_ref() != Some(&ok_drop.node_id)
            || branch.err_root_drop.as_ref() != Some(&err_drop.node_id)
            || !branch.err_consumes_source_root
            || !drop_local(ok_drop, source_root)
            || !drop_local(err_drop, source_root)
        {
            return None;
        }
        let root_type = &body.locals.get(source_root)?.ty;
        map_actions.push(drop_action(
            MapAction::RootDrop,
            ok_drop,
            dispatch.scrutinee,
            source_root,
            root_type,
        ));
        map_actions.push(drop_action(
            MapAction::RootDrop,
            err_drop,
            dispatch.scrutinee,
            source_root,
            root_type,
        ));
    }
    if branches[0].err_creates_root
        || branches[0].err_publishes_updated_root
        || branches[0].err_consumes_source_root
        || branches[2].err_creates_root
        || branches[2].err_publishes_updated_root
        || branches[2].err_consumes_source_root
    {
        return None;
    }
    map_actions.sort_by_key(|action| action.point.clone());
    if map_actions
        .iter()
        .any(|action| !cfg_has_point(cfg, &action.point))
    {
        return None;
    }
    if map_actions.len() != 18 {
        return None;
    }

    Some(Receipt {
        profile_version: Version::V1,
        owner: body.owner.clone(),
        source_body_commitment: format!("{:#?}", body.root),
        result_branches: branches,
        owner_chains: vec![first_chain, second_chain],
        map_actions,
        owned_get_attempts: expected_v1_attempts,
        owned_get_actions: owned_get_actions.to_vec(),
        any_v2_attempts: expected_v2_attempts,
        any_v2_actions: any_v2_actions.to_vec(),
        cfg: ManagedAnyCfgReceipt::from_cfg(cfg, body),
    })
}

pub(super) fn validate(
    receipt: &Receipt,
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
    owned_get_attempts: &BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
    owned_get_actions: &[OrdinaryMapOwnedGetI32StringV1Action],
    any_v2_attempts: &BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    any_v2_actions: &[OrdinaryMapOwnedAnyV2Action],
) -> Result<(), String> {
    if collect(
        cfg,
        body,
        signature,
        types,
        owned_get_attempts,
        owned_get_actions,
        any_v2_attempts,
        any_v2_actions,
    )
    .as_ref()
        == Some(receipt)
    {
        Ok(())
    } else {
        Err("ordinary Map owned-get legacy tuple Result receipt disagrees with exact Checker source/CFG/operation attempts".into())
    }
}
