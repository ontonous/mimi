//! Typed production-island eligibility predicates.
//!
//! These predicates are shared by route selection and legacy deletion gates.
//! They intentionally inspect only checker-owned facts; a backend must not
//! rediscover a migrated shape from the retained surface AST.

use crate::ast::Type;
use crate::core::ir::{
    ResolvedBinaryOp, ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedLiteral,
    ResolvedPatternKind, ResolvedProjection, ResolvedStmtKind, ResolvedType,
    ResolvedValueProjection,
};
use crate::core::{
    CheckedProgram, NodeId, ResolvedBody, ResolvedLocalId, ResolvedPattern, ResolvedStmt,
    TransitionId,
};

/// Whether the checked program is one of the deliberately narrow recoverable
/// Flow profiles. M3 is the single-state retry profile; F2 is the cross-state
/// `Result<state, (source, error)>` match profile. The predicate is only
/// admission; the canonical MIR graph and all consumer gates still prove the
/// concrete Result/source-transfer contract. The failure arm may either bind
/// the complete tuple and use a checked read projection, or destructure it
/// into direct source/error bindings; both forms retain one nested Move
/// receipt for the complete failure payload.
pub fn is_flow_failure_retry_candidate(program: &CheckedProgram) -> bool {
    if program.has_imports() || program.flows().len() != 1 || !program.actors().is_empty() {
        return false;
    }
    let implemented = program
        .transitions()
        .values()
        .filter(|transition| {
            !transition.is_fallback
                && transition.fails.is_some()
                && program.resolved_body(&transition.node_id).is_some()
        })
        .collect::<Vec<_>>();
    let [transition] = implemented.as_slice() else {
        return false;
    };
    let Some(flow) = program.flows().get(&transition.id.flow) else {
        return false;
    };
    let Some(source_state) = flow.states.get(&transition.id.source.name) else {
        return false;
    };
    let Some(target) = transition.targets.first() else {
        return false;
    };
    let Some(target_state) = flow.states.get(&target.name) else {
        return false;
    };
    let Some(signature) = program.resolved_signature(&transition.node_id) else {
        return false;
    };
    let result_is_result = matches!(
        program.resolved_types().get(&signature.result),
        Some(ResolvedType::Result { .. })
    );
    let local_retry = transition.silent_transition
        && transition.targets.len() == 1
        && transition.targets[0] == transition.id.source
        && !transition.is_ffi_pinned
        && flow
            .states
            .keys()
            .filter(|name| name.as_str() != "Fault")
            .count()
            == 1
        && flow.persistent_fields.is_empty()
        && source_state.payload.len() == 1
        && target_state.payload.len() == 1
        && is_supported_local_retry_state_type(&source_state.payload[0].1)
        && is_supported_local_retry_state_type(&target_state.payload[0].1)
        && transition.params.len() == 1
        && is_concrete_i32_type(&transition.params[0].1)
        && result_is_result
        && program
            .resolved_body(&transition.node_id)
            .is_some_and(|body| block_contains_try(&body.root));

    local_retry
        || is_exact_cross_state_result_match(program, transition, flow)
        || is_exact_multifield_cross_state_record_receipt(program, transition, flow)
}

/// The bounded F1 record-residual shape: a failing cross-state transition
/// moves one owned String field from a two-field source state into a one-field
/// target state, while the other source field is released by the canonical
/// `MoveProjectDrop` receipt on the failure path.  The MIR lowerer and all
/// consumers already support this receipt; admission was intentionally kept
/// behind the single-field retry/F2 profiles until this composition boundary
/// had a fixed real-world fixture.
fn is_exact_multifield_cross_state_record_receipt(
    program: &CheckedProgram,
    transition: &crate::core::resolved::ResolvedTransition,
    flow: &crate::core::resolved::ResolvedFlow,
) -> bool {
    if transition.silent_transition
        || transition.targets.len() != 1
        || transition.targets[0] == transition.id.source
        || transition.params.len() != 1
        || transition
            .fails
            .as_ref()
            .is_none_or(|ty| !is_concrete_string_type(ty))
        || transition.is_fallback
        || transition.is_ffi_pinned
        || flow.persistent_fields.len() != 0
        || flow
            .states
            .keys()
            .filter(|name| name.as_str() != "Fault")
            .count()
            != 2
        || program
            .transitions()
            .values()
            .filter(|item| !item.is_fallback && program.resolved_body(&item.node_id).is_some())
            .count()
            != 1
        || !is_concrete_i32_type(&transition.params[0].1)
    {
        return false;
    }
    let Some(source_state) = flow.states.get(&transition.id.source.name) else {
        return false;
    };
    let Some(target_id) = transition.targets.first() else {
        return false;
    };
    let Some(target_state) = flow.states.get(&target_id.name) else {
        return false;
    };
    let [(target_field, target_ty)] = target_state.payload.as_slice() else {
        return false;
    };
    if source_state.payload.len() != 2
        || !is_concrete_string_type(target_ty)
        || target_field.is_empty()
        || !source_state
            .payload
            .iter()
            .all(|(_, ty)| is_concrete_string_type(ty))
        || source_state
            .payload
            .iter()
            .filter(|(name, ty)| name == target_field && is_concrete_string_type(ty))
            .count()
            != 1
    {
        return false;
    }
    let Some(signature) = program.resolved_signature(&transition.node_id) else {
        return false;
    };
    if !matches!(
        program.resolved_types().get(&signature.result),
        Some(ResolvedType::Result { .. })
    ) {
        return false;
    }
    program
        .resolved_body(&transition.node_id)
        .is_some_and(|body| block_contains_try(&body.root))
}

/// F2 is intentionally a source-shaped matcher, not a broad "Flow with a
/// match" admission. It opens exactly one synchronous cross-state failing
/// transition, with one Copy `i32` field in each state, and a caller that
/// matches `Ok(Target { field })` versus `Err(_)`. All payload and ownership
/// facts still come from the canonical MIR construction and validators.
fn is_exact_cross_state_result_match(
    program: &CheckedProgram,
    transition: &crate::core::resolved::ResolvedTransition,
    flow: &crate::core::resolved::ResolvedFlow,
) -> bool {
    if transition.silent_transition
        || transition.targets.len() != 1
        || transition.targets[0] == transition.id.source
        || transition.params.len() != 0
        || transition.fails.is_none()
        || transition.is_fallback
        || transition.is_ffi_pinned
        || flow.persistent_fields.len() != 0
        || flow
            .states
            .keys()
            .filter(|name| name.as_str() != "Fault")
            .count()
            != 2
        || program
            .transitions()
            .values()
            .filter(|item| !item.is_fallback && program.resolved_body(&item.node_id).is_some())
            .count()
            != 1
    {
        return false;
    }
    let Some(source_state) = flow.states.get(&transition.id.source.name) else {
        return false;
    };
    let Some(target_id) = transition.targets.first() else {
        return false;
    };
    let Some(target_state) = flow.states.get(&target_id.name) else {
        return false;
    };
    let [(_, source_field_ty)] = source_state.payload.as_slice() else {
        return false;
    };
    let [(_, target_field_ty)] = target_state.payload.as_slice() else {
        return false;
    };
    if !is_concrete_i32_type(source_field_ty) || !is_concrete_i32_type(target_field_ty) {
        return false;
    }
    let Some(signature) = program.resolved_signature(&transition.node_id) else {
        return false;
    };
    if !matches!(
        program.resolved_types().get(&signature.result),
        Some(ResolvedType::Result { .. })
    ) {
        return false;
    }
    let Some(transition_body) = program.resolved_body(&transition.node_id) else {
        return false;
    };
    let Some(source_local) = transition_body.parameters.first() else {
        return false;
    };
    let target_nominal = format!("state:{}::{}", flow.id.0, target_id.name);
    let success_transition =
        exact_cross_state_transition_body(&transition_body.root, source_local, &target_nominal);
    let failure_transition = exact_cross_state_failure_transition_body(
        &transition_body.root,
        source_local,
        &target_nominal,
    );
    program
        .resolved_body(&NodeId("function:main".into()))
        .is_some_and(|body| {
            let success_main =
                exact_cross_state_match_main(program, body, transition, flow, target_id);
            let failure_main =
                exact_cross_state_failure_match_main(program, body, transition, flow, target_id);
            (success_transition && success_main) || (failure_transition && failure_main)
        })
}

fn exact_cross_state_transition_body(
    block: &crate::core::ir::ResolvedBlock,
    source_local: &ResolvedLocalId,
    target_nominal: &str,
) -> bool {
    match block.statements.as_slice() {
        [crate::core::ir::ResolvedStmt {
            kind: ResolvedStmtKind::Return {
                value: Some(value), ..
            },
            ..
        }] => {
            let ResolvedExprKind::Record {
                nominal,
                fields,
                rest: None,
            } = &value.kind
            else {
                return false;
            };
            let [field] = fields.as_slice() else {
                return false;
            };
            if nominal.as_str() != target_nominal || field.field.0.is_empty() {
                return false;
            }
            let ResolvedExprKind::Binary {
                op: ResolvedBinaryOp::Add,
                left,
                right,
            } = &field.value.kind
            else {
                return false;
            };
            matches!(
                (&left.kind, &right.kind),
                (
                    ResolvedExprKind::Load(crate::core::ir::ResolvedPlace {
                        base,
                        projections,
                    }),
                    ResolvedExprKind::Literal(ResolvedLiteral::Int(1)),
                ) if base == source_local
                    && projections.len() == 1
                    && matches!(
                        projections[0],
                        ResolvedProjection::Field { .. }
                    )
            )
        }
        [crate::core::ir::ResolvedStmt {
            kind:
                ResolvedStmtKind::Scope {
                    kind: crate::core::ir::ResolvedScopeKind::Lexical,
                    body,
                },
            ..
        }] => exact_cross_state_transition_body(body, source_local, target_nominal),
        _ => false,
    }
}

fn exact_cross_state_failure_transition_body(
    block: &crate::core::ir::ResolvedBlock,
    source_local: &ResolvedLocalId,
    target_nominal: &str,
) -> bool {
    let [checked, next, return_statement] = match block.statements.as_slice() {
        [checked, next, return_statement] => [checked, next, return_statement],
        [crate::core::ir::ResolvedStmt {
            kind:
                ResolvedStmtKind::Scope {
                    kind: crate::core::ir::ResolvedScopeKind::Lexical,
                    body,
                },
            ..
        }] => return exact_cross_state_failure_transition_body(body, source_local, target_nominal),
        _ => return false,
    };
    let (checked_pattern, checked_initializer, next_pattern, next_initializer, value) =
        match (&checked.kind, &next.kind, &return_statement.kind) {
            (
                ResolvedStmtKind::Bind {
                    pattern: checked_pattern,
                    initializer: Some(checked_initializer),
                },
                ResolvedStmtKind::Bind {
                    pattern: next_pattern,
                    initializer: Some(next_initializer),
                },
                ResolvedStmtKind::Return {
                    value: Some(value), ..
                },
            ) => (
                checked_pattern,
                checked_initializer,
                next_pattern,
                next_initializer,
                value,
            ),
            _ => return false,
        };
    let (checked_local, next_local) = match (&checked_pattern.kind, &next_pattern.kind) {
        (
            ResolvedPatternKind::Binding {
                local: checked_local,
                by_reference: None,
            },
            ResolvedPatternKind::Binding {
                local: next_local,
                by_reference: None,
            },
        ) => (checked_local, next_local),
        _ => return false,
    };
    let ResolvedExprKind::If {
        condition,
        then_block,
        else_block,
    } = &checked_initializer.kind
    else {
        return false;
    };
    let ResolvedExprKind::Binary {
        op: ResolvedBinaryOp::Equal,
        left,
        right,
    } = &condition.kind
    else {
        return false;
    };
    let source_field = match &left.kind {
        ResolvedExprKind::Load(crate::core::ir::ResolvedPlace { base, projections })
            if base == source_local
                && projections.len() == 1
                && matches!(projections[0], ResolvedProjection::Field { .. }) =>
        {
            true
        }
        _ => false,
    };
    if !source_field
        || !matches!(
            right.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(1))
        )
    {
        return false;
    }
    let Some(error_call) = single_block_call(then_block) else {
        return false;
    };
    if !matches!(&error_call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "Err")
        || error_call.arguments.len() != 1
    {
        return false;
    }
    if !matches!(
        error_call.arguments[0].value.kind,
        ResolvedExprKind::Literal(ResolvedLiteral::String(_))
    ) {
        return false;
    }
    let Some(ok_call) = single_block_call(else_block) else {
        return false;
    };
    if !matches!(&ok_call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "Ok")
        || ok_call.arguments.len() != 1
    {
        return false;
    }
    let ResolvedExprKind::Binary {
        op: ResolvedBinaryOp::Add,
        left: ok_left,
        right: ok_right,
    } = &ok_call.arguments[0].value.kind
    else {
        return false;
    };
    if !matches!(&ok_left.kind, ResolvedExprKind::Load(crate::core::ir::ResolvedPlace { base, projections }) if base == source_local && projections.len() == 1 && matches!(projections[0], ResolvedProjection::Field { .. }))
        || !matches!(
            ok_right.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(1))
        )
    {
        return false;
    }
    let ResolvedExprKind::Try {
        value: try_value, ..
    } = &next_initializer.kind
    else {
        return false;
    };
    if !matches!(&try_value.kind, ResolvedExprKind::Load(crate::core::ir::ResolvedPlace { base, projections }) if base == checked_local && projections.is_empty())
    {
        return false;
    }
    let ResolvedExprKind::Record {
        nominal,
        fields,
        rest: None,
    } = &value.kind
    else {
        return false;
    };
    let [field] = fields.as_slice() else {
        return false;
    };
    nominal.as_str() == target_nominal
        && !field.field.0.is_empty()
        && matches!(&field.value.kind, ResolvedExprKind::Load(crate::core::ir::ResolvedPlace { base, projections }) if base == next_local && projections.is_empty())
}

fn single_block_call(
    block: &crate::core::ir::ResolvedBlock,
) -> Option<&crate::core::ir::ResolvedCall> {
    match (block.statements.as_slice(), block.result.as_deref()) {
        (
            [crate::core::ir::ResolvedStmt {
                kind:
                    ResolvedStmtKind::Expr(ResolvedExpr {
                        kind: ResolvedExprKind::Call(call),
                        ..
                    }),
                ..
            }],
            None,
        )
        | (
            [],
            Some(ResolvedExpr {
                kind: ResolvedExprKind::Call(call),
                ..
            }),
        ) => Some(call),
        _ => None,
    }
}

fn exact_cross_state_failure_match_main(
    _program: &CheckedProgram,
    body: &ResolvedBody,
    transition: &crate::core::resolved::ResolvedTransition,
    flow: &crate::core::resolved::ResolvedFlow,
    target: &crate::core::StateId,
) -> bool {
    let statements = executable_statements(&body.root);
    let (first, match_expression) = match statements.as_slice() {
        [first, second]
            if body.root.result.as_deref().is_some_and(|result| {
                matches!(
                    &result.kind,
                    ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
                )
            }) =>
        {
            let ResolvedStmtKind::Expr(expression) = &second.kind else {
                return false;
            };
            (first, expression)
        }
        [first] => {
            let Some(expression) = body.root.result.as_deref() else {
                return false;
            };
            (first, expression)
        }
        _ => return false,
    };
    let ResolvedExprKind::Match { scrutinee, arms } = &match_expression.kind else {
        return false;
    };
    let (source_local, transition_owner) = match &first.kind {
        ResolvedStmtKind::Bind {
            pattern:
                crate::core::ir::ResolvedPattern {
                    kind:
                        ResolvedPatternKind::Binding {
                            local,
                            by_reference: None,
                        },
                    ..
                },
            initializer:
                Some(ResolvedExpr {
                    kind: ResolvedExprKind::Call(call),
                    ..
                }),
        } => match &call.callee {
            ResolvedCallee::Transition(owner) => (local, owner),
            _ => return false,
        },
        _ => return false,
    };
    if transition_owner != &transition.id {
        return false;
    }
    let ResolvedExprKind::Load(crate::core::ir::ResolvedPlace { base, projections }) =
        &scrutinee.kind
    else {
        return false;
    };
    if base != source_local || !projections.is_empty() || arms.len() != 2 {
        return false;
    }
    let target_nominal = format!("state:{}::{}", flow.id.0, target.name);
    let mut saw_ok = false;
    let mut saw_err = false;
    for arm in arms {
        match &arm.pattern.kind {
            ResolvedPatternKind::Constructor { variant, fields }
                if variant.0 == "builtin:variant:Result::Ok" =>
            {
                let [(_, nested)] = fields.as_slice() else {
                    return false;
                };
                let ResolvedPatternKind::Constructor {
                    variant: nested_variant,
                    fields: nested_fields,
                } = &nested.kind
                else {
                    return false;
                };
                let [(nested_field, nested_binding)] = nested_fields.as_slice() else {
                    return false;
                };
                let ResolvedPatternKind::Binding {
                    local,
                    by_reference: None,
                } = &nested_binding.kind
                else {
                    return false;
                };
                if nested_variant.0 != target_nominal
                    || nested_field.0.is_empty()
                    || (!exact_println_block(&arm.body, Some(local))
                        && !exact_println_then_value_block(&arm.body, local, 99))
                {
                    return false;
                }
                saw_ok = true;
            }
            ResolvedPatternKind::Constructor { variant, fields }
                if variant.0 == "builtin:variant:Result::Err" =>
            {
                let [(field, payload)] = fields.as_slice() else {
                    return false;
                };
                if field.0.is_empty()
                    || (!matches!(&payload.kind, ResolvedPatternKind::Binding { .. })
                        && !matches!(&payload.kind, ResolvedPatternKind::Tuple(_)))
                    || (!exact_failure_match_arm(&arm.body, payload)
                        && !exact_destructured_failure_match_arm(&arm.body, payload))
                {
                    return false;
                }
                saw_err = true;
            }
            _ => return false,
        }
    }
    saw_ok && saw_err
}

fn exact_failure_match_arm(expression: &ResolvedExpr, payload: &ResolvedPattern) -> bool {
    let ResolvedPatternKind::Binding {
        local,
        by_reference: None,
    } = &payload.kind
    else {
        return false;
    };
    exact_failure_match_arm_for_local(expression, &local)
}

fn exact_failure_match_arm_for_local(
    expression: &ResolvedExpr,
    error_local: &ResolvedLocalId,
) -> bool {
    let ResolvedExprKind::Block(block) = &expression.kind else {
        return false;
    };
    let [print_statement, drop_statement] = block.statements.as_slice() else {
        return false;
    };
    let ResolvedStmtKind::Expr(ResolvedExpr {
        kind: ResolvedExprKind::Call(print_call),
        ..
    }) = &print_statement.kind
    else {
        return false;
    };
    if !matches!(&print_call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "println")
        || print_call.type_arguments.len() != 0
        || print_call.session.len() != 0
        || print_call.arguments.len() != 1
    {
        return false;
    }
    let ResolvedExprKind::Load(crate::core::ir::ResolvedPlace { base, projections }) =
        &print_call.arguments[0].value.kind
    else {
        return false;
    };
    if base != error_local
        || projections.len() != 2
        || !matches!(projections[0], ResolvedProjection::Tuple { index: 0, .. })
        || !matches!(projections[1], ResolvedProjection::Field { .. })
    {
        return false;
    }
    let ResolvedStmtKind::Drop(places) = &drop_statement.kind else {
        return false;
    };
    if places.len() != 1 || places[0].base != *error_local || !places[0].projections.is_empty() {
        return false;
    }
    matches!(
        block.result.as_deref().map(|result| &result.kind),
        Some(ResolvedExprKind::Literal(ResolvedLiteral::Int(0)))
    )
}

fn exact_destructured_failure_match_arm(
    expression: &ResolvedExpr,
    payload: &ResolvedPattern,
) -> bool {
    let ResolvedPatternKind::Tuple(elements) = &payload.kind else {
        return false;
    };
    let [source, error] = elements.as_slice() else {
        return false;
    };
    let ResolvedPatternKind::Binding {
        local: source_local,
        by_reference: None,
    } = &source.kind
    else {
        return false;
    };
    let ResolvedPatternKind::Binding {
        local: error_local,
        by_reference: None,
    } = &error.kind
    else {
        return false;
    };
    let ResolvedExprKind::Block(block) = &expression.kind else {
        return false;
    };
    let [print_statement, drop_source, drop_error] = block.statements.as_slice() else {
        return false;
    };
    let ResolvedStmtKind::Expr(ResolvedExpr {
        kind: ResolvedExprKind::Call(print_call),
        ..
    }) = &print_statement.kind
    else {
        return false;
    };
    if !matches!(&print_call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "println")
        || !print_call.type_arguments.is_empty()
        || !print_call.session.is_empty()
        || print_call.arguments.len() != 1
    {
        return false;
    }
    let ResolvedExprKind::Load(crate::core::ir::ResolvedPlace { base, projections }) =
        &print_call.arguments[0].value.kind
    else {
        return false;
    };
    if base != source_local
        || projections.len() != 1
        || !matches!(projections[0], ResolvedProjection::Field { .. })
    {
        return false;
    }
    let is_drop = |statement: &ResolvedStmt, local: &ResolvedLocalId| {
        matches!(&statement.kind, ResolvedStmtKind::Drop(places)
            if places.len() == 1
                && places[0].base == *local
                && places[0].projections.is_empty())
    };
    is_drop(drop_source, source_local)
        && is_drop(drop_error, error_local)
        && matches!(
            block.result.as_deref().map(|result| &result.kind),
            Some(ResolvedExprKind::Literal(ResolvedLiteral::Int(0)))
        )
}

fn executable_statements(
    block: &crate::core::ir::ResolvedBlock,
) -> Vec<crate::core::ir::ResolvedStmt> {
    block
        .statements
        .iter()
        .filter(|statement| !matches!(statement.kind, ResolvedStmtKind::Contract { .. }))
        .cloned()
        .collect()
}

fn exact_println_then_value_block(
    expression: &ResolvedExpr,
    expected_local: &ResolvedLocalId,
    expected_value: i64,
) -> bool {
    let ResolvedExprKind::Block(block) = &expression.kind else {
        return false;
    };
    let [print_statement] = block.statements.as_slice() else {
        return false;
    };
    let ResolvedStmtKind::Expr(ResolvedExpr {
        kind: ResolvedExprKind::Call(call),
        ..
    }) = &print_statement.kind
    else {
        return false;
    };
    if !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "println")
        || !call.type_arguments.is_empty()
        || !call.session.is_empty()
        || call.arguments.len() != 1
        || !matches!(
            &call.arguments[0].value.kind,
            ResolvedExprKind::Load(crate::core::ir::ResolvedPlace { base, projections })
                if base == expected_local && projections.is_empty()
        )
    {
        return false;
    }
    matches!(
        block.result.as_deref().map(|result| &result.kind),
        Some(ResolvedExprKind::Literal(ResolvedLiteral::Int(value))) if *value == expected_value
    )
}

fn exact_cross_state_match_main(
    program: &CheckedProgram,
    body: &ResolvedBody,
    transition: &crate::core::resolved::ResolvedTransition,
    flow: &crate::core::resolved::ResolvedFlow,
    target: &crate::core::StateId,
) -> bool {
    let statements = executable_statements(&body.root);
    let (first, match_expression) = match statements.as_slice() {
        [first, second]
            if body.root.result.as_deref().is_some_and(|result| {
                matches!(
                    &result.kind,
                    ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
                )
            }) =>
        {
            let ResolvedStmtKind::Expr(expression) = &second.kind else {
                return false;
            };
            (first, expression)
        }
        [first] => {
            let Some(expression) = body.root.result.as_deref() else {
                return false;
            };
            (first, expression)
        }
        _ => return false,
    };
    let ResolvedExprKind::Match { scrutinee, arms } = &match_expression.kind else {
        return false;
    };
    let (source_local, transition_owner) = match &first.kind {
        ResolvedStmtKind::Bind {
            pattern:
                crate::core::ir::ResolvedPattern {
                    kind:
                        ResolvedPatternKind::Binding {
                            local,
                            by_reference: None,
                        },
                    ..
                },
            initializer:
                Some(ResolvedExpr {
                    kind: ResolvedExprKind::Call(call),
                    ..
                }),
        } => match &call.callee {
            ResolvedCallee::Transition(owner) => (local, owner),
            _ => return false,
        },
        _ => return false,
    };
    if transition_owner != &transition.id {
        return false;
    }
    let ResolvedExprKind::Load(crate::core::ir::ResolvedPlace { base, projections }) =
        &scrutinee.kind
    else {
        return false;
    };
    if base != source_local || !projections.is_empty() || arms.len() != 2 {
        return false;
    }
    let target_nominal = format!("state:{}::{}", flow.id.0, target.name);
    let mut saw_ok = false;
    let mut saw_err = false;
    for arm in arms {
        match &arm.pattern.kind {
            ResolvedPatternKind::Constructor { variant, fields }
                if variant.0 == "builtin:variant:Result::Ok" =>
            {
                let [(_, nested)] = fields.as_slice() else {
                    return false;
                };
                let ResolvedPatternKind::Constructor {
                    variant: nested_variant,
                    fields: nested_fields,
                } = &nested.kind
                else {
                    return false;
                };
                let [(nested_field, nested_binding)] = nested_fields.as_slice() else {
                    return false;
                };
                let ResolvedPatternKind::Binding {
                    local,
                    by_reference: None,
                } = &nested_binding.kind
                else {
                    return false;
                };
                if nested_variant.0 != target_nominal
                    || nested_field.0.is_empty()
                    || !is_concrete_i32_resolved(program, &nested_binding.ty)
                    || !exact_println_block(&arm.body, Some(local))
                {
                    return false;
                }
                saw_ok = true;
            }
            ResolvedPatternKind::Constructor { variant, fields }
                if variant.0 == "builtin:variant:Result::Err" =>
            {
                let [(field, payload)] = fields.as_slice() else {
                    return false;
                };
                if field.0.is_empty()
                    || !matches!(payload.kind, ResolvedPatternKind::Wildcard)
                    || !exact_println_block(&arm.body, None)
                {
                    return false;
                }
                saw_err = true;
            }
            _ => return false,
        }
    }
    saw_ok && saw_err
}

fn is_concrete_i32_resolved(program: &CheckedProgram, ty: &crate::core::ResolvedTypeId) -> bool {
    matches!(
        program.resolved_types().get(ty),
        Some(ResolvedType::Primitive(crate::core::PrimitiveType::I32))
    )
}

fn exact_println_block(
    expression: &ResolvedExpr,
    expected_local: Option<&ResolvedLocalId>,
) -> bool {
    let ResolvedExprKind::Block(block) = &expression.kind else {
        return false;
    };
    let call = match (block.statements.as_slice(), block.result.as_deref()) {
        (
            [crate::core::ir::ResolvedStmt {
                kind:
                    ResolvedStmtKind::Expr(ResolvedExpr {
                        kind: ResolvedExprKind::Call(call),
                        ..
                    }),
                ..
            }],
            None,
        ) => call,
        (
            [],
            Some(ResolvedExpr {
                kind: ResolvedExprKind::Call(call),
                ..
            }),
        ) => call,
        _ => return false,
    };
    if !call.type_arguments.is_empty() || !call.session.is_empty() || call.arguments.len() != 1 {
        return false;
    }
    if !matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "println") {
        return false;
    }
    match (expected_local, &call.arguments[0].value.kind) {
        (Some(expected), ResolvedExprKind::Load(place)) => {
            place.base == *expected && place.projections.is_empty()
        }
        (None, ResolvedExprKind::Literal(ResolvedLiteral::Int(0))) => true,
        _ => false,
    }
}

fn block_contains_try(block: &crate::core::ir::ResolvedBlock) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => {
                initializer.as_ref().is_some_and(expr_contains_try)
            }
            ResolvedStmtKind::Expr(expression) => expr_contains_try(expression),
            ResolvedStmtKind::Return { value, .. } => value.as_ref().is_some_and(expr_contains_try),
            ResolvedStmtKind::While { condition, body } => {
                expr_contains_try(condition) || block_contains_try(body)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => expr_contains_try(initializer) || block_contains_try(body),
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_contains_try(initializer)
                    || block_contains_try(then_block)
                    || else_block.as_ref().is_some_and(block_contains_try)
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                block_contains_try(body)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                expr_contains_try(iterable) || block_contains_try(body)
            }
            ResolvedStmtKind::Pinned { value, body, .. } => {
                expr_contains_try(value) || block_contains_try(body)
            }
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => expr_contains_try(value),
            ResolvedStmtKind::Math(values) => values.iter().any(expr_contains_try),
            _ => false,
        })
        || block.result.as_deref().is_some_and(expr_contains_try)
}

fn expr_contains_try(expression: &ResolvedExpr) -> bool {
    match &expression.kind {
        ResolvedExprKind::Try { .. } => true,
        ResolvedExprKind::Unary { operand, .. } => expr_contains_try(operand),
        ResolvedExprKind::Binary { left, right, .. } => {
            expr_contains_try(left) || expr_contains_try(right)
        }
        ResolvedExprKind::Project { value, projection } => {
            expr_contains_try(value)
                || matches!(projection, ResolvedValueProjection::Index(index) if expr_contains_try(index))
        }
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => values.iter().any(expr_contains_try),
        ResolvedExprKind::Record { fields, rest, .. } => {
            fields.iter().any(|field| expr_contains_try(&field.value))
                || rest.as_deref().is_some_and(expr_contains_try)
        }
        ResolvedExprKind::Call(call) => call
            .arguments
            .iter()
            .any(|argument| expr_contains_try(&argument.value)),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            expr_contains_try(condition)
                || block_contains_try(then_block)
                || block_contains_try(else_block)
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            expr_contains_try(scrutinee) || arms.iter().any(|arm| expr_contains_try(&arm.body))
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            block_contains_try(block)
        }
        ResolvedExprKind::Cast { value, .. } => expr_contains_try(value),
        _ => false,
    }
}

/// Whether `program` contains an S8-shaped Flow transition candidate.
///
/// S8 is deliberately narrower than "a Flow that happens to compile": one
/// import-free Flow, one user transition, one non-Fault state, a concrete
/// `i32` payload, and a silent self-loop with no failure, parameter, pinned,
/// persistent, or actor boundary. The default dispatcher uses this typed
/// candidate predicate so a candidate whose body has not passed every
/// consumer preflight is rejected rather than silently returning to legacy.
pub fn is_s8_flow_transition_candidate(program: &CheckedProgram) -> bool {
    if program.has_imports() || program.flows().len() != 1 || !program.actors().is_empty() {
        return false;
    }

    let implemented = program
        .transitions()
        .values()
        .filter(|transition| {
            !transition.is_fallback && program.resolved_body(&transition.node_id).is_some()
        })
        .collect::<Vec<_>>();
    let [transition] = implemented.as_slice() else {
        return false;
    };

    let Some(flow) = program.flows().get(&transition.id.flow) else {
        return false;
    };
    let Some(source_state) = flow.states.get(&transition.id.source.name) else {
        return false;
    };
    let [(_, source_ty)] = source_state.payload.as_slice() else {
        return false;
    };
    let Some(target) = transition.targets.first() else {
        return false;
    };
    let Some(target_state) = flow.states.get(&target.name) else {
        return false;
    };

    transition.silent_transition
        && transition.targets.len() == 1
        && transition.targets[0] == transition.id.source
        && transition.params.is_empty()
        && transition.fails.is_none()
        && !transition.is_fallback
        && !transition.is_ffi_pinned
        && flow
            .states
            .keys()
            .filter(|name| name.as_str() != "Fault")
            .count()
            == 1
        && flow
            .transitions
            .iter()
            .filter(|id| {
                program
                    .transitions()
                    .get(*id)
                    .is_some_and(|item| !item.is_fallback)
            })
            .count()
            == 1
        && flow.persistent_fields.is_empty()
        && target_state.payload.len() == 1
        && is_concrete_i32_type(source_ty)
        && target_state
            .payload
            .first()
            .is_some_and(|(_, ty)| is_concrete_i32_type(ty))
}

/// Whether `program` is the exact S8 production island already closed by all
/// four consumers. This is stricter than the candidate predicate: the
/// admitted bodies contain only typed scalar/record expressions and the one
/// transition call. A candidate with `println`, a container, a variant,
/// control flow, an effect, or another call remains a candidate for the
/// dispatcher's fail-closed preflight, but does not enter the deleted native
/// compatibility route.
pub fn is_exact_s8_flow_transition(program: &CheckedProgram) -> bool {
    if !is_s8_flow_transition_candidate(program) {
        return false;
    }

    let implemented = program
        .transitions()
        .values()
        .filter(|transition| {
            !transition.is_fallback && program.resolved_body(&transition.node_id).is_some()
        })
        .collect::<Vec<_>>();
    let [transition] = implemented.as_slice() else {
        return false;
    };
    let Some(main) = program.resolved_body(&NodeId("function:main".into())) else {
        return false;
    };
    let Some(transition_body) = program.resolved_body(&transition.node_id) else {
        return false;
    };
    let mut saw_transition_call = false;
    let only_closed_island_callables = program.functions().values().all(|function| {
        if function.node_id == NodeId("function:main".into())
            || function.node_id == transition.node_id
        {
            return true;
        }
        // The CLI installs the checker-known prelude before checking. Prelude
        // helpers are declaration dependencies outside the user S8 island;
        // every other callable must be accounted for by the whole-program
        // admission rather than silently omitted from the verifier receipt.
        program
            .source_registry()
            .key(function.origin.user_span().source_id)
            .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi")
    });
    only_closed_island_callables
        && is_exact_s8_body(main, &transition.id, &mut saw_transition_call)
        && is_exact_s8_body(transition_body, &transition.id, &mut saw_transition_call)
        && saw_transition_call
}

/// The S8 native island is the closed, dependency-free transition example,
/// not every program that declares a compatible Flow.  This body predicate is
/// deliberately expressed over ResolvedBody facts: adding a builtin, a
/// container/variant operation, a control-flow expression, or an ordinary call
/// keeps the caller on the explicit compatibility route until that shape has
/// its own all-consumer contract.
fn is_exact_s8_body(
    body: &ResolvedBody,
    transition: &TransitionId,
    saw_transition_call: &mut bool,
) -> bool {
    body.captures.is_empty()
        && body.default_values.is_empty()
        && is_exact_s8_block(&body.root, transition, saw_transition_call)
}

fn is_exact_s8_block(
    block: &crate::core::ir::ResolvedBlock,
    transition: &TransitionId,
    saw_transition_call: &mut bool,
) -> bool {
    block
        .statements
        .iter()
        .all(|statement| match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer,
            } => {
                matches!(
                    &pattern.kind,
                    ResolvedPatternKind::Binding {
                        by_reference: None,
                        ..
                    }
                ) && initializer.as_ref().is_some_and(|initializer| {
                    is_exact_s8_expr(initializer, transition, saw_transition_call)
                })
            }
            ResolvedStmtKind::Return { value, .. } => value
                .as_ref()
                .is_some_and(|value| is_exact_s8_expr(value, transition, saw_transition_call)),
            _ => false,
        })
        && block
            .result
            .as_deref()
            .is_none_or(|result| is_exact_s8_expr(result, transition, saw_transition_call))
}

fn is_exact_s8_expr(
    expression: &ResolvedExpr,
    transition: &TransitionId,
    saw_transition_call: &mut bool,
) -> bool {
    if !expression.effects.is_empty() || !expression.backend_requirements.is_empty() {
        return false;
    }
    match &expression.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::Int(_)) => true,
        ResolvedExprKind::Load(crate::core::ir::ResolvedPlace { projections, .. }) => projections
            .iter()
            .all(|projection| matches!(projection, ResolvedProjection::Field { .. })),
        ResolvedExprKind::Project { value, projection } => {
            matches!(projection, ResolvedValueProjection::Field(_))
                && is_exact_s8_expr(value, transition, saw_transition_call)
        }
        ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::Add,
            left,
            right,
        } => {
            is_exact_s8_expr(left, transition, saw_transition_call)
                && is_exact_s8_expr(right, transition, saw_transition_call)
        }
        ResolvedExprKind::Call(call) => {
            let ResolvedCallee::Transition(callee) = &call.callee else {
                return false;
            };
            if callee != transition {
                return false;
            }
            *saw_transition_call = true;
            call.arguments
                .iter()
                .all(|argument| is_exact_s8_expr(&argument.value, transition, saw_transition_call))
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            rest.is_none()
                && fields
                    .iter()
                    .all(|field| is_exact_s8_expr(&field.value, transition, saw_transition_call))
        }
        _ => false,
    }
}

fn is_concrete_i32_type(ty: &Type) -> bool {
    match ty {
        Type::Located { ty, .. } => is_concrete_i32_type(ty),
        Type::Name(name, arguments) => name == "i32" && arguments.is_empty(),
        _ => false,
    }
}

fn is_concrete_string_type(ty: &Type) -> bool {
    match ty {
        Type::Located { ty, .. } => is_concrete_string_type(ty),
        Type::Name(name, arguments) => name == "string" && arguments.is_empty(),
        _ => false,
    }
}

/// The local recoverable retry island carries the state through the failure
/// envelope and back into the same transition.  Keep the admission explicit:
/// the payload may be a Copy `i32` or one owned `string`, whose aggregate
/// move/drop contract is now materialized by Canonical MIR.  Other payloads
/// remain outside the profile until their receipts and all four consumers are
/// closed.
fn is_supported_local_retry_state_type(ty: &Type) -> bool {
    match ty {
        Type::Located { ty, .. } => is_supported_local_retry_state_type(ty),
        Type::Name(name, arguments) => arguments.is_empty() && (name == "i32" || name == "string"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_exact_s8_flow_transition, is_flow_failure_retry_candidate,
        is_s8_flow_transition_candidate,
    };

    fn checked(source: &str) -> crate::core::CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file).expect("check")
    }

    #[test]
    fn cross_state_result_match_is_a_narrow_recoverable_flow_candidate() {
        let source = include_str!(
            "../../../tests/real_world/flow_state_match_fail_result_dual_backend.mimi"
        );
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        assert!(is_flow_failure_retry_candidate(&program));
    }

    #[test]
    fn cross_state_failure_match_with_source_read_is_a_narrow_candidate() {
        let source = include_str!(
            "../../../tests/real_world/flow_state_match_fail_result_failure_dual_backend.mimi"
        );
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        assert!(is_flow_failure_retry_candidate(&program));
    }

    #[test]
    fn local_retry_with_owned_string_state_is_a_narrow_candidate() {
        let source = include_str!("../../../tests/fixtures/mir_m3_flow_retry_string_state.mimi");
        let program = checked(source);
        assert!(is_flow_failure_retry_candidate(&program));
    }

    #[test]
    fn multifield_cross_state_string_receipt_is_a_narrow_candidate() {
        let source = include_str!(
            "../../../tests/fixtures/mir_m3_flow_multifield_string_source_receipt.mimi"
        );
        let program = checked(source);
        assert!(is_flow_failure_retry_candidate(&program));
    }

    #[test]
    fn multifield_cross_state_string_receipt_rejects_non_string_residual() {
        let source = include_str!(
            "../../../tests/fixtures/mir_m3_flow_multifield_string_source_receipt.mimi"
        );
        let mutated = source
            .replace("note: string", "note: i32")
            .replace("note: \"discarded\"", "note: 7");
        let program = checked(&mutated);
        assert!(!is_flow_failure_retry_candidate(&program));
    }

    #[test]
    fn rejects_cross_state_result_match_when_body_or_arm_shape_drifts() {
        let source = include_str!(
            "../../../tests/real_world/flow_state_match_fail_result_dual_backend.mimi"
        );
        for mutated in [
            source.replace("self.n + 1", "self.n - 1"),
            source.replace("println(n)", "println(1)"),
        ] {
            let tokens = crate::lexer::Lexer::new(&mutated).tokenize().expect("lex");
            let file = crate::parser::Parser::new(tokens)
                .parse_file()
                .expect("parse");
            let program = crate::core::check_program(&file).expect("check");
            assert!(
                !is_flow_failure_retry_candidate(&program),
                "drifted F2 shape was admitted: {mutated}"
            );
        }
    }

    #[test]
    fn recognizes_only_the_typed_s8_shape() {
        let program = checked(
            "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) c2.n }",
        );
        assert!(is_exact_s8_flow_transition(&program));
    }

    #[test]
    fn rejects_an_imported_or_non_i32_flow() {
        let program = checked(
            "flow Counter { state Zero { n: i64 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i64 { let c = Zero { n: 41 } let c2 = Counter::inc(c) c2.n }",
        );
        assert!(!is_exact_s8_flow_transition(&program));
    }

    #[test]
    fn keeps_unsupported_body_as_candidate_but_outside_exact_island() {
        let program = checked(
            "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) println(c2.n) c2.n }",
        );
        assert!(is_s8_flow_transition_candidate(&program));
        assert!(!is_exact_s8_flow_transition(&program));
    }

    #[test]
    fn rejects_an_unrelated_user_callable_from_the_exact_island() {
        let program = checked(
            "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func helper() -> i32 { println(7) 7 } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) c2.n }",
        );
        assert!(is_s8_flow_transition_candidate(&program));
        assert!(!is_exact_s8_flow_transition(&program));
    }
}
