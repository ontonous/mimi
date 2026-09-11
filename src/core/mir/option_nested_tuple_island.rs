//! Default-route contract for the bounded consuming `Option<(...)>` family.
//!
//! This is intentionally separate from the concrete `Option<string>` island:
//! the nested tuple receipt is a second-stage projection, not a variant
//! payload that can be inferred from a generic `Option<T>` operation.  The
//! checker admission is therefore source-typed and the MIR materialization
//! gate is receipt-based.

use std::collections::BTreeSet;

use crate::core::ir::{
    MatchArm, Permission, ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedPattern,
    ResolvedPatternKind, ResolvedStmtKind, ResolvedType,
};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::MirTerminator;
use crate::core::{CheckedProgram, PrimitiveType, ResolvedTypeId};

use super::islands::has_mixed_coverage;

/// Stable name for the bounded non-generic nested tuple variant island.
pub const NON_COPY_OPTION_NESTED_TUPLE_VARIANT_ISLAND: &str =
    "non-copy-option-nested-tuple-variant-v1";

/// Checker-owned admission state for the bounded nested tuple switch shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionNestedTupleVariantAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Classify only the concrete, non-generic `Option<(scalar/String, ...)>`
/// consuming match shape.  Generic Option calls and Result/Flow payloads do
/// not enter this profile.
pub fn classify_option_nested_tuple_variant_admission(
    program: &CheckedProgram,
) -> OptionNestedTupleVariantAdmission {
    let mut candidate = false;
    let mut mixed = has_mixed_coverage(program);

    for callable in program.callables().values() {
        if super::islands::is_prelude_origin(program, &callable.body.root.origin)
            || !callable.signature.generic_parameters.is_empty()
        {
            continue;
        }
        let closed = nested_tuple_body_is_closed(&callable.body.root);
        candidate |= block_has_option_nested_tuple_switch(program, &callable.body.root);
        if !closed
            || !callable.signature.effects.is_empty()
            || callable.signature.parameters.iter().any(|parameter| {
                matches!(
                    parameter.permission,
                    Some(Permission::View | Permission::Mutate)
                )
            })
            || !callable.body.captures.is_empty()
            || !callable.body.default_values.is_empty()
        {
            mixed = true;
        }
    }

    if !candidate {
        OptionNestedTupleVariantAdmission::OutsideProfile
    } else if mixed {
        OptionNestedTupleVariantAdmission::MixedCoverage
    } else {
        OptionNestedTupleVariantAdmission::CompleteCoverage
    }
}

fn block_has_option_nested_tuple_switch(
    program: &CheckedProgram,
    block: &crate::core::ir::ResolvedBlock,
) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|expression| expr_has_option_nested_tuple_switch(program, expression)),
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => expr_has_option_nested_tuple_switch(program, value),
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => value
                .as_ref()
                .is_some_and(|expression| expr_has_option_nested_tuple_switch(program, expression)),
            ResolvedStmtKind::While { condition, body } => {
                expr_has_option_nested_tuple_switch(program, condition)
                    || block_has_option_nested_tuple_switch(program, body)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => {
                expr_has_option_nested_tuple_switch(program, initializer)
                    || block_has_option_nested_tuple_switch(program, body)
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_has_option_nested_tuple_switch(program, initializer)
                    || block_has_option_nested_tuple_switch(program, then_block)
                    || else_block
                        .as_ref()
                        .is_some_and(|body| block_has_option_nested_tuple_switch(program, body))
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                block_has_option_nested_tuple_switch(program, body)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                expr_has_option_nested_tuple_switch(program, iterable)
                    || block_has_option_nested_tuple_switch(program, body)
            }
            ResolvedStmtKind::Math(expressions) => expressions
                .iter()
                .any(|expression| expr_has_option_nested_tuple_switch(program, expression)),
            ResolvedStmtKind::Pinned { value, body, .. } => {
                expr_has_option_nested_tuple_switch(program, value)
                    || block_has_option_nested_tuple_switch(program, body)
            }
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_) => false,
        })
        || block
            .result
            .as_ref()
            .is_some_and(|expression| expr_has_option_nested_tuple_switch(program, expression))
}

fn expr_has_option_nested_tuple_switch(
    program: &CheckedProgram,
    expression: &ResolvedExpr,
) -> bool {
    match &expression.kind {
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => block_has_option_nested_tuple_switch(program, block),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            expr_has_option_nested_tuple_switch(program, condition)
                || block_has_option_nested_tuple_switch(program, then_block)
                || block_has_option_nested_tuple_switch(program, else_block)
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            (is_option_nested_tuple_type(program, &scrutinee.ty)
                && matches!(scrutinee.kind, ResolvedExprKind::Load(_)))
                || expr_has_option_nested_tuple_switch(program, scrutinee)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|guard| expr_has_option_nested_tuple_switch(program, guard))
                        || expr_has_option_nested_tuple_switch(program, &arm.body)
                })
        }
        ResolvedExprKind::Call(call) => call
            .arguments
            .iter()
            .any(|argument| expr_has_option_nested_tuple_switch(program, &argument.value)),
        ResolvedExprKind::Project { value, .. }
        | ResolvedExprKind::Unary { operand: value, .. }
        | ResolvedExprKind::Cast { value, .. }
        | ResolvedExprKind::Old(value) => expr_has_option_nested_tuple_switch(program, value),
        ResolvedExprKind::Binary { left, right, .. } => {
            expr_has_option_nested_tuple_switch(program, left)
                || expr_has_option_nested_tuple_switch(program, right)
        }
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => values
            .iter()
            .any(|value| expr_has_option_nested_tuple_switch(program, value)),
        ResolvedExprKind::Map(entries) => entries.iter().any(|(key, value)| {
            expr_has_option_nested_tuple_switch(program, key)
                || expr_has_option_nested_tuple_switch(program, value)
        }),
        ResolvedExprKind::Record { fields, rest, .. } => {
            rest.as_ref()
                .is_some_and(|value| expr_has_option_nested_tuple_switch(program, value))
                || fields
                    .iter()
                    .any(|field| expr_has_option_nested_tuple_switch(program, &field.value))
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            expr_has_option_nested_tuple_switch(program, value)
                || expr_has_option_nested_tuple_switch(program, iterable)
                || guard
                    .as_ref()
                    .is_some_and(|guard| expr_has_option_nested_tuple_switch(program, guard))
        }
        ResolvedExprKind::OptionalChain { receiver, .. }
        | ResolvedExprKind::TypeOf(receiver)
        | ResolvedExprKind::Spawn(receiver)
        | ResolvedExprKind::Await(receiver) => {
            expr_has_option_nested_tuple_switch(program, receiver)
        }
        ResolvedExprKind::Try { value, .. } => expr_has_option_nested_tuple_switch(program, value),
        ResolvedExprKind::FString(parts) => parts.iter().any(|part| match part {
            crate::core::ir::ResolvedFStringPart::Text(_) => false,
            crate::core::ir::ResolvedFStringPart::Interpolation(value) => {
                expr_has_option_nested_tuple_switch(program, value)
            }
        }),
        ResolvedExprKind::Range { start, end } => {
            expr_has_option_nested_tuple_switch(program, start)
                || expr_has_option_nested_tuple_switch(program, end)
        }
        ResolvedExprKind::Slice { target, start, end } => {
            expr_has_option_nested_tuple_switch(program, target)
                || start
                    .as_ref()
                    .is_some_and(|value| expr_has_option_nested_tuple_switch(program, value))
                || end
                    .as_ref()
                    .is_some_and(|value| expr_has_option_nested_tuple_switch(program, value))
        }
        ResolvedExprKind::Lambda(lambda) => {
            block_has_option_nested_tuple_switch(program, &lambda.body)
        }
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => false,
    }
}

fn is_option_nested_tuple_type(program: &CheckedProgram, ty: &ResolvedTypeId) -> bool {
    let Some(ResolvedType::Option(inner)) = program.resolved_types().get(ty) else {
        return false;
    };
    let Some(ResolvedType::Tuple(elements)) = program.resolved_types().get(inner) else {
        return false;
    };
    elements.len() >= 2
        && tuple_payload_is_supported(program, elements)
        && tuple_contains_owned_string(program, inner)
}

fn tuple_payload_is_supported(program: &CheckedProgram, elements: &[ResolvedTypeId]) -> bool {
    elements
        .iter()
        .all(|element| match program.resolved_types().get(element) {
            Some(ResolvedType::Primitive(_)) => true,
            Some(ResolvedType::Tuple(children)) if !children.is_empty() => {
                tuple_payload_is_supported(program, children)
            }
            _ => false,
        })
}

fn tuple_contains_owned_string(program: &CheckedProgram, ty: &ResolvedTypeId) -> bool {
    match program.resolved_types().get(ty) {
        Some(ResolvedType::Primitive(PrimitiveType::String)) => true,
        Some(ResolvedType::Tuple(elements)) => elements
            .iter()
            .any(|element| tuple_contains_owned_string(program, element)),
        _ => false,
    }
}

fn nested_tuple_body_is_closed(block: &crate::core::ir::ResolvedBlock) -> bool {
    block.statements.iter().all(|statement| {
        if !statement.backend_requirements.is_empty() {
            return false;
        }
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer,
            } => {
                nested_tuple_pattern_is_closed(pattern)
                    && initializer.as_ref().is_none_or(nested_tuple_expr_is_closed)
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                value.as_ref().is_none_or(nested_tuple_expr_is_closed)
            }
            ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => nested_tuple_expr_is_closed(value),
            ResolvedStmtKind::Drop(_) => true,
            _ => false,
        }
    }) && block
        .result
        .as_ref()
        .is_none_or(|expression| nested_tuple_expr_is_closed(expression))
}

fn nested_tuple_pattern_is_closed(pattern: &ResolvedPattern) -> bool {
    match &pattern.kind {
        ResolvedPatternKind::Wildcard
        | ResolvedPatternKind::Binding {
            by_reference: None, ..
        }
        | ResolvedPatternKind::Literal(_) => true,
        ResolvedPatternKind::Constructor { fields, .. } => fields
            .iter()
            .all(|(_, field)| nested_tuple_pattern_is_closed(field)),
        ResolvedPatternKind::Tuple(elements) => elements.iter().all(nested_tuple_pattern_is_closed),
        _ => false,
    }
}

fn nested_tuple_expr_is_closed(expression: &ResolvedExpr) -> bool {
    if !expression.effects.is_empty() || !expression.backend_requirements.is_empty() {
        return false;
    }
    match &expression.kind {
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_) => true,
        ResolvedExprKind::Call(call) => {
            let allowed = match &call.callee {
                ResolvedCallee::Builtin(name) => {
                    matches!(name.as_str(), "Some" | "None" | "println")
                }
                ResolvedCallee::Function(_) => {
                    call.type_arguments.is_empty()
                        && call.permission.is_none()
                        && call.effects.is_empty()
                        && call.session.is_empty()
                }
                _ => false,
            };
            allowed
                && call
                    .arguments
                    .iter()
                    .all(|argument| nested_tuple_expr_is_closed(&argument.value))
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            nested_tuple_expr_is_closed(left) && nested_tuple_expr_is_closed(right)
        }
        ResolvedExprKind::Unary { operand, op } => {
            !matches!(
                op,
                crate::core::ir::ResolvedUnaryOp::BorrowShared
                    | crate::core::ir::ResolvedUnaryOp::BorrowMutable
                    | crate::core::ir::ResolvedUnaryOp::Dereference
            ) && nested_tuple_expr_is_closed(operand)
        }
        ResolvedExprKind::Cast { value, .. } | ResolvedExprKind::Old(value) => {
            nested_tuple_expr_is_closed(value)
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            nested_tuple_body_is_closed(block)
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            nested_tuple_expr_is_closed(scrutinee)
                && if arms.iter().any(|arm| {
                    matches!(
                        &arm.pattern.kind,
                        ResolvedPatternKind::Constructor { variant, .. }
                            if variant.0 == "builtin:variant:Option::Some"
                                || variant.0 == "builtin:variant:Option::None"
                    )
                }) {
                    is_direct_nested_tuple_match(arms)
                } else {
                    arms.iter().all(|arm| {
                        arm.guard.is_none()
                            && nested_tuple_pattern_is_closed(&arm.pattern)
                            && nested_tuple_expr_is_closed(&arm.body)
                    })
                }
        }
        ResolvedExprKind::Tuple(values) => values.iter().all(nested_tuple_expr_is_closed),
        _ => false,
    }
}

fn is_direct_nested_tuple_match(arms: &[MatchArm]) -> bool {
    if arms.len() != 2 {
        return false;
    }
    let mut saw_some = false;
    let mut saw_none = false;
    for arm in arms {
        if arm.guard.is_some() {
            return false;
        }
        let ResolvedPatternKind::Constructor { variant, fields } = &arm.pattern.kind else {
            return false;
        };
        if variant.0 == "builtin:variant:Option::Some" {
            if saw_some || fields.len() != 1 {
                return false;
            }
            let Some((_, payload)) = fields.first() else {
                return false;
            };
            let ResolvedPatternKind::Tuple(elements) = &payload.kind else {
                return false;
            };
            if elements.iter().any(|element| {
                !matches!(
                    element.kind,
                    ResolvedPatternKind::Binding {
                        by_reference: None,
                        ..
                    }
                )
            }) {
                return false;
            }
            saw_some = true;
        } else if variant.0 == "builtin:variant:Option::None" {
            if saw_none || !fields.is_empty() {
                return false;
            }
            saw_none = true;
        } else {
            return false;
        }
    }
    saw_some && saw_none
}

/// Return whether canonical MIR contains the receipt-bearing nested tuple
/// operation selected by this profile.
pub fn contains_option_nested_tuple_variant_candidate(program: &MirProgram) -> bool {
    program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            let MirTerminator::SwitchMove { scrutinee, arms } = &block.terminator else {
                return false;
            };
            function.values.get(scrutinee).is_some_and(|value| {
                program
                    .type_catalog()
                    .get(&value.ty)
                    .is_some_and(|descriptor| descriptor.kind == super::types::MirTypeKind::Option)
                    && arms.iter().any(|arm| {
                        arm.bindings
                            .iter()
                            .any(|binding| binding.nested_tuple.is_some())
                    })
            })
        })
    })
}

/// Validate every nested tuple receipt in a canonical program before any
/// default consumer is allowed to observe it.
pub fn validate_option_nested_tuple_variant_island(
    program: &MirProgram,
) -> Result<(), Vec<String>> {
    let mut errors = BTreeSet::new();
    let mut saw_nested = false;
    for function in program.functions().values() {
        for block in function.blocks.values() {
            let MirTerminator::SwitchMove { scrutinee, arms } = &block.terminator else {
                continue;
            };
            let Some(scrutinee_value) = function.values.get(scrutinee) else {
                errors.insert(format!(
                    "nested tuple switch scrutinee '{}' is absent from canonical values",
                    scrutinee
                ));
                continue;
            };
            let has_nested = arms.iter().any(|arm| {
                arm.bindings
                    .iter()
                    .any(|binding| binding.nested_tuple.is_some())
            });
            let is_option = program
                .type_catalog()
                .get(&scrutinee_value.ty)
                .is_some_and(|descriptor| descriptor.kind == super::types::MirTypeKind::Option);
            if has_nested && is_option {
                saw_nested = true;
                if let Err(error) = program
                    .type_catalog()
                    .validate_option_nested_tuple_variant(&scrutinee_value.ty)
                {
                    errors.insert(error);
                }
                if let Err(error) = program
                    .type_catalog()
                    .validate_variant_switch_move_contract(&scrutinee_value.ty, arms)
                {
                    errors.insert(error);
                }
            } else if !has_nested && program
                .type_catalog()
                .get(&scrutinee_value.ty)
                .is_some_and(|descriptor| {
                    matches!(
                        descriptor.layout,
                        crate::core::mir::types::MirLayout::Option { ref inner, .. }
                            if program
                                .type_catalog()
                                .get(inner)
                                .is_some_and(|inner| {
                                    matches!(inner.kind, crate::core::mir::types::MirTypeKind::Tuple { .. })
                                        && inner.ownership != crate::core::mir::types::MirOwnership::Copy
                                })
                    )
                })
            {
                errors.insert(
                    "non-Copy Option tuple SwitchMove is missing nested tuple projection receipts"
                        .into(),
                );
            }
        }
    }
    if !saw_nested {
        errors.insert("canonical program has no nested tuple SwitchMove receipt".into());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.into_iter().collect())
    }
}
