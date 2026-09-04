//! Whole-program contract for the concrete Copy `Option<i32>` projection island.
//!
//! The direct `Option<i32>.unwrap()` node already has a canonical
//! `VariantProject` receipt.  This module is the program-level admission and
//! MIR-only capability gate that permits that shape on the default route.  It
//! intentionally excludes generic instances, Flow transitions, consuming
//! projections, and every other Option/Result payload family.

use std::collections::BTreeSet;

use crate::core::ir::{ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedStmtKind};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{MirGlueKind, MirLayout, MirOwnership, MirTypeKind};
use crate::core::{CheckedProgram, NodeId, PrimitiveType, ResolvedTypeId};

use super::{MirFunction, MirInstructionKind, MirTerminator};

/// Versioned default-route island for direct Copy `Option<i32>` projection.
pub const COPY_OPTION_I32_VARIANT_ISLAND: &str = "copy-option-i32-variant-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOptionI32VariantAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Classify the concrete checker shape before MIR construction.  A body is
/// admitted only when it is closed and contains a direct `Option<i32>.unwrap`
/// call.  Any other Option unwrap in a candidate program makes the whole
/// profile mixed, so it cannot silently fall through to legacy.
pub fn classify_copy_option_i32_variant_admission(
    program: &CheckedProgram,
) -> CopyOptionI32VariantAdmission {
    let mut candidate = false;
    let mut mixed = super::islands::has_mixed_coverage(program);
    for callable in program.callables().values() {
        if super::islands::is_prelude_origin(program, &callable.body.root.origin) {
            continue;
        }
        let body_is_closed = super::option_island::option_body_is_closed(&callable.body.root);
        let has_any_unwrap = body_has_option_unwrap(program, &callable.body.root);
        let has_i32_unwrap = body_has_option_i32_unwrap(program, &callable.body.root);
        let has_option_string_shape =
            super::option_island::block_has_option_string_switch(program, &callable.body.root);
        if body_is_closed {
            candidate |= has_i32_unwrap;
            mixed |= has_any_unwrap && !has_i32_unwrap;
            mixed |= has_option_string_shape;
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
        CopyOptionI32VariantAdmission::OutsideProfile
    } else if mixed {
        CopyOptionI32VariantAdmission::MixedCoverage
    } else {
        CopyOptionI32VariantAdmission::CompleteCoverage
    }
}

fn is_option_with_inner(
    program: &CheckedProgram,
    ty: &ResolvedTypeId,
    expected: Option<PrimitiveType>,
) -> bool {
    let Some(crate::core::ir::ResolvedType::Option(inner)) = program.resolved_types().get(ty)
    else {
        return false;
    };
    match program.resolved_types().get(inner) {
        Some(crate::core::ir::ResolvedType::Primitive(primitive)) => {
            expected.is_none_or(|wanted| *primitive == wanted)
        }
        _ => false,
    }
}

fn call_is_option_unwrap(
    program: &CheckedProgram,
    call: &crate::core::ir::ResolvedCall,
    expected: Option<PrimitiveType>,
) -> bool {
    matches!(&call.callee, ResolvedCallee::Builtin(name)
        if name.as_str() == "builtin.method.option.unwrap")
        && call.arguments.len() == 1
        && is_option_with_inner(program, &call.arguments[0].value.ty, expected)
}

fn body_has_option_unwrap(
    program: &CheckedProgram,
    block: &crate::core::ir::ResolvedBlock,
) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|expr| expr_has_option_unwrap(program, expr, None)),
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => expr_has_option_unwrap(program, value, None),
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => value
                .as_ref()
                .is_some_and(|expr| expr_has_option_unwrap(program, expr, None)),
            ResolvedStmtKind::While { condition, body } => {
                expr_has_option_unwrap(program, condition, None)
                    || body_has_option_unwrap(program, body)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => {
                expr_has_option_unwrap(program, initializer, None)
                    || body_has_option_unwrap(program, body)
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_has_option_unwrap(program, initializer, None)
                    || body_has_option_unwrap(program, then_block)
                    || else_block
                        .as_ref()
                        .is_some_and(|block| body_has_option_unwrap(program, block))
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                body_has_option_unwrap(program, body)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                expr_has_option_unwrap(program, iterable, None)
                    || body_has_option_unwrap(program, body)
            }
            ResolvedStmtKind::Math(expressions) => expressions
                .iter()
                .any(|expr| expr_has_option_unwrap(program, expr, None)),
            ResolvedStmtKind::Pinned { value, body, .. } => {
                expr_has_option_unwrap(program, value, None)
                    || body_has_option_unwrap(program, body)
            }
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_) => false,
        })
        || block
            .result
            .as_ref()
            .is_some_and(|expr| expr_has_option_unwrap(program, expr, None))
}

fn body_has_option_i32_unwrap(
    program: &CheckedProgram,
    block: &crate::core::ir::ResolvedBlock,
) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => {
                initializer.as_ref().is_some_and(|expr| {
                    expr_has_option_unwrap(program, expr, Some(PrimitiveType::I32))
                })
            }
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => expr_has_option_unwrap(program, value, Some(PrimitiveType::I32)),
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                value.as_ref().is_some_and(|expr| {
                    expr_has_option_unwrap(program, expr, Some(PrimitiveType::I32))
                })
            }
            ResolvedStmtKind::While { condition, body } => {
                expr_has_option_unwrap(program, condition, Some(PrimitiveType::I32))
                    || body_has_option_i32_unwrap(program, body)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => {
                expr_has_option_unwrap(program, initializer, Some(PrimitiveType::I32))
                    || body_has_option_i32_unwrap(program, body)
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_has_option_unwrap(program, initializer, Some(PrimitiveType::I32))
                    || body_has_option_i32_unwrap(program, then_block)
                    || else_block
                        .as_ref()
                        .is_some_and(|block| body_has_option_i32_unwrap(program, block))
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                body_has_option_i32_unwrap(program, body)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                expr_has_option_unwrap(program, iterable, Some(PrimitiveType::I32))
                    || body_has_option_i32_unwrap(program, body)
            }
            ResolvedStmtKind::Math(expressions) => expressions
                .iter()
                .any(|expr| expr_has_option_unwrap(program, expr, Some(PrimitiveType::I32))),
            ResolvedStmtKind::Pinned { value, body, .. } => {
                expr_has_option_unwrap(program, value, Some(PrimitiveType::I32))
                    || body_has_option_i32_unwrap(program, body)
            }
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_) => false,
        })
        || block
            .result
            .as_ref()
            .is_some_and(|expr| expr_has_option_unwrap(program, expr, Some(PrimitiveType::I32)))
}

fn expr_has_option_unwrap(
    program: &CheckedProgram,
    expression: &ResolvedExpr,
    expected: Option<PrimitiveType>,
) -> bool {
    match &expression.kind {
        ResolvedExprKind::Call(call) => {
            call_is_option_unwrap(program, call, expected)
                || call
                    .arguments
                    .iter()
                    .any(|argument| expr_has_option_unwrap(program, &argument.value, expected))
        }
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => match expected {
            Some(primitive) => {
                body_has_option_i32_unwrap(program, block) && primitive == PrimitiveType::I32
            }
            None => body_has_option_unwrap(program, block),
        },
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            expr_has_option_unwrap(program, condition, expected)
                || match expected {
                    Some(primitive) if primitive == PrimitiveType::I32 => {
                        body_has_option_i32_unwrap(program, then_block)
                            || body_has_option_i32_unwrap(program, else_block)
                    }
                    Some(_) => false,
                    None => {
                        body_has_option_unwrap(program, then_block)
                            || body_has_option_unwrap(program, else_block)
                    }
                }
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            expr_has_option_unwrap(program, scrutinee, expected)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|guard| expr_has_option_unwrap(program, guard, expected))
                        || expr_has_option_unwrap(program, &arm.body, expected)
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
        | ResolvedExprKind::Try { value, .. } => expr_has_option_unwrap(program, value, expected),
        ResolvedExprKind::Binary { left, right, .. } => {
            expr_has_option_unwrap(program, left, expected)
                || expr_has_option_unwrap(program, right, expected)
        }
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => values
            .iter()
            .any(|value| expr_has_option_unwrap(program, value, expected)),
        ResolvedExprKind::Map(entries) => entries.iter().any(|(key, value)| {
            expr_has_option_unwrap(program, key, expected)
                || expr_has_option_unwrap(program, value, expected)
        }),
        ResolvedExprKind::Record { fields, rest, .. } => {
            rest.as_ref()
                .is_some_and(|value| expr_has_option_unwrap(program, value, expected))
                || fields
                    .iter()
                    .any(|field| expr_has_option_unwrap(program, &field.value, expected))
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            expr_has_option_unwrap(program, value, expected)
                || expr_has_option_unwrap(program, iterable, expected)
                || guard
                    .as_ref()
                    .is_some_and(|guard| expr_has_option_unwrap(program, guard, expected))
        }
        ResolvedExprKind::FString(parts) => parts.iter().any(|part| match part {
            crate::core::ir::ResolvedFStringPart::Text(_) => false,
            crate::core::ir::ResolvedFStringPart::Interpolation(value) => {
                expr_has_option_unwrap(program, value, expected)
            }
        }),
        ResolvedExprKind::Range { start, end } => {
            expr_has_option_unwrap(program, start, expected)
                || expr_has_option_unwrap(program, end, expected)
        }
        ResolvedExprKind::Slice { target, start, end } => {
            expr_has_option_unwrap(program, target, expected)
                || start
                    .as_ref()
                    .is_some_and(|value| expr_has_option_unwrap(program, value, expected))
                || end
                    .as_ref()
                    .is_some_and(|value| expr_has_option_unwrap(program, value, expected))
        }
        ResolvedExprKind::Lambda(lambda) => match expected {
            Some(primitive) if primitive == PrimitiveType::I32 => {
                body_has_option_i32_unwrap(program, &lambda.body)
            }
            Some(_) => false,
            None => body_has_option_unwrap(program, &lambda.body),
        },
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => false,
    }
}

/// MIR-side materialization receipt counterpart of checker admission.
pub fn contains_copy_option_i32_variant_candidate(program: &MirProgram) -> bool {
    program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                let MirInstructionKind::VariantProject { base, result, .. } = &instruction.kind
                else {
                    return false;
                };
                let Some(base_ty) = function.values.get(base).map(|value| &value.ty) else {
                    return false;
                };
                let Some(result_ty) = function.values.get(result).map(|value| &value.ty) else {
                    return false;
                };
                is_copy_option_i32(program, base_ty) && is_i32(program, result_ty)
            })
        })
    })
}

fn is_i32(program: &MirProgram, ty: &ResolvedTypeId) -> bool {
    program
        .type_catalog()
        .get(ty)
        .is_some_and(|descriptor| descriptor.kind == MirTypeKind::Primitive(PrimitiveType::I32))
}

fn is_copy_option_i32(program: &MirProgram, ty: &ResolvedTypeId) -> bool {
    let Some(descriptor) = program.type_catalog().get(ty) else {
        return false;
    };
    let MirLayout::Option { inner, .. } = &descriptor.layout else {
        return false;
    };
    is_i32(program, inner)
        && program
            .type_catalog()
            .validate_copy_option_i32_variant(ty)
            .is_ok()
}

/// Validate the complete Copy `Option<i32>` island using only canonical MIR
/// and TypeDesc receipts.  This is intentionally a narrow operation gate over
/// the generic MIR validator: unsupported variant instructions are rejected,
/// while ordinary scalar code remains available to the shared consumers.
pub fn validate_copy_option_i32_variant_island(program: &MirProgram) -> Result<(), Vec<String>> {
    let mut validator = CopyOptionI32VariantValidator {
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

struct CopyOptionI32VariantValidator<'a> {
    program: &'a MirProgram,
    errors: BTreeSet<String>,
    saw_projection: bool,
}

impl<'a> CopyOptionI32VariantValidator<'a> {
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
                "{COPY_OPTION_I32_VARIANT_ISLAND} does not admit generic MIR instances"
            ));
        }
        if !self.program.transitions().is_empty() {
            self.error(format!(
                "{COPY_OPTION_I32_VARIANT_ISLAND} does not admit FlowTransition contracts"
            ));
        }
        for function in self.program.functions().values() {
            self.validate_function(function);
        }
        if !self.saw_projection {
            self.error(format!(
                "{COPY_OPTION_I32_VARIANT_ISLAND} has no executable Option<i32> projection"
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
                        let Some(result_ty) = function.values.get(result).map(|value| &value.ty) else {
                            self.error(format!("{} variant result is absent", instruction.id));
                            continue;
                        };
                        let Some(receipt) = contract else {
                            self.error(format!("{} has no TypeDesc projection receipt", instruction.id));
                            continue;
                        };
                        if !is_copy_option_i32(self.program, base_ty) || !is_i32(self.program, result_ty) {
                            self.error(format!(
                                "{} is outside the Copy Option<i32> projection shape",
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
                        if receipt.projection.ownership != MirOwnership::Copy
                            || receipt.projection.move_out_glue != MirGlueKind::Noop
                        {
                            self.error(format!(
                                "{} projection does not prove Copy + Noop glue",
                                instruction.id
                            ));
                        }
                    }
                    MirInstructionKind::VariantProjectMove { .. } => self.error(format!(
                        "{} consuming variant operation is outside {COPY_OPTION_I32_VARIANT_ISLAND}",
                        instruction.id
                    )),
                    _ => {}
                }
            }
            if matches!(block.terminator, MirTerminator::SwitchMove { .. }) {
                self.error(format!(
                    "{} consuming variant terminator is outside {COPY_OPTION_I32_VARIANT_ISLAND}",
                    block.id
                ));
            }
        }
    }

    fn error(&mut self, message: String) {
        self.errors.insert(message);
    }
}
