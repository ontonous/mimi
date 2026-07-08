use crate::ast::*;
use crate::span::Span;
use std::collections::HashMap;

/// Extracted contract from an mms block
#[derive(Debug, Clone)]
pub struct Contract {
    pub requires: Vec<String>,
    pub ensures: Vec<String>,
    pub math: Vec<String>,
    pub span: Span,
}

impl Default for Contract {
    fn default() -> Self {
        Self {
            requires: Vec::new(),
            ensures: Vec::new(),
            math: Vec::new(),
            // No source context for Default; callers should set span explicitly
            span: Span::single(0, 0),
        }
    }
}

/// Extract contracts from mms block text content (no span known).
/// Prefer `extract_contracts_with_span` when the source position is available.
pub fn extract_contracts(mms_text: &str) -> Contract {
    Contract {
        requires: mms_text
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("requires:")
                    .map(|s| s.trim().to_string())
            })
            .collect(),
        ensures: mms_text
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("ensures:")
                    .map(|s| s.trim().to_string())
            })
            .collect(),
        math: mms_text
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("math:")
                    .map(|s| s.trim().to_string())
            })
            .collect(),
        // Callers should use extract_contracts_with_span when possible
        span: Span::single(0, 0),
    }
}

pub fn extract_contracts_with_span(mms_text: &str, span: Span) -> Contract {
    Contract {
        requires: mms_text
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("requires:")
                    .map(|s| s.trim().to_string())
            })
            .collect(),
        ensures: mms_text
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("ensures:")
                    .map(|s| s.trim().to_string())
            })
            .collect(),
        math: mms_text
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("math:")
                    .map(|s| s.trim().to_string())
            })
            .collect(),
        span,
    }
}

/// Bind extracted contracts to their corresponding functions in the AST
/// Returns a list of parse errors encountered during contract expression parsing.
pub fn bind_contracts(file: &mut File, contracts: HashMap<String, Contract>) -> Vec<String> {
    let mut errors = Vec::new();
    for item in &mut file.items {
        bind_item_contracts(item, &contracts, &mut errors);
    }
    errors
}

fn bind_item_contracts(
    item: &mut Item,
    contracts: &HashMap<String, Contract>,
    errors: &mut Vec<String>,
) {
    match item {
        Item::Func(func) => {
            if let Some(contract) = contracts.get(&func.name) {
                let mut prefix = Vec::new();
                for req in &contract.requires {
                    match parse_condition(req) {
                        Ok(expr) => prefix.push(Stmt::Requires(expr, contract.span)),
                        Err(e) => errors.push(format!(
                            "parse error in requires for '{}': {}",
                            func.name, e
                        )),
                    }
                }
                for ens in &contract.ensures {
                    match parse_condition(ens) {
                        Ok(expr) => prefix.push(Stmt::Ensures(expr, contract.span)),
                        Err(e) => errors
                            .push(format!("parse error in ensures for '{}': {}", func.name, e)),
                    }
                }
                if !contract.math.is_empty() {
                    let mut math_exprs = Vec::new();
                    for m in &contract.math {
                        match parse_condition(m) {
                            Ok(expr) => math_exprs.push(expr),
                            Err(e) => errors
                                .push(format!("parse error in math for '{}': {}", func.name, e)),
                        }
                    }
                    if !math_exprs.is_empty() {
                        prefix.push(Stmt::Math(math_exprs));
                    }
                }
                // Prepend contract statements to the function body
                prefix.extend(func.body.clone());
                func.body = prefix;
            }
        }
        Item::Module(m) => {
            for inner in &mut m.items {
                bind_item_contracts(inner, contracts, errors);
            }
        }
        _ => {}
    }
}

/// Simple condition parser for contract expressions
fn parse_condition(text: &str) -> Result<Expr, String> {
    // Try to parse as a simple expression using the Mimi lexer/parser
    let tokens = crate::lexer::Lexer::new(text)
        .tokenize()
        .map_err(|e| format!("lex error: {}", e))?;
    let mut parser = crate::parser::Parser::new(tokens);
    parser
        .parse_expr(0)
        .map_err(|e| format!("parse error: {}", e))
}
