//! Whole-program contract for the concrete Copy `Result<i32, i32>` projection.
//!
//! This is deliberately a separate route profile from the Copy `Option` islands:
//! the MIR node is shared, but checker admission, TypeDesc proof and stable
//! diagnostics must not let an Option receipt accidentally qualify a Result.
//!
//! R6-1090 admits the guarded-free `match` face: an unguarded constructor arm
//! over a Copy `Result<i32, i32>` scrutinee lowers to a plain `Switch` with
//! `Variant` arms, so the classifier, the route receipt, and the island
//! validator all accept exactly that shape (mirroring the R6-1089 Copy Option
//! match admission).

use std::collections::BTreeSet;

use crate::core::ir::{
    ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedPatternKind, ResolvedStmtKind,
};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{MirGlueKind, MirOwnership, MirTypeKind};
use crate::core::{CheckedProgram, NodeId, PrimitiveType, ResolvedTypeId};

use super::{MirFunction, MirInstructionKind, MirSwitchCase, MirTerminator};

/// Versioned default-route island for direct Copy `Result<i32, i32>.unwrap()`
/// and total `unwrap_or` projection.
pub const COPY_RESULT_I32_VARIANT_ISLAND: &str = "copy-result-i32-variant-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyResultI32VariantAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Classify the concrete checker shape before MIR construction. The profile
/// is closed: every Result unwrap/unwrap_or must use the canonical builtin,
/// and only the exact `Result<i32, i32>` shape is complete.  The R6-1090
/// match face is family-strict like the parameterized Copy Option islands:
/// only an i32/i32 `match` counts as a candidate, and a foreign-payload
/// `match` is invisible to this island (it stays on the legacy route, which
/// lowers it correctly as an ordinary plain `Switch`).
pub fn classify_copy_result_i32_variant_admission(
    program: &CheckedProgram,
) -> CopyResultI32VariantAdmission {
    let mut candidate = false;
    let mut mixed = super::islands::has_mixed_coverage(program);
    for callable in program.callables().values() {
        if super::islands::is_prelude_origin(program, &callable.body.root.origin) {
            continue;
        }
        // A specialized generic Result projection has its own admission and
        // receipt. Do not let the generic template's builtin unwrap make the
        // concrete Result<i32, i32> island report mixed coverage before the
        // generic route can claim it.
        if super::islands::is_generic_result_projection_callable(program, callable)
            || super::islands::is_generic_result_projection_fallback_callable(program, callable)
        {
            continue;
        }
        let body_is_closed =
            super::option_island::option_body_is_closed(program, &callable.body.root);
        let has_any_unwrap = body_has_result_unwrap(program, &callable.body.root, false);
        let has_expected_unwrap = body_has_result_unwrap(program, &callable.body.root, true);
        let has_expected_switch = body_has_result_i32_switch(program, &callable.body.root);
        if body_is_closed {
            // Any Result::unwrap is a profile candidate, and an i32/i32 match
            // joins it.  Only the concrete i32/i32 payload is complete;
            // unsupported unwrap payloads must become a MixedCoverage hard
            // rejection rather than silently entering legacy.
            candidate |= has_any_unwrap || has_expected_switch;
            mixed |= has_any_unwrap && !has_expected_unwrap;
        } else {
            mixed = true;
        }
        if !callable.signature.generic_parameters.is_empty()
            || !callable.signature.effects.is_empty()
            || callable.signature.parameters.iter().any(|parameter| {
                matches!(
                    parameter.permission,
                    Some(crate::core::ir::Permission::View | crate::core::ir::Permission::Mutate)
                )
            })
            || !callable.body.captures.is_empty()
            || !callable.body.default_values.is_empty()
        {
            mixed = true;
        }
    }
    if !candidate {
        CopyResultI32VariantAdmission::OutsideProfile
    } else if mixed {
        CopyResultI32VariantAdmission::MixedCoverage
    } else {
        CopyResultI32VariantAdmission::CompleteCoverage
    }
}

fn is_result_i32_i32(program: &CheckedProgram, ty: &ResolvedTypeId) -> bool {
    let Some(crate::core::ir::ResolvedType::Result { ok, error }) =
        program.resolved_types().get(ty)
    else {
        return false;
    };
    matches!(
        (
            program.resolved_types().get(ok),
            program.resolved_types().get(error)
        ),
        (
            Some(crate::core::ir::ResolvedType::Primitive(PrimitiveType::I32)),
            Some(crate::core::ir::ResolvedType::Primitive(PrimitiveType::I32))
        )
    )
}

fn call_is_result_unwrap(
    program: &CheckedProgram,
    call: &crate::core::ir::ResolvedCall,
    expected: bool,
) -> bool {
    let ResolvedCallee::Builtin(name) = &call.callee else {
        return false;
    };
    let is_unwrap = name.as_str() == "builtin.method.result.unwrap";
    let is_unwrap_or = name.as_str() == "builtin.method.result.unwrap_or";
    let arity_ok =
        (is_unwrap && call.arguments.len() == 1) || (is_unwrap_or && call.arguments.len() == 2);
    let Some(receiver) = call.arguments.first() else {
        return false;
    };
    let is_result_shape = matches!(
        program.resolved_types().get(&receiver.value.ty),
        Some(crate::core::ir::ResolvedType::Result { .. })
    );
    if !arity_ok || !is_result_shape {
        return false;
    }
    if !expected {
        return true;
    }
    if !is_result_i32_i32(program, &receiver.value.ty) {
        return false;
    }
    if is_unwrap {
        return true;
    }
    if !is_unwrap_or {
        return false;
    }
    call.arguments.get(1).is_some_and(|argument| {
        matches!(
            program.resolved_types().get(&argument.value.ty),
            Some(crate::core::ir::ResolvedType::Primitive(PrimitiveType::I32))
        )
    })
}

fn body_has_result_unwrap(
    program: &CheckedProgram,
    block: &crate::core::ir::ResolvedBlock,
    expected: bool,
) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|expr| expr_has_result_unwrap(program, expr, expected)),
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => expr_has_result_unwrap(program, value, expected),
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => value
                .as_ref()
                .is_some_and(|expr| expr_has_result_unwrap(program, expr, expected)),
            ResolvedStmtKind::While { condition, body } => {
                expr_has_result_unwrap(program, condition, expected)
                    || body_has_result_unwrap(program, body, expected)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => {
                expr_has_result_unwrap(program, initializer, expected)
                    || body_has_result_unwrap(program, body, expected)
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_has_result_unwrap(program, initializer, expected)
                    || body_has_result_unwrap(program, then_block, expected)
                    || else_block
                        .as_ref()
                        .is_some_and(|block| body_has_result_unwrap(program, block, expected))
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                body_has_result_unwrap(program, body, expected)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                expr_has_result_unwrap(program, iterable, expected)
                    || body_has_result_unwrap(program, body, expected)
            }
            ResolvedStmtKind::Math(expressions) => expressions
                .iter()
                .any(|expr| expr_has_result_unwrap(program, expr, expected)),
            ResolvedStmtKind::Pinned { value, body, .. } => {
                expr_has_result_unwrap(program, value, expected)
                    || body_has_result_unwrap(program, body, expected)
            }
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_) => false,
        })
        || block
            .result
            .as_ref()
            .is_some_and(|expr| expr_has_result_unwrap(program, expr, expected))
}

fn expr_has_result_unwrap(
    program: &CheckedProgram,
    expression: &ResolvedExpr,
    expected: bool,
) -> bool {
    match &expression.kind {
        ResolvedExprKind::Call(call) => {
            call_is_result_unwrap(program, call, expected)
                || call
                    .arguments
                    .iter()
                    .any(|argument| expr_has_result_unwrap(program, &argument.value, expected))
        }
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => body_has_result_unwrap(program, block, expected),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            expr_has_result_unwrap(program, condition, expected)
                || body_has_result_unwrap(program, then_block, expected)
                || body_has_result_unwrap(program, else_block, expected)
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            expr_has_result_unwrap(program, scrutinee, expected)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|guard| expr_has_result_unwrap(program, guard, expected))
                        || expr_has_result_unwrap(program, &arm.body, expected)
                })
        }
        ResolvedExprKind::Project { value, .. }
        | ResolvedExprKind::Unary { operand: value, .. }
        | ResolvedExprKind::Cast { value, .. }
        | ResolvedExprKind::Old(value)
        | ResolvedExprKind::OptionalChain {
            receiver: value, ..
        }
        | ResolvedExprKind::TypeOf(value)
        | ResolvedExprKind::Spawn(value)
        | ResolvedExprKind::Await(value)
        | ResolvedExprKind::Try { value, .. } => expr_has_result_unwrap(program, value, expected),
        ResolvedExprKind::Binary { left, right, .. } => {
            expr_has_result_unwrap(program, left, expected)
                || expr_has_result_unwrap(program, right, expected)
        }
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => values
            .iter()
            .any(|value| expr_has_result_unwrap(program, value, expected)),
        ResolvedExprKind::Map(entries) => entries.iter().any(|(key, value)| {
            expr_has_result_unwrap(program, key, expected)
                || expr_has_result_unwrap(program, value, expected)
        }),
        ResolvedExprKind::Record { fields, rest, .. } => {
            rest.as_ref()
                .is_some_and(|value| expr_has_result_unwrap(program, value, expected))
                || fields
                    .iter()
                    .any(|field| expr_has_result_unwrap(program, &field.value, expected))
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            expr_has_result_unwrap(program, value, expected)
                || expr_has_result_unwrap(program, iterable, expected)
                || guard
                    .as_ref()
                    .is_some_and(|guard| expr_has_result_unwrap(program, guard, expected))
        }
        ResolvedExprKind::FString(parts) => parts.iter().any(|part| match part {
            crate::core::ir::ResolvedFStringPart::Text(_) => false,
            crate::core::ir::ResolvedFStringPart::Interpolation(value) => {
                expr_has_result_unwrap(program, value, expected)
            }
        }),
        ResolvedExprKind::Range { start, end } => {
            expr_has_result_unwrap(program, start, expected)
                || expr_has_result_unwrap(program, end, expected)
        }
        ResolvedExprKind::Slice { target, start, end } => {
            expr_has_result_unwrap(program, target, expected)
                || start
                    .as_ref()
                    .is_some_and(|value| expr_has_result_unwrap(program, value, expected))
                || end
                    .as_ref()
                    .is_some_and(|value| expr_has_result_unwrap(program, value, expected))
        }
        ResolvedExprKind::Lambda(lambda) => body_has_result_unwrap(program, &lambda.body, expected),
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => false,
    }
}

/// Is the `match` scrutinee the concrete island family `Result<i32, i32>`?
/// Foreign-payload Result matches are invisible to this single-family island
/// (the parameterized Copy Option islands give each family its own admission;
/// here the non-i32/i32 payload simply stays outside the profile).
fn match_scrutinee_is_result_i32(program: &CheckedProgram, scrutinee: &ResolvedExpr) -> bool {
    match program.resolved_types().get(&scrutinee.ty) {
        Some(crate::core::ir::ResolvedType::Result { ok, error }) => matches!(
            (
                program.resolved_types().get(ok),
                program.resolved_types().get(error)
            ),
            (
                Some(crate::core::ir::ResolvedType::Primitive(PrimitiveType::I32)),
                Some(crate::core::ir::ResolvedType::Primitive(PrimitiveType::I32))
            )
        ),
        _ => false,
    }
}

/// Scan a body for `match` expressions over concrete `Result<i32, i32>`
/// scrutinees.  A match only counts when at least one constructor arm is
/// unguarded — MIR Phase 0 lowers exactly that shape to a `Switch` with
/// `Variant` arms, so the checker-side candidate stays aligned with the
/// materialized receipt (mirroring the R6-1089 Copy Option match scan).
fn body_has_result_i32_switch(
    program: &CheckedProgram,
    block: &crate::core::ir::ResolvedBlock,
) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|expr| expr_has_result_i32_switch(program, expr)),
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => expr_has_result_i32_switch(program, value),
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => value
                .as_ref()
                .is_some_and(|expr| expr_has_result_i32_switch(program, expr)),
            ResolvedStmtKind::While { condition, body } => {
                expr_has_result_i32_switch(program, condition)
                    || body_has_result_i32_switch(program, body)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => {
                expr_has_result_i32_switch(program, initializer)
                    || body_has_result_i32_switch(program, body)
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_has_result_i32_switch(program, initializer)
                    || body_has_result_i32_switch(program, then_block)
                    || else_block
                        .as_ref()
                        .is_some_and(|block| body_has_result_i32_switch(program, block))
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                body_has_result_i32_switch(program, body)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                expr_has_result_i32_switch(program, iterable)
                    || body_has_result_i32_switch(program, body)
            }
            ResolvedStmtKind::Math(expressions) => expressions
                .iter()
                .any(|expr| expr_has_result_i32_switch(program, expr)),
            ResolvedStmtKind::Pinned { value, body, .. } => {
                expr_has_result_i32_switch(program, value)
                    || body_has_result_i32_switch(program, body)
            }
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_) => false,
        })
        || block
            .result
            .as_ref()
            .is_some_and(|expr| expr_has_result_i32_switch(program, expr))
}

fn expr_has_result_i32_switch(program: &CheckedProgram, expression: &ResolvedExpr) -> bool {
    match &expression.kind {
        ResolvedExprKind::Match { scrutinee, arms } => {
            let switch_receipt = arms.iter().any(|arm| {
                arm.guard.is_none()
                    && matches!(arm.pattern.kind, ResolvedPatternKind::Constructor { .. })
            }) && match_scrutinee_is_result_i32(program, scrutinee);
            switch_receipt
                || expr_has_result_i32_switch(program, scrutinee)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|guard| expr_has_result_i32_switch(program, guard))
                        || expr_has_result_i32_switch(program, &arm.body)
                })
        }
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => body_has_result_i32_switch(program, block),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            expr_has_result_i32_switch(program, condition)
                || body_has_result_i32_switch(program, then_block)
                || body_has_result_i32_switch(program, else_block)
        }
        ResolvedExprKind::Project { value, .. }
        | ResolvedExprKind::Unary { operand: value, .. }
        | ResolvedExprKind::Cast { value, .. }
        | ResolvedExprKind::Old(value)
        | ResolvedExprKind::OptionalChain {
            receiver: value, ..
        }
        | ResolvedExprKind::TypeOf(value)
        | ResolvedExprKind::Spawn(value)
        | ResolvedExprKind::Await(value)
        | ResolvedExprKind::Try { value, .. } => expr_has_result_i32_switch(program, value),
        ResolvedExprKind::Binary { left, right, .. } => {
            expr_has_result_i32_switch(program, left) || expr_has_result_i32_switch(program, right)
        }
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => values
            .iter()
            .any(|value| expr_has_result_i32_switch(program, value)),
        ResolvedExprKind::Map(entries) => entries.iter().any(|(key, value)| {
            expr_has_result_i32_switch(program, key) || expr_has_result_i32_switch(program, value)
        }),
        ResolvedExprKind::Record { fields, rest, .. } => {
            rest.as_ref()
                .is_some_and(|value| expr_has_result_i32_switch(program, value))
                || fields
                    .iter()
                    .any(|field| expr_has_result_i32_switch(program, &field.value))
        }
        ResolvedExprKind::Call(call) => call
            .arguments
            .iter()
            .any(|argument| expr_has_result_i32_switch(program, &argument.value)),
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            expr_has_result_i32_switch(program, value)
                || expr_has_result_i32_switch(program, iterable)
                || guard
                    .as_ref()
                    .is_some_and(|guard| expr_has_result_i32_switch(program, guard))
        }
        ResolvedExprKind::FString(parts) => parts.iter().any(|part| match part {
            crate::core::ir::ResolvedFStringPart::Text(_) => false,
            crate::core::ir::ResolvedFStringPart::Interpolation(value) => {
                expr_has_result_i32_switch(program, value)
            }
        }),
        ResolvedExprKind::Range { start, end } => {
            expr_has_result_i32_switch(program, start) || expr_has_result_i32_switch(program, end)
        }
        ResolvedExprKind::Slice { target, start, end } => {
            expr_has_result_i32_switch(program, target)
                || start
                    .as_ref()
                    .is_some_and(|value| expr_has_result_i32_switch(program, value))
                || end
                    .as_ref()
                    .is_some_and(|value| expr_has_result_i32_switch(program, value))
        }
        ResolvedExprKind::Lambda(lambda) => body_has_result_i32_switch(program, &lambda.body),
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => false,
    }
}
/// Detect one concrete Copy Result projection receipt in MIR.
pub fn contains_copy_result_i32_variant_candidate(program: &MirProgram) -> bool {
    // Generic Result projection instances carry a specialized receipt and
    // belong to their own route profile; do not reclassify their VariantProject
    // body as the direct concrete Result<i32, i32> island.
    let generic_instance_functions = program
        .instances()
        .values()
        .map(|instance| instance.function.clone())
        .collect::<BTreeSet<_>>();
    program
        .functions()
        .values()
        .filter(|function| !generic_instance_functions.contains(&function.owner))
        .any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    let (base, result, fallback) = match &instruction.kind {
                        MirInstructionKind::VariantProject { base, result, .. } => {
                            (base, result, None)
                        }
                        MirInstructionKind::VariantProjectOr {
                            base,
                            result,
                            fallback,
                            ..
                        } => (base, result, Some(fallback)),
                        _ => return false,
                    };
                    let Some(base_ty) = function.values.get(base).map(|value| &value.ty) else {
                        return false;
                    };
                    let Some(result_ty) = function.values.get(result).map(|value| &value.ty) else {
                        return false;
                    };
                    let result_is_i32 =
                        program
                            .type_catalog()
                            .get(result_ty)
                            .is_some_and(|descriptor| {
                                descriptor.kind == MirTypeKind::Primitive(PrimitiveType::I32)
                            });
                    let fallback_is_i32 = fallback.is_none_or(|fallback| {
                        function
                            .values
                            .get(fallback)
                            .and_then(|value| program.type_catalog().get(&value.ty))
                            .is_some_and(|descriptor| {
                                descriptor.kind == MirTypeKind::Primitive(PrimitiveType::I32)
                            })
                    });
                    is_copy_result_i32(program, base_ty) && result_is_i32 && fallback_is_i32
                })
                    // R6-1090: the match face is a plain `Switch` with
                    // `Variant` arms over a cloned Copy `Result<i32, i32>`
                    // scrutinee — no `VariantProject` is emitted, so the
                    // switch itself is the executable projection receipt.
                    || match &block.terminator {
                        MirTerminator::Switch { scrutinee, arms } => {
                            function
                                .values
                                .get(scrutinee)
                                .is_some_and(|value| is_copy_result_i32(program, &value.ty))
                                && arms
                                    .iter()
                                    .any(|arm| matches!(arm.case, MirSwitchCase::Variant(_)))
                        }
                        _ => false,
                    }
            })
        })
}

fn is_copy_result_i32(program: &MirProgram, ty: &ResolvedTypeId) -> bool {
    program
        .type_catalog()
        .validate_copy_result_i32_variant(ty)
        .is_ok()
}

/// Validate the complete Copy Result island using only canonical MIR and
/// TypeDesc receipts. Unsupported variant forms are hard errors.
pub fn validate_copy_result_i32_variant_island(program: &MirProgram) -> Result<(), Vec<String>> {
    let mut validator = CopyResultI32VariantValidator {
        program,
        errors: BTreeSet::new(),
        saw_projection: false,
    };
    validator.validate();
    if validator.errors.is_empty() {
        Ok(())
    } else {
        Err(validator.errors.into_iter().collect())
    }
}

struct CopyResultI32VariantValidator<'a> {
    program: &'a MirProgram,
    errors: BTreeSet<String>,
    saw_projection: bool,
}

impl<'a> CopyResultI32VariantValidator<'a> {
    fn validate(&mut self) {
        if !self
            .program
            .functions()
            .contains_key(&NodeId("function:main".into()))
        {
            self.error("program has no canonical function:main".into());
        }
        if !self.program.instances().is_empty() {
            self.error(format!(
                "{COPY_RESULT_I32_VARIANT_ISLAND} does not admit generic MIR instances"
            ));
        }
        if !self.program.transitions().is_empty() {
            self.error(format!(
                "{COPY_RESULT_I32_VARIANT_ISLAND} does not admit FlowTransition contracts"
            ));
        }
        for function in self.program.functions().values() {
            self.validate_function(function);
        }
        if !self.saw_projection {
            self.error(format!(
                "{COPY_RESULT_I32_VARIANT_ISLAND} has no executable Copy Result projection"
            ));
        }
    }

    fn validate_function(&mut self, function: &MirFunction) {
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                match &instruction.kind {
                    MirInstructionKind::VariantProject {
                        base,
                        result,
                        contract,
                    } => {
                        self.saw_projection = true;
                        let Some(base_ty) = function.values.get(base).map(|value| &value.ty) else {
                            self.error(format!("{} variant base is absent", instruction.id));
                            continue;
                        };
                        let Some(result_ty) = function.values.get(result).map(|value| &value.ty)
                        else {
                            self.error(format!("{} variant result is absent", instruction.id));
                            continue;
                        };
                        let Some(receipt) = contract else {
                            self.error(format!(
                                "{} has no TypeDesc projection receipt",
                                instruction.id
                            ));
                            continue;
                        };
                        if !is_copy_result_i32(self.program, base_ty)
                            || self
                                .program
                                .type_catalog()
                                .get(result_ty)
                                .is_none_or(|descriptor| {
                                    descriptor.kind != MirTypeKind::Primitive(PrimitiveType::I32)
                                })
                        {
                            self.error(format!(
                                "{} is outside the Copy Result<i32, i32> projection shape",
                                instruction.id
                            ));
                            continue;
                        }
                        if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_variant_projection_trap_receipt(base_ty, result_ty, receipt)
                        {
                            self.error(format!("{} receipt rejected: {message}", instruction.id));
                        }
                        if receipt.variant_name != "Ok"
                            || receipt.projection.ownership != MirOwnership::Copy
                            || receipt.projection.move_out_glue != MirGlueKind::Noop
                        {
                            self.error(format!(
                                "{} projection does not prove Copy Ok + Noop glue",
                                instruction.id
                            ));
                        }
                        if self
                            .program
                            .type_catalog()
                            .validate_copy_result_i32_variant(base_ty)
                            .is_err()
                        {
                            self.error(format!(
                                "{} source TypeDesc is outside {COPY_RESULT_I32_VARIANT_ISLAND}",
                                instruction.id
                            ));
                        }
                    }
                    MirInstructionKind::VariantProjectOr {
                        base,
                        fallback,
                        result,
                        contract,
                    } => {
                        self.saw_projection = true;
                        let Some(base_ty) = function.values.get(base).map(|value| &value.ty) else {
                            self.error(format!("{} variant base is absent", instruction.id));
                            continue;
                        };
                        let Some(result_ty) = function.values.get(result).map(|value| &value.ty)
                        else {
                            self.error(format!("{} variant result is absent", instruction.id));
                            continue;
                        };
                        let Some(fallback_ty) =
                            function.values.get(fallback).map(|value| &value.ty)
                        else {
                            self.error(format!("{} fallback value is absent", instruction.id));
                            continue;
                        };
                        let Some(receipt) = contract else {
                            self.error(format!(
                                "{} has no TypeDesc fallback projection receipt",
                                instruction.id
                            ));
                            continue;
                        };
                        if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_variant_projection_fallback_receipt(
                                base_ty,
                                result_ty,
                                fallback_ty,
                                receipt,
                            )
                        {
                            self.error(format!(
                                "{} fallback receipt rejected: {message}",
                                instruction.id
                            ));
                        }
                    }
                    MirInstructionKind::VariantProjectMove { .. } => self.error(format!(
                        "{} consuming variant operation is outside {}",
                        instruction.id, COPY_RESULT_I32_VARIANT_ISLAND
                    )),
                    _ => {}
                }
            }
            match &block.terminator {
                MirTerminator::SwitchMove { .. } => self.error(format!(
                    "{} consuming variant terminator is outside {}",
                    block.id, COPY_RESULT_I32_VARIANT_ISLAND
                )),
                // R6-1090: the match face is a plain `Switch` with `Variant`
                // arms over a cloned Copy scrutinee.  Only this island's own
                // family is envelope-checked here; foreign switches (scalar
                // literal arms, user enums, other islands' families) remain
                // ordinary code for the shared structural validator.
                MirTerminator::Switch { scrutinee, arms } => {
                    let has_variant_arm = arms
                        .iter()
                        .any(|arm| matches!(arm.case, MirSwitchCase::Variant(_)));
                    let scrutinee_is_island = function
                        .values
                        .get(scrutinee)
                        .is_some_and(|value| is_copy_result_i32(self.program, &value.ty));
                    if has_variant_arm && scrutinee_is_island {
                        self.saw_projection = true;
                        for arm in arms {
                            for binding in &arm.bindings {
                                if binding.nested_tuple.is_some() {
                                    self.error(format!(
                                        "{} consuming tuple destructure is outside {}",
                                        block.id, COPY_RESULT_I32_VARIANT_ISLAND
                                    ));
                                }
                                if !self
                                    .program
                                    .type_catalog()
                                    .get(&binding.projection.field_ty)
                                    .is_some_and(|descriptor| {
                                        descriptor.kind
                                            == MirTypeKind::Primitive(PrimitiveType::I32)
                                    })
                                {
                                    self.error(format!(
                                        "{} switch binding is outside the Copy Result<i32, i32> projection shape",
                                        block.id
                                    ));
                                }
                                if binding.projection.ownership != MirOwnership::Copy
                                    || binding.projection.move_out_glue != MirGlueKind::Noop
                                {
                                    self.error(format!(
                                        "{} switch binding does not prove Copy + Noop glue",
                                        block.id
                                    ));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn error(&mut self, message: String) {
        self.errors.insert(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_fixture_is_complete_admission() {
        let source = include_str!("../../../tests/fixtures/mir_native_result_i32_unwrap.mimi");
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_copy_result_i32_variant_admission(&checked),
            CopyResultI32VariantAdmission::CompleteCoverage
        );
    }

    #[test]
    fn err_projection_keeps_the_canonical_active_tag_trap_receipt() {
        let source = include_str!("../../../tests/fixtures/mir_native_result_i32_unwrap_err.mimi");
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("lower");
        validate_copy_result_i32_variant_island(&program).expect("Result island validator");
        let projection = program
            .functions()
            .get(&NodeId("function:main".into()))
            .expect("main MIR function")
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::VariantProject {
                    contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .expect("Err unwrap must carry a projection receipt");
        assert_eq!(projection.variant_name, "Ok");
        assert_eq!(projection.discriminant, 0);
        assert_eq!(
            projection.trap_code,
            crate::core::mir::types::MIR_VARIANT_PROJECTION_TRAP_CODE
        );
    }

    #[test]
    fn unwrap_or_carries_both_result_tag_identities_and_copy_fallback_receipt() {
        let source = include_str!("../../../tests/fixtures/mir_native_result_i32_unwrap_or.mimi");
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_copy_result_i32_variant_admission(&checked),
            CopyResultI32VariantAdmission::CompleteCoverage
        );
        let program = MirProgram::from_checked_program(&checked).expect("lower");
        validate_copy_result_i32_variant_island(&program).expect("Result unwrap_or island");
        let receipt = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::VariantProjectOr {
                    contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .expect("unwrap_or must carry a fallback receipt");
        assert_eq!(receipt.variant_name, "Ok");
        assert_eq!(receipt.discriminant, 0);
        assert_eq!(receipt.fallback_variant_name, "Err");
        assert_eq!(receipt.fallback_discriminant, 1);
        assert_eq!(receipt.result_ty, receipt.fallback_ty);
    }
}

#[cfg(test)]
mod match_tests {
    use super::*;
    use crate::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
    use crate::interp::bytecode::{compile_mir_program, BytecodeVM};

    fn check(source: &str) -> CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file).expect("check")
    }

    #[test]
    fn result_i32_match_binds_and_materializes_switch_receipt() {
        let source = include_str!("../../../tests/fixtures/mir_native_result_i32_match.mimi");
        let checked = check(source);
        assert_eq!(
            classify_copy_result_i32_variant_admission(&checked),
            CopyResultI32VariantAdmission::CompleteCoverage,
            "the bound-payload match face must classify complete"
        );
        let program = MirProgram::from_checked_program(&checked).expect("lower");
        assert!(
            contains_copy_result_i32_variant_candidate(&program),
            "the match face must materialize a receipt for the route"
        );
        validate_copy_result_i32_variant_island(&program)
            .unwrap_or_else(|errors| panic!("match island: {errors:?}"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .unwrap_or_else(|error| panic!("reference: {error:?}"));
        assert_eq!(reference, MirRuntimeValue::Int(0));
        let bytecode =
            compile_mir_program(&program).unwrap_or_else(|error| panic!("bytecode: {error:?}"));
        BytecodeVM::new(bytecode)
            .run()
            .unwrap_or_else(|error| panic!("vm: {error:?}"));
    }

    #[test]
    fn result_i32_match_wildcard_arms_discriminate_without_binding() {
        let source = r#"
func pick(r: Result<i32, i32>) -> i32 {
    ensures: result >= 0
    match r {
        Ok(_) => 8,
        Err(_) => 3
    }
}
func main() -> i32 {
    let ok: Result<i32, i32> = Ok(1)
    let bad: Result<i32, i32> = Err(2)
    pick(ok) + pick(bad) - 11
}
"#;
        let checked = check(source);
        assert_eq!(
            classify_copy_result_i32_variant_admission(&checked),
            CopyResultI32VariantAdmission::CompleteCoverage,
            "a binding-free constructor arm must still count as a candidate"
        );
        let program = MirProgram::from_checked_program(&checked).expect("lower");
        assert!(
            contains_copy_result_i32_variant_candidate(&program),
            "a binding-free Variant arm is the executable Result receipt"
        );
        validate_copy_result_i32_variant_island(&program)
            .unwrap_or_else(|errors| panic!("wildcard island: {errors:?}"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .unwrap_or_else(|error| panic!("reference: {error:?}"));
        assert_eq!(reference, MirRuntimeValue::Int(0));
    }

    #[test]
    fn result_match_foreign_payload_stays_outside_profile() {
        let source = r#"
func pick(r: Result<string, i32>) -> i32 {
    ensures: result >= 0
    match r {
        Ok(_) => 8,
        Err(_) => 3
    }
}
func main() -> i32 {
    let ok: Result<string, i32> = Ok("a")
    pick(ok)
}
"#;
        let checked = check(source);
        assert_eq!(
            classify_copy_result_i32_variant_admission(&checked),
            CopyResultI32VariantAdmission::OutsideProfile,
            "a foreign-payload Result match is invisible to the single-family \
             island and stays on the legacy route"
        );
    }

    #[test]
    fn result_i32_match_guard_stays_outside_profile() {
        let source = r#"
func pick(r: Result<i32, i32>) -> i32 {
    match r {
        Ok(x) if x > 0 => x,
        Err(e) => e,
        _ => 0
    }
}
func main() -> i32 {
    let ok: Result<i32, i32> = Ok(41)
    pick(ok)
}
"#;
        let checked = check(source);
        assert_eq!(
            classify_copy_result_i32_variant_admission(&checked),
            CopyResultI32VariantAdmission::OutsideProfile,
            "a guarded match body is not closed, so it is not an island candidate"
        );
        // The guard body cannot lower to MIR at all ("match guards require
        // CFG lowering"), so there is nothing further to materialize — the
        // classification gate alone keeps it on the legacy route.
    }
}
