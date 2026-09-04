//! Whole-program contract for the concrete Copy `Result<i32, i32>` projection.
//!
//! This is deliberately a separate route profile from the Copy `Option` islands:
//! the MIR node is shared, but checker admission, TypeDesc proof and stable
//! diagnostics must not let an Option receipt accidentally qualify a Result.

use std::collections::BTreeSet;

use crate::core::ir::{ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedStmtKind};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{MirGlueKind, MirOwnership, MirTypeKind};
use crate::core::{CheckedProgram, NodeId, PrimitiveType, ResolvedTypeId};

use super::{MirFunction, MirInstructionKind, MirTerminator};

/// Versioned default-route island for direct Copy `Result<i32, i32>.unwrap()`.
pub const COPY_RESULT_I32_VARIANT_ISLAND: &str = "copy-result-i32-variant-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyResultI32VariantAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Classify the concrete checker shape before MIR construction. The profile
/// is closed: every Result unwrap must be the exact `Result<i32, i32>` builtin
/// and every callable must contain only the already-migrated body subset.
pub fn classify_copy_result_i32_variant_admission(
    program: &CheckedProgram,
) -> CopyResultI32VariantAdmission {
    let mut candidate = false;
    let mut mixed = super::islands::has_mixed_coverage(program);
    for callable in program.callables().values() {
        if super::islands::is_prelude_origin(program, &callable.body.root.origin) {
            continue;
        }
        let body_is_closed = super::option_island::option_body_is_closed(&callable.body.root);
        let has_any_unwrap = body_has_result_unwrap(program, &callable.body.root, false);
        let has_expected_unwrap = body_has_result_unwrap(program, &callable.body.root, true);
        if body_is_closed {
            // Any Result::unwrap is a profile candidate. Only the concrete
            // i32/i32 payload is complete; unsupported payloads must become a
            // MixedCoverage hard rejection rather than silently entering legacy.
            candidate |= has_any_unwrap;
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
    matches!(&call.callee, ResolvedCallee::Builtin(name)
        if name.as_str() == "builtin.method.result.unwrap")
        && call.arguments.len() == 1
        && (!expected || is_result_i32_i32(program, &call.arguments[0].value.ty))
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

/// Detect one concrete Copy Result projection receipt in MIR.
pub fn contains_copy_result_i32_variant_candidate(program: &MirProgram) -> bool {
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
                is_copy_result_i32(program, base_ty)
                    && program
                        .type_catalog()
                        .get(result_ty)
                        .is_some_and(|descriptor| {
                            descriptor.kind == MirTypeKind::Primitive(PrimitiveType::I32)
                        })
            })
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
                    MirInstructionKind::VariantProjectMove { .. } => self.error(format!(
                        "{} consuming variant operation is outside {}",
                        instruction.id, COPY_RESULT_I32_VARIANT_ISLAND
                    )),
                    _ => {}
                }
            }
            if matches!(block.terminator, MirTerminator::SwitchMove { .. }) {
                self.error(format!(
                    "{} consuming variant terminator is outside {}",
                    block.id, COPY_RESULT_I32_VARIANT_ISLAND
                ));
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
}
