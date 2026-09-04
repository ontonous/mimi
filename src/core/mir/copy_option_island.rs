//! Whole-program contracts for concrete Copy `Option` projection islands.
//!
//! The direct `Option<primitive>.unwrap()` node already has a canonical
//! `VariantProject` receipt. This module is the program-level admission and
//! MIR-only capability gate that permits those shapes on the default route. It
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

/// Versioned default-route island for direct Copy `Option<bool>` projection.
pub const COPY_OPTION_BOOL_VARIANT_ISLAND: &str = "copy-option-bool-variant-v1";

/// Versioned default-route island for direct Copy `Option<i64>` projection.
pub const COPY_OPTION_I64_VARIANT_ISLAND: &str = "copy-option-i64-variant-v1";

/// Versioned default-route island for direct Copy `Option<f64>` projection.
pub const COPY_OPTION_F64_VARIANT_ISLAND: &str = "copy-option-f64-variant-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOptionVariantAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Compatibility alias retained for the S114 i32 public API.
pub type CopyOptionI32VariantAdmission = CopyOptionVariantAdmission;

/// Classify the concrete checker shape before MIR construction. A body is
/// admitted only when it is closed and contains a direct Copy
/// `Option<primitive>.unwrap`/`unwrap_or` call. Any other Option projection in a candidate
/// program makes the whole profile mixed, so it cannot silently fall through
/// to legacy.
pub fn classify_copy_option_i32_variant_admission(
    program: &CheckedProgram,
) -> CopyOptionVariantAdmission {
    classify_copy_option_variant_admission(program, PrimitiveType::I32)
}

/// Classify one concrete Copy `Option<primitive>` projection family before
/// canonical MIR construction. The public i32 wrapper keeps S114's stable
/// API while S115/S116 reuse the exact same checker scan for bool/i64.
pub fn classify_copy_option_variant_admission(
    program: &CheckedProgram,
    expected: PrimitiveType,
) -> CopyOptionVariantAdmission {
    let mut candidate = false;
    let mut mixed = super::islands::has_mixed_coverage(program);
    for callable in program.callables().values() {
        if super::islands::is_prelude_origin(program, &callable.body.root.origin) {
            continue;
        }
        let body_is_closed = super::option_island::option_body_is_closed(&callable.body.root);
        let has_any_unwrap = body_has_option_unwrap(program, &callable.body.root);
        let has_expected_unwrap =
            body_has_option_primitive_unwrap(program, &callable.body.root, expected);
        let has_option_string_shape =
            super::option_island::block_has_option_string_switch(program, &callable.body.root);
        if body_is_closed {
            candidate |= has_expected_unwrap;
            mixed |= has_any_unwrap && !has_expected_unwrap;
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
        CopyOptionVariantAdmission::OutsideProfile
    } else if mixed {
        CopyOptionVariantAdmission::MixedCoverage
    } else {
        CopyOptionVariantAdmission::CompleteCoverage
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
    let ResolvedCallee::Builtin(name) = &call.callee else {
        return false;
    };
    let is_unwrap = name.as_str() == "builtin.method.option.unwrap";
    let is_unwrap_or = name.as_str() == "builtin.method.option.unwrap_or";
    let arity_ok =
        (is_unwrap && call.arguments.len() == 1) || (is_unwrap_or && call.arguments.len() == 2);
    let Some(receiver) = call.arguments.first() else {
        return false;
    };
    if !arity_ok || !is_option_with_inner(program, &receiver.value.ty, expected) {
        return false;
    }
    if !is_unwrap_or {
        return true;
    }
    let Some(fallback) = call.arguments.get(1) else {
        return false;
    };
    let Some(crate::core::ir::ResolvedType::Primitive(inner)) = program
        .resolved_types()
        .get(&receiver.value.ty)
        .and_then(|ty| {
            let crate::core::ir::ResolvedType::Option(inner) = ty else {
                return None;
            };
            program.resolved_types().get(inner)
        })
    else {
        return false;
    };
    matches!(program.resolved_types().get(&fallback.value.ty),
        Some(crate::core::ir::ResolvedType::Primitive(fallback_inner)) if fallback_inner == inner)
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

fn body_has_option_primitive_unwrap(
    program: &CheckedProgram,
    block: &crate::core::ir::ResolvedBlock,
    expected: PrimitiveType,
) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|expr| expr_has_option_unwrap(program, expr, Some(expected))),
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => expr_has_option_unwrap(program, value, Some(expected)),
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => value
                .as_ref()
                .is_some_and(|expr| expr_has_option_unwrap(program, expr, Some(expected))),
            ResolvedStmtKind::While { condition, body } => {
                expr_has_option_unwrap(program, condition, Some(expected))
                    || body_has_option_primitive_unwrap(program, body, expected)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => {
                expr_has_option_unwrap(program, initializer, Some(expected))
                    || body_has_option_primitive_unwrap(program, body, expected)
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_has_option_unwrap(program, initializer, Some(expected))
                    || body_has_option_primitive_unwrap(program, then_block, expected)
                    || else_block.as_ref().is_some_and(|block| {
                        body_has_option_primitive_unwrap(program, block, expected)
                    })
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                body_has_option_primitive_unwrap(program, body, expected)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                expr_has_option_unwrap(program, iterable, Some(expected))
                    || body_has_option_primitive_unwrap(program, body, expected)
            }
            ResolvedStmtKind::Math(expressions) => expressions
                .iter()
                .any(|expr| expr_has_option_unwrap(program, expr, Some(expected))),
            ResolvedStmtKind::Pinned { value, body, .. } => {
                expr_has_option_unwrap(program, value, Some(expected))
                    || body_has_option_primitive_unwrap(program, body, expected)
            }
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_) => false,
        })
        || block
            .result
            .as_ref()
            .is_some_and(|expr| expr_has_option_unwrap(program, expr, Some(expected)))
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
            Some(primitive) => body_has_option_primitive_unwrap(program, block, primitive),
            None => body_has_option_unwrap(program, block),
        },
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            expr_has_option_unwrap(program, condition, expected)
                || match expected {
                    Some(primitive) => {
                        body_has_option_primitive_unwrap(program, then_block, primitive)
                            || body_has_option_primitive_unwrap(program, else_block, primitive)
                    }
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
            Some(primitive) => body_has_option_primitive_unwrap(program, &lambda.body, primitive),
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
    contains_copy_option_variant_candidate(program, PrimitiveType::I32)
}

/// MIR-side materialization receipt counterpart for the concrete Copy
/// `Option<i64>` projection island.
pub fn contains_copy_option_i64_variant_candidate(program: &MirProgram) -> bool {
    contains_copy_option_variant_candidate(program, PrimitiveType::I64)
}

/// MIR-side materialization receipt counterpart for the concrete Copy
/// `Option<f64>` projection island.
pub fn contains_copy_option_f64_variant_candidate(program: &MirProgram) -> bool {
    contains_copy_option_variant_candidate(program, PrimitiveType::F64)
}

/// Detect one concrete Copy `Option<primitive>` projection receipt in MIR.
/// The operation is intentionally independent of source names and only
/// accepts the read-only `VariantProject`/`VariantProjectOr` nodes with a
/// matching TypeDesc receipt.
pub fn contains_copy_option_variant_candidate(
    program: &MirProgram,
    expected: PrimitiveType,
) -> bool {
    // Generic instance bodies have their own projection receipt and are
    // admitted by the generic Option projection island.  Do not reclassify
    // their specialized `VariantProject` as a direct concrete Option island;
    // otherwise a generic `unwrap<T>` call would spuriously enter the i32
    // validator and be rejected for containing a generic instance.
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
                    let (base, result) = match &instruction.kind {
                        MirInstructionKind::VariantProject { base, result, .. }
                        | MirInstructionKind::VariantProjectOr { base, result, .. } => {
                            (base, result)
                        }
                        _ => return false,
                    };
                    let Some(base_ty) = function.values.get(base).map(|value| &value.ty) else {
                        return false;
                    };
                    let Some(result_ty) = function.values.get(result).map(|value| &value.ty) else {
                        return false;
                    };
                    is_copy_option_primitive(program, base_ty, expected)
                        && is_primitive(program, result_ty, expected)
                })
            })
        })
}

fn is_primitive(program: &MirProgram, ty: &ResolvedTypeId, expected: PrimitiveType) -> bool {
    program
        .type_catalog()
        .get(ty)
        .is_some_and(|descriptor| descriptor.kind == MirTypeKind::Primitive(expected))
}

fn is_copy_option_primitive(
    program: &MirProgram,
    ty: &ResolvedTypeId,
    expected: PrimitiveType,
) -> bool {
    let Some(descriptor) = program.type_catalog().get(ty) else {
        return false;
    };
    let MirLayout::Option { inner, .. } = &descriptor.layout else {
        return false;
    };
    is_primitive(program, inner, expected)
        && program
            .type_catalog()
            .validate_copy_option_variant(ty, expected)
            .is_ok()
}

/// Validate the complete Copy `Option<i32>` island using only canonical MIR
/// and TypeDesc receipts.  This is intentionally a narrow operation gate over
/// the generic MIR validator: unsupported variant instructions are rejected,
/// while ordinary scalar code remains available to the shared consumers.
pub fn validate_copy_option_i32_variant_island(program: &MirProgram) -> Result<(), Vec<String>> {
    validate_copy_option_variant_island(program, PrimitiveType::I32, COPY_OPTION_I32_VARIANT_ISLAND)
}

/// Validate the complete Copy `Option<i64>` island using the shared MIR-only
/// validator and its versioned route identity.
pub fn validate_copy_option_i64_variant_island(program: &MirProgram) -> Result<(), Vec<String>> {
    validate_copy_option_variant_island(program, PrimitiveType::I64, COPY_OPTION_I64_VARIANT_ISLAND)
}

/// Validate the complete Copy `Option<f64>` island using the shared MIR-only
/// validator and its versioned route identity.
pub fn validate_copy_option_f64_variant_island(program: &MirProgram) -> Result<(), Vec<String>> {
    validate_copy_option_variant_island(program, PrimitiveType::F64, COPY_OPTION_F64_VARIANT_ISLAND)
}

/// Validate one concrete Copy `Option<primitive>` island using only canonical
/// MIR and TypeDesc receipts.  The caller supplies the versioned profile name
/// so diagnostics remain stable for each default-route admission family.
pub fn validate_copy_option_variant_island(
    program: &MirProgram,
    expected: PrimitiveType,
    island: &'static str,
) -> Result<(), Vec<String>> {
    let mut validator = CopyOptionVariantValidator {
        program,
        errors: BTreeSet::new(),
        saw_projection: false,
        expected,
        island,
    };
    validator.validate();
    if validator.errors.is_empty() {
        Ok(())
    } else {
        Err(validator.errors.into_iter().collect())
    }
}

struct CopyOptionVariantValidator<'a> {
    program: &'a MirProgram,
    errors: BTreeSet<String>,
    saw_projection: bool,
    expected: PrimitiveType,
    island: &'static str,
}

impl<'a> CopyOptionVariantValidator<'a> {
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
                "{} does not admit generic MIR instances",
                self.island
            ));
        }
        if !self.program.transitions().is_empty() {
            self.error(format!(
                "{} does not admit FlowTransition contracts",
                self.island
            ));
        }
        for function in self.program.functions().values() {
            self.validate_function(function);
        }
        if !self.saw_projection {
            self.error(format!(
                "{} has no executable Copy Option projection",
                self.island
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
                        if !is_copy_option_primitive(self.program, base_ty, self.expected)
                            || !is_primitive(self.program, result_ty, self.expected)
                        {
                            self.error(format!(
                                "{} is outside the Copy Option<{:?}> projection shape",
                                instruction.id, self.expected
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
                    MirInstructionKind::VariantProjectOr {
                        base,
                        result,
                        fallback,
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
                        if !is_copy_option_primitive(self.program, base_ty, self.expected)
                            || !is_primitive(self.program, result_ty, self.expected)
                            || !is_primitive(self.program, fallback_ty, self.expected)
                        {
                            self.error(format!(
                                "{} is outside the Copy Option<{:?}> fallback projection shape",
                                instruction.id, self.expected
                            ));
                            continue;
                        }
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
                        instruction.id, self.island
                    )),
                    _ => {}
                }
            }
            if matches!(block.terminator, MirTerminator::SwitchMove { .. }) {
                self.error(format!(
                    "{} consuming variant terminator is outside {}",
                    block.id, self.island
                ));
            }
        }
    }

    fn error(&mut self, message: String) {
        self.errors.insert(message);
    }
}

#[cfg(test)]
mod unwrap_or_tests {
    use super::*;

    #[test]
    fn option_i32_unwrap_or_is_complete_and_records_zero_field_none() {
        let source = include_str!("../../../tests/fixtures/mir_native_option_i32_unwrap_or.mimi");
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_copy_option_i32_variant_admission(&checked),
            CopyOptionVariantAdmission::CompleteCoverage
        );
        let program = MirProgram::from_checked_program(&checked).expect("lower");
        validate_copy_option_i32_variant_island(&program).expect("Option unwrap_or island");
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
            .expect("Option unwrap_or must carry a fallback receipt");
        assert_eq!(receipt.variant_name, "Some");
        assert_eq!(receipt.discriminant, 1);
        assert_eq!(receipt.fallback_variant_name, "None");
        assert_eq!(receipt.fallback_discriminant, 0);
        assert_eq!(receipt.projection.arity, 1);
        assert_eq!(receipt.fallback_arity, 0);
    }
}
