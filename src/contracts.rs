use crate::ast::*;
use crate::span::Span;
use std::collections::HashMap;

/// Extracted contract from an mms block
#[derive(Debug, Clone, Default)]
pub struct Contract {
    pub requires: Vec<String>,
    pub ensures: Vec<String>,
    pub math: Vec<String>,
}

/// Extract contracts from mms block text content
pub fn extract_contracts(mms_text: &str) -> Contract {
    let mut contract = Contract::default();
    for line in mms_text.lines() {
        let line = line.trim();
        if let Some(cond) = line.strip_prefix("requires:") {
            contract.requires.push(cond.trim().to_string());
        } else if let Some(cond) = line.strip_prefix("ensures:") {
            contract.ensures.push(cond.trim().to_string());
        } else if let Some(cond) = line.strip_prefix("math:") {
            contract.math.push(cond.trim().to_string());
        }
    }
    contract
}

/// Bind extracted contracts to their corresponding functions in the AST
pub fn bind_contracts(file: &mut File, contracts: HashMap<String, Contract>) {
    for item in &mut file.items {
        bind_item_contracts(item, &contracts);
    }
}

fn bind_item_contracts(item: &mut Item, contracts: &HashMap<String, Contract>) {
    match item {
        Item::Func(func) => {
            if let Some(contract) = contracts.get(&func.name) {
                // Add requires/ensures/math statements to the beginning of the function body
                let mut prefix = Vec::new();
                for req in &contract.requires {
                    // Parse the condition as an expression if possible
                    if let Ok(expr) = parse_condition(req) {
                        prefix.push(Stmt::Requires(expr, Span::single(0, 0)));
                    }
                }
                for ens in &contract.ensures {
                    if let Ok(expr) = parse_condition(ens) {
                        prefix.push(Stmt::Ensures(expr, Span::single(0, 0)));
                    }
                }
                if !contract.math.is_empty() {
                    let math_exprs: Vec<Expr> = contract.math.iter()
                        .filter_map(|m| parse_condition(m).ok())
                        .collect();
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
                bind_item_contracts(inner, contracts);
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
    parser.parse_expr(0).map_err(|e| format!("parse error: {}", e))
}
