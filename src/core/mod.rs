use crate::ast::*;
use crate::diagnostic::Diagnostic;

pub(crate) use crate::span::Span;
pub(crate) use std::collections::HashMap;

mod borrow;
mod checker;
pub(crate) mod helpers;
pub mod unification;

mod check_stmt;
mod infer;
mod infer_expr;
mod ownership;
pub mod resolved;

pub(crate) use checker::Checker;
pub use helpers::{fmt_type, is_type_param, subst_type_params};
pub(crate) use helpers::{is_bool, is_numeric_coercion, is_trait_coercion};
#[cfg(test)]
pub(crate) use helpers::{is_int, is_numeric, is_string, same_type};
pub use ownership::{
    BranchMerge, OwnershipLedger, ResourceAction, ResourceActionKind, ResourceState,
};
pub use resolved::{
    BackendProfile, CheckedProgram, FlowId, NodeId, NodeMeta, Origin, ResolvedActor,
    ResolvedActorMethod,
    ResolvedCapability, ResolvedConstant,
    ResolvedCallKind,
    ResolvedCallSite,
    ResolvedConstValue, ResolvedExternBlock,
    ResolvedExternFunc, ResolvedFlow, ResolvedFunction,
    ResolvedImpl, ResolvedItem, ResolvedItemKind, ResolvedProtocol, ResolvedSession, ResolvedState,
    ResolvedTrait, ResolvedTypeDef, ResolvedTypeKind, SpanPrecision, StateId, TransitionId,
    RESOLVED_IR_VERSION,
};

pub fn check(file: &File) -> Result<(), Vec<Diagnostic>> {
    check_program(file).map(|_| ())
}

pub fn check_strict(file: &File) -> Result<(), Vec<Diagnostic>> {
    check_program_strict(file).map(|_| ())
}

pub fn check_program(file: &File) -> Result<CheckedProgram<'_>, Vec<Diagnostic>> {
    let ownership = checker::flow::flow_check_with_artifacts(file)?;
    CheckedProgram::from_checked_file_with_ownership(file, ownership)
}

pub fn check_program_strict(file: &File) -> Result<CheckedProgram<'_>, Vec<Diagnostic>> {
    let ownership = checker::flow::flow_check_strict_with_artifacts(file)?;
    CheckedProgram::from_checked_file_with_ownership(file, ownership)
}

/// Verify that MMS rule attachments are consistent.
/// Rules must be attached to a following entity; orphan rules are errors.
pub fn verify_rules(file: &File) -> Vec<String> {
    let mut errors = Vec::new();
    for item in &file.items {
        match item {
            Item::Func(func) => {
                verify_rules_in_block(&func.body, &mut errors, &func.name);
            }
            Item::Module(module) => {
                for item in &module.items {
                    if let Item::Func(func) = item {
                        verify_rules_in_block(&func.body, &mut errors, &func.name);
                    }
                }
            }
            _ => {}
        }
    }
    errors
}

fn verify_rules_in_block(block: &[Stmt], errors: &mut Vec<String>, context: &str) {
    let mut last_was_rule = false;
    let mut rule_pos = String::new();
    for stmt in block {
        match stmt {
            Stmt::Rule(text, _) => {
                if last_was_rule {
                    errors.push(format!(
                        "consecutive rules without attached contract in '{}': '{}'",
                        context, text
                    ));
                }
                last_was_rule = true;
                rule_pos = text.clone();
            }
            Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::Invariant(_, _) => {
                last_was_rule = false;
            }
            Stmt::Block(inner) => {
                verify_rules_in_block(inner, errors, context);
                // A block after a rule potentially contains the contract
                if last_was_rule {
                    last_was_rule = false;
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::Loop(body) => {
                verify_rules_in_block(body, errors, context);
                last_was_rule = false;
            }
            // Bug-9 fix: WhileLet also has a body that may contain rules
            Stmt::WhileLet { body, .. } => {
                verify_rules_in_block(body, errors, context);
                last_was_rule = false;
            }
            Stmt::If { then_, else_, .. } => {
                verify_rules_in_block(then_, errors, context);
                if let Some(else_) = else_ {
                    verify_rules_in_block(else_, errors, context);
                }
                last_was_rule = false;
            }
            _ => {
                last_was_rule = false;
            }
        }
    }
    if last_was_rule {
        errors.push(format!(
            "orphan rule without attached contract at end of '{}': '{}'",
            context, rule_pos
        ));
    }
}
