//! Whole-program contract for the first non-Copy variant production island.
//!
//! This module deliberately admits only the concrete `Option<string>` shape
//! already implemented by the four MIR consumers. Result, nested payloads,
//! user variants, borrowing, effects, and containers stay outside this island
//! and fail closed.

use std::collections::BTreeSet;

use crate::core::ir::{
    ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedLiteral, ResolvedPattern,
    ResolvedPatternKind, ResolvedStmtKind, ResolvedType,
};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{MirAbiClass, MirGlueKind, MirLayout, MirOwnership, MirTypeKind};
use crate::core::{CheckedProgram, NodeId, PrimitiveType, ResolvedTypeId};

use super::{MirFunction, MirInstructionKind, MirSwitchCase, MirTerminator};

/// Versioned whole-program production island shared by route admission and
/// all default consumers.
pub const NON_COPY_OPTION_STRING_VARIANT_ISLAND: &str = "non-copy-option-string-variant-v1";

/// Checker-owned admission state. Only `CompleteCoverage` may cross into a
/// successful canonical route; a recognized mixed candidate remains a hard
/// boundary once materialization produces the candidate operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionStringVariantAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Classify the Option<string> island before canonical MIR construction.
/// This scan uses only resolved checker facts and never infers eligibility
/// from whether lowering happens to succeed.
pub fn classify_option_string_variant_admission(
    program: &CheckedProgram,
) -> OptionStringVariantAdmission {
    let mut candidate = false;
    let mut mixed = super::islands::has_mixed_coverage(program);

    for callable in program.callables().values() {
        if super::islands::is_prelude_origin(program, &callable.body.root.origin) {
            continue;
        }
        if !callable.signature.generic_parameters.is_empty() {
            continue;
        }
        let body_is_closed = option_body_is_closed(&callable.body.root);
        if body_is_closed {
            candidate |= callable_has_option_string_switch(program, callable);
        }
        if !body_is_closed
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
        OptionStringVariantAdmission::OutsideProfile
    } else if mixed {
        OptionStringVariantAdmission::MixedCoverage
    } else {
        OptionStringVariantAdmission::CompleteCoverage
    }
}

fn callable_has_option_string_switch(
    program: &CheckedProgram,
    callable: &crate::core::ResolvedCallable,
) -> bool {
    block_has_option_string_switch(program, &callable.body.root)
}

fn is_option_string_type_in_checked(program: &CheckedProgram, ty: &ResolvedTypeId) -> bool {
    matches!(
        program.resolved_types().get(ty),
        Some(ResolvedType::Option(inner))
            if matches!(
                program.resolved_types().get(inner),
                Some(ResolvedType::Primitive(PrimitiveType::String))
            )
    )
}

fn block_has_option_string_switch(
    program: &CheckedProgram,
    block: &crate::core::ir::ResolvedBlock,
) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|expr| expr_has_option_string_switch(program, expr)),
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => expr_has_option_string_switch(program, value),
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => value
                .as_ref()
                .is_some_and(|expr| expr_has_option_string_switch(program, expr)),
            ResolvedStmtKind::While { condition, body } => {
                expr_has_option_string_switch(program, condition)
                    || block_has_option_string_switch(program, body)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => {
                expr_has_option_string_switch(program, initializer)
                    || block_has_option_string_switch(program, body)
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_has_option_string_switch(program, initializer)
                    || block_has_option_string_switch(program, then_block)
                    || else_block
                        .as_ref()
                        .is_some_and(|block| block_has_option_string_switch(program, block))
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                block_has_option_string_switch(program, body)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                expr_has_option_string_switch(program, iterable)
                    || block_has_option_string_switch(program, body)
            }
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_) => false,
            ResolvedStmtKind::Math(expressions) => expressions
                .iter()
                .any(|expr| expr_has_option_string_switch(program, expr)),
            ResolvedStmtKind::Pinned { value, body, .. } => {
                expr_has_option_string_switch(program, value)
                    || block_has_option_string_switch(program, body)
            }
        })
        || block
            .result
            .as_ref()
            .is_some_and(|expr| expr_has_option_string_switch(program, expr))
}

fn expr_has_option_string_switch(program: &CheckedProgram, expression: &ResolvedExpr) -> bool {
    match &expression.kind {
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => block_has_option_string_switch(program, block),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            expr_has_option_string_switch(program, condition)
                || block_has_option_string_switch(program, then_block)
                || block_has_option_string_switch(program, else_block)
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            (is_option_string_type_in_checked(program, &scrutinee.ty)
                && matches!(scrutinee.kind, ResolvedExprKind::Load(_)))
                || expr_has_option_string_switch(program, scrutinee)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|expr| expr_has_option_string_switch(program, expr))
                        || expr_has_option_string_switch(program, &arm.body)
                })
        }
        ResolvedExprKind::Call(call) => call
            .arguments
            .iter()
            .any(|argument| expr_has_option_string_switch(program, &argument.value)),
        ResolvedExprKind::Project { value, .. }
        | ResolvedExprKind::Unary { operand: value, .. }
        | ResolvedExprKind::Cast { value, .. }
        | ResolvedExprKind::Old(value) => expr_has_option_string_switch(program, value),
        ResolvedExprKind::Binary { left, right, .. } => {
            expr_has_option_string_switch(program, left)
                || expr_has_option_string_switch(program, right)
        }
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => values
            .iter()
            .any(|expr| expr_has_option_string_switch(program, expr)),
        ResolvedExprKind::Map(entries) => entries.iter().any(|(key, value)| {
            expr_has_option_string_switch(program, key)
                || expr_has_option_string_switch(program, value)
        }),
        ResolvedExprKind::Record { fields, rest, .. } => {
            rest.as_ref()
                .is_some_and(|value| expr_has_option_string_switch(program, value))
                || fields
                    .iter()
                    .any(|field| expr_has_option_string_switch(program, &field.value))
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            expr_has_option_string_switch(program, value)
                || expr_has_option_string_switch(program, iterable)
                || guard
                    .as_ref()
                    .is_some_and(|expr| expr_has_option_string_switch(program, expr))
        }
        ResolvedExprKind::OptionalChain { receiver, .. }
        | ResolvedExprKind::TypeOf(receiver)
        | ResolvedExprKind::Spawn(receiver)
        | ResolvedExprKind::Await(receiver) => expr_has_option_string_switch(program, receiver),
        ResolvedExprKind::Try { value, .. } => expr_has_option_string_switch(program, value),
        ResolvedExprKind::FString(parts) => parts.iter().any(|part| match part {
            crate::core::ir::ResolvedFStringPart::Text(_) => false,
            crate::core::ir::ResolvedFStringPart::Interpolation(value) => {
                expr_has_option_string_switch(program, value)
            }
        }),
        ResolvedExprKind::Range { start, end } => {
            expr_has_option_string_switch(program, start)
                || expr_has_option_string_switch(program, end)
        }
        ResolvedExprKind::Slice { target, start, end } => {
            expr_has_option_string_switch(program, target)
                || start
                    .as_ref()
                    .is_some_and(|expr| expr_has_option_string_switch(program, expr))
                || end
                    .as_ref()
                    .is_some_and(|expr| expr_has_option_string_switch(program, expr))
        }
        ResolvedExprKind::Lambda(lambda) => block_has_option_string_switch(program, &lambda.body),
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => false,
    }
}

fn option_body_is_closed(block: &crate::core::ir::ResolvedBlock) -> bool {
    block.statements.iter().all(|statement| {
        if !statement.backend_requirements.is_empty() {
            return false;
        }
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer,
            } => {
                pattern_is_closed(pattern) && initializer.as_ref().is_none_or(option_expr_is_closed)
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                value.as_ref().is_none_or(option_expr_is_closed)
            }
            ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => option_expr_is_closed(value),
            ResolvedStmtKind::Drop(_) => true,
            _ => false,
        }
    }) && block
        .result
        .as_ref()
        .is_none_or(|expr| option_expr_is_closed(expr))
}

fn pattern_is_closed(pattern: &ResolvedPattern) -> bool {
    match &pattern.kind {
        ResolvedPatternKind::Wildcard
        | ResolvedPatternKind::Binding {
            by_reference: None, ..
        }
        | ResolvedPatternKind::Literal(_) => true,
        ResolvedPatternKind::Constructor { fields, .. } => {
            fields.iter().all(|(_, field)| pattern_is_closed(field))
        }
        _ => false,
    }
}

fn option_expr_is_closed(expression: &ResolvedExpr) -> bool {
    if !expression.effects.is_empty() || !expression.backend_requirements.is_empty() {
        return false;
    }
    match &expression.kind {
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_) => true,
        ResolvedExprKind::Call(call) => {
            let callee_allowed = match &call.callee {
                ResolvedCallee::Builtin(name) => matches!(name.as_str(), "Some" | "None"),
                ResolvedCallee::Function(_) => {
                    call.type_arguments.is_empty()
                        && call.permission.is_none()
                        && call.effects.is_empty()
                        && call.session.is_empty()
                }
                _ => false,
            };
            callee_allowed
                && call
                    .arguments
                    .iter()
                    .all(|argument| option_expr_is_closed(&argument.value))
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            option_expr_is_closed(left) && option_expr_is_closed(right)
        }
        ResolvedExprKind::Unary { operand, op } => {
            !matches!(
                op,
                crate::core::ir::ResolvedUnaryOp::BorrowShared
                    | crate::core::ir::ResolvedUnaryOp::BorrowMutable
                    | crate::core::ir::ResolvedUnaryOp::Dereference
            ) && option_expr_is_closed(operand)
        }
        ResolvedExprKind::Cast { value, .. } | ResolvedExprKind::Old(value) => {
            option_expr_is_closed(value)
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            option_body_is_closed(block)
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            option_expr_is_closed(scrutinee)
                && arms.iter().all(|arm| {
                    arm.guard.is_none()
                        && pattern_is_closed(&arm.pattern)
                        && option_expr_is_closed(&arm.body)
                })
        }
        _ => false,
    }
}

/// Materialization receipt counterpart of checker admission.
pub fn contains_option_string_variant_candidate(program: &MirProgram) -> bool {
    program.functions().values().any(|function| {
        function
            .values
            .values()
            .any(|value| is_option_string_type(program, &value.ty))
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ConstructVariantMove { .. }
                    )
                }) || matches!(block.terminator, MirTerminator::SwitchMove { .. })
            })
    })
}

fn is_option_string_type(program: &MirProgram, ty: &ResolvedTypeId) -> bool {
    let Some(descriptor) = program.type_catalog().get(ty) else {
        return false;
    };
    let MirLayout::Option { inner, .. } = &descriptor.layout else {
        return false;
    };
    matches!(
        program
            .type_catalog()
            .get(inner)
            .map(|descriptor| &descriptor.kind),
        Some(MirTypeKind::Primitive(PrimitiveType::String))
    ) && program
        .type_catalog()
        .validate_option_string_variant(ty)
        .is_ok()
}

/// Validate the complete non-Copy Option<string> production island using
/// only canonical MIR and TypeDesc facts. This gate runs before verifier,
/// bytecode, and native capability checks and rejects every other shape.
pub fn validate_option_string_variant_island(program: &MirProgram) -> Result<(), Vec<String>> {
    let mut validator = OptionStringVariantValidator {
        program,
        errors: BTreeSet::new(),
        checked_types: BTreeSet::new(),
        saw_option: false,
    };
    validator.validate();
    if validator.errors.is_empty() {
        Ok(())
    } else {
        Err(validator.errors.into_iter().collect())
    }
}

struct OptionStringVariantValidator<'a> {
    program: &'a MirProgram,
    errors: BTreeSet<String>,
    checked_types: BTreeSet<ResolvedTypeId>,
    saw_option: bool,
}

impl<'a> OptionStringVariantValidator<'a> {
    fn validate(&mut self) {
        let main = NodeId("function:main".into());
        if !self.program.functions().contains_key(&main) {
            self.error("program has no canonical function:main".into());
        }
        if !self.program.instances().is_empty() {
            self.error(format!(
                "{NON_COPY_OPTION_STRING_VARIANT_ISLAND} does not admit generic MIR instances"
            ));
        }
        if !self.program.transitions().is_empty() {
            self.error(format!(
                "{NON_COPY_OPTION_STRING_VARIANT_ISLAND} does not admit FlowTransition contracts"
            ));
        }
        for function in self.program.functions().values() {
            self.validate_function(function);
        }
        if !self.saw_option {
            self.error(format!(
                "{NON_COPY_OPTION_STRING_VARIANT_ISLAND} has no executable Option<string> value"
            ));
        }
    }

    fn validate_function(&mut self, function: &MirFunction) {
        self.validate_type(
            &function.result,
            &format!("function '{}' result", function.owner.0),
        );
        for value in function.values.values() {
            self.validate_type(&value.ty, &format!("value '{}'", value.id));
        }
        if function
            .contracts
            .iter()
            .any(|contract| contract.kind == crate::core::mir::MirContractKind::Invariant)
        {
            self.error(format!(
                "function '{}' invariant contract is outside {}",
                function.owner.0, NON_COPY_OPTION_STRING_VARIANT_ISLAND
            ));
        }
        for event in &function.ownership.events {
            if !matches!(
                event.kind,
                crate::core::mir::MirOwnershipEventKind::Read
                    | crate::core::mir::MirOwnershipEventKind::Introduce
                    | crate::core::mir::MirOwnershipEventKind::Move
                    | crate::core::mir::MirOwnershipEventKind::Drop
                    | crate::core::mir::MirOwnershipEventKind::Return
            ) {
                self.error(format!(
                    "function '{}' ownership event '{}' is outside {}",
                    function.owner.0,
                    event.kind.as_str(),
                    NON_COPY_OPTION_STRING_VARIANT_ISLAND
                ));
            }
        }
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                self.validate_instruction(function, &instruction.kind, instruction.id.as_str());
            }
            self.validate_terminator(function, &block.terminator, block.id.as_str());
        }
    }

    fn validate_type(&mut self, ty: &ResolvedTypeId, subject: &str) {
        if !self.checked_types.insert(ty.clone()) {
            return;
        }
        let Some(descriptor) = self.program.type_catalog().get(ty) else {
            self.error(format!("{subject} TypeDesc '{}' is absent", ty.as_str()));
            return;
        };
        match &descriptor.kind {
            MirTypeKind::Primitive(
                PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::Bool,
            ) => {
                if let Err(message) = self.program.type_catalog().validate_copy_scalar(ty) {
                    self.error(format!(
                        "{subject} type '{}' rejected: {message}",
                        ty.as_str()
                    ));
                }
            }
            MirTypeKind::Primitive(PrimitiveType::String) => {
                if let Err(message) = self.program.type_catalog().validate_owned_string(ty) {
                    self.error(format!(
                        "{subject} type '{}' rejected: {message}",
                        ty.as_str()
                    ));
                }
            }
            MirTypeKind::Primitive(PrimitiveType::Unit) => {
                if descriptor.layout != MirLayout::Unit
                    || descriptor.ownership != MirOwnership::Copy
                    || !is_noop_glue(descriptor.glue)
                {
                    self.error(format!("{subject} unit TypeDesc is inconsistent"));
                }
            }
            MirTypeKind::Option => {
                self.saw_option = true;
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_option_string_variant(ty)
                {
                    self.error(format!("{subject} Option<string> rejected: {message}"));
                }
            }
            _ => self.error(format!(
                "{subject} type '{}' is outside {}",
                ty.as_str(),
                NON_COPY_OPTION_STRING_VARIANT_ISLAND
            )),
        }
    }

    fn validate_value(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
    ) -> Option<ResolvedTypeId> {
        let Some(value) = function.values.get(value) else {
            self.error(format!("{subject} value is absent"));
            return None;
        };
        self.validate_type(&value.ty, subject);
        Some(value.ty.clone())
    }

    fn validate_instruction(
        &mut self,
        function: &MirFunction,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        match instruction {
            MirInstructionKind::Const { result, literal } => {
                let Some(ty) = self.validate_value(function, result, "constant result") else {
                    return;
                };
                self.validate_literal(&ty, literal, subject);
            }
            MirInstructionKind::Load { result, .. } => {
                self.validate_value(function, result, "load result");
                self.error(format!(
                    "{subject} Load is outside the closed Option<string> lowering"
                ));
            }
            MirInstructionKind::Copy { result, source }
            | MirInstructionKind::Move { result, source }
            | MirInstructionKind::Clone { result, source } => {
                let Some(result_ty) = self.validate_value(function, result, "copy result") else {
                    return;
                };
                let Some(source_ty) = self.validate_value(function, source, "copy source") else {
                    return;
                };
                if result_ty != source_ty {
                    self.error(format!("{subject} result/source types disagree"));
                }
                if matches!(instruction, MirInstructionKind::Copy { .. })
                    && is_move_owned_type(self.program, &source_ty)
                {
                    self.error(format!(
                        "{subject} shallow Copy of move-owned value is forbidden"
                    ));
                }
            }
            MirInstructionKind::Drop { value } => {
                self.validate_value(function, value, "drop value");
            }
            MirInstructionKind::ConstructVariantMove {
                result,
                nominal,
                variant,
                fields,
            } => {
                let Some(result_ty) = self.validate_value(function, result, "variant result")
                else {
                    return;
                };
                let field_types = fields
                    .iter()
                    .filter_map(|(_, value)| self.validate_value(function, value, "variant field"))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    return;
                }
                let field_ids = fields
                    .iter()
                    .map(|(field, _)| field.clone())
                    .collect::<Vec<_>>();
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_option_string_variant(&result_ty)
                    .and_then(|_| {
                        self.program.type_catalog().validate_variant_construct(
                            &result_ty,
                            nominal,
                            variant,
                            &field_ids,
                            &field_types,
                        )
                    })
                {
                    self.error(format!(
                        "{subject} variant construction rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::Binary {
                result,
                left,
                right,
                ..
            } => {
                self.validate_scalar_value(function, result, subject);
                self.validate_scalar_value(function, left, subject);
                self.validate_scalar_value(function, right, subject);
            }
            MirInstructionKind::Unary {
                result,
                operand,
                op,
            } => {
                if matches!(
                    op,
                    crate::core::ir::ResolvedUnaryOp::BorrowShared
                        | crate::core::ir::ResolvedUnaryOp::BorrowMutable
                        | crate::core::ir::ResolvedUnaryOp::Dereference
                ) {
                    self.error(format!(
                        "{subject} borrow/dereference is outside the island"
                    ));
                }
                self.validate_scalar_value(function, result, subject);
                self.validate_scalar_value(function, operand, subject);
            }
            MirInstructionKind::Call {
                result,
                callee,
                type_arguments,
                arguments,
                ..
            } => {
                let ResolvedCallee::Function(owner) = callee else {
                    self.error(format!(
                        "{subject} callee is outside the closed function-call contract"
                    ));
                    return;
                };
                if !type_arguments.is_empty() {
                    self.error(format!("{subject} generic call is outside the island"));
                }
                let Some(target) = self.program.functions().get(owner) else {
                    self.error(format!("{subject} callee '{}' is absent", owner.0));
                    return;
                };
                if arguments.len() != target.parameters.len() {
                    self.error(format!("{subject} call arity disagrees with callee"));
                }
                for argument in arguments {
                    self.validate_value(function, argument, "call argument");
                }
                if let Some(result) = result {
                    self.validate_value(function, result, "call result");
                }
            }
            MirInstructionKind::Convert { result, source } => {
                let Some(source_ty) = self.validate_value(function, source, "conversion source")
                else {
                    return;
                };
                let Some(result_ty) = self.validate_value(function, result, "conversion result")
                else {
                    return;
                };
                match self
                    .program
                    .type_catalog()
                    .validate_conversion(&source_ty, &result_ty)
                {
                    Ok(contract)
                        if matches!(
                            contract.kind,
                            crate::core::mir::types::MirConversionKind::ScalarIdentity
                                | crate::core::mir::types::MirConversionKind::SignedI32ToI64
                        ) => {}
                    Ok(_) | Err(_) => {
                        self.error(format!("{subject} conversion is outside the island"))
                    }
                }
            }
            MirInstructionKind::Nop => {}
            MirInstructionKind::Borrow { .. }
            | MirInstructionKind::EndBorrow { .. }
            | MirInstructionKind::Project { .. }
            | MirInstructionKind::MoveProject { .. }
            | MirInstructionKind::MoveProjectDrop { .. }
            | MirInstructionKind::VariantProject { .. }
            | MirInstructionKind::VariantProjectMove { .. }
            | MirInstructionKind::Construct { .. }
            | MirInstructionKind::ConstructList { .. }
            | MirInstructionKind::ListOp { .. }
            | MirInstructionKind::ConstructSet { .. }
            | MirInstructionKind::SetOp { .. }
            | MirInstructionKind::ConstructVariant { .. }
            | MirInstructionKind::VariantPredicate { .. }
            | MirInstructionKind::UpdateRecord { .. }
            | MirInstructionKind::BuiltinCall { .. }
            | MirInstructionKind::FlowTransition { .. } => self.error(format!(
                "{subject} instruction is outside {NON_COPY_OPTION_STRING_VARIANT_ISLAND}"
            )),
        }
    }

    fn validate_scalar_value(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
    ) {
        let Some(ty) = self.validate_value(function, value, subject) else {
            return;
        };
        if !is_scalar_type(self.program, &ty) {
            self.error(format!(
                "{subject} value type '{}' is not scalar",
                ty.as_str()
            ));
        }
    }

    fn validate_literal(&mut self, ty: &ResolvedTypeId, literal: &ResolvedLiteral, subject: &str) {
        let Some(descriptor) = self.program.type_catalog().get(ty) else {
            return;
        };
        let valid = match (&descriptor.abi, literal) {
            (
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true,
                },
                ResolvedLiteral::Int(value),
            ) => {
                descriptor.abi
                    == MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    }
                    || i32::try_from(*value).is_ok()
            }
            (MirAbiClass::Bool, ResolvedLiteral::Bool(_))
            | (MirAbiClass::StringHandle, ResolvedLiteral::String(_))
            | (MirAbiClass::Unit, ResolvedLiteral::Unit) => true,
            _ => false,
        };
        if !valid {
            self.error(format!("{subject} literal does not match TypeDesc ABI"));
        }
    }

    fn validate_terminator(
        &mut self,
        function: &MirFunction,
        terminator: &MirTerminator,
        subject: &str,
    ) {
        match terminator {
            MirTerminator::Goto { .. } => {}
            MirTerminator::Branch { condition, .. } => {
                self.validate_scalar_value(function, condition, subject);
            }
            MirTerminator::Return { value } => {
                if let Some(value) = value {
                    self.validate_value(function, value, "return value");
                }
            }
            MirTerminator::Trap { code } => {
                if let Err(message) = crate::core::mir::types::validate_trap_code(code) {
                    self.error(format!("{subject} trap rejected: {message}"));
                }
            }
            MirTerminator::SwitchMove { scrutinee, arms } => {
                let Some(scrutinee_ty) =
                    self.validate_value(function, scrutinee, "switch scrutinee")
                else {
                    return;
                };
                if !is_option_string_type(self.program, &scrutinee_ty) {
                    self.error(format!("{subject} SwitchMove is not Option<string>"));
                    return;
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_variant_switch_move_contract(&scrutinee_ty, arms)
                {
                    self.error(format!("{subject} SwitchMove rejected: {message}"));
                }
                let variants = arms
                    .iter()
                    .filter_map(|arm| match &arm.case {
                        MirSwitchCase::Variant(variant) => Some(variant.clone()),
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>();
                let expected = BTreeSet::from([
                    NodeId("builtin:variant:Option::None".into()),
                    NodeId("builtin:variant:Option::Some".into()),
                ]);
                if variants != expected || arms.len() != expected.len() {
                    self.error(format!(
                        "{subject} SwitchMove must cover exactly None and Some"
                    ));
                }
            }
            MirTerminator::Switch { .. }
            | MirTerminator::Fault { .. }
            | MirTerminator::Unreachable => self.error(format!(
                "{subject} terminator is outside {NON_COPY_OPTION_STRING_VARIANT_ISLAND}"
            )),
        }
    }

    fn error(&mut self, message: String) {
        self.errors.insert(message);
    }
}

fn is_scalar_type(program: &MirProgram, ty: &ResolvedTypeId) -> bool {
    program.type_catalog().get(ty).is_some_and(|descriptor| {
        matches!(
            descriptor.kind,
            MirTypeKind::Primitive(
                PrimitiveType::I32
                    | PrimitiveType::I64
                    | PrimitiveType::Bool
                    | PrimitiveType::String
                    | PrimitiveType::Unit
            )
        )
    })
}

fn is_move_owned_type(program: &MirProgram, ty: &ResolvedTypeId) -> bool {
    program
        .type_catalog()
        .get(ty)
        .is_some_and(|descriptor| descriptor.ownership == MirOwnership::Move)
}

fn is_noop_glue(glue: crate::core::mir::types::MirGlueContract) -> bool {
    glue.move_out == MirGlueKind::Noop
        && glue.clone == MirGlueKind::Noop
        && glue.drop == MirGlueKind::Noop
}
