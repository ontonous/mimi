use crate::ast::*;
use crate::diagnostic::Diagnostic;

pub(crate) use crate::span::Span;
pub(crate) use std::collections::HashMap;

mod borrow;
mod checker;
mod helpers;

mod check_stmt;
mod infer;
mod infer_expr;

pub(crate) use checker::Checker;
pub use helpers::{fmt_type, is_type_param, subst_type_params};
pub(crate) use helpers::{is_bool, is_numeric_coercion, same_type, is_trait_coercion};
#[cfg(test)]
pub(crate) use helpers::{is_int, is_numeric, is_string};

pub fn check(file: &File) -> Result<(), Vec<Diagnostic>> {
    let mut checker = Checker::new(file);
    checker.check()
}

pub fn check_strict(file: &File) -> Result<(), Vec<Diagnostic>> {
    let mut checker = Checker::new(file);
    checker.strict = true;
    checker.check()
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
            Stmt::Desc(text, _) if text.starts_with("rule:") => {
                // Rule must be followed by requires/ensures or a block that contains them.
                // For now, flag any consecutive rules without intervening contract.
                if last_was_rule {
                    errors.push(format!(
                        "consecutive rules without attached contract in '{}': '{}'",
                        context, text
                    ));
                }
                last_was_rule = true;
                rule_pos = text.clone();
            }
            Stmt::Requires(_, _) | Stmt::Ensures(_, _) => {
                last_was_rule = false;
            }
            Stmt::Block(inner) => {
                verify_rules_in_block(inner, errors, context);
                // A block after a rule potentially contains the contract
                if last_was_rule {
                    last_was_rule = false;
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } => {
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
