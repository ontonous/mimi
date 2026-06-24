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
        requires: mms_text.lines()
            .filter_map(|line| line.trim().strip_prefix("requires:").map(|s| s.trim().to_string()))
            .collect(),
        ensures: mms_text.lines()
            .filter_map(|line| line.trim().strip_prefix("ensures:").map(|s| s.trim().to_string()))
            .collect(),
        math: mms_text.lines()
            .filter_map(|line| line.trim().strip_prefix("math:").map(|s| s.trim().to_string()))
            .collect(),
        // Callers should use extract_contracts_with_span when possible
        span: Span::single(0, 0),
    }
}

pub fn extract_contracts_with_span(mms_text: &str, span: Span) -> Contract {
    Contract {
        requires: mms_text.lines()
            .filter_map(|line| line.trim().strip_prefix("requires:").map(|s| s.trim().to_string()))
            .collect(),
        ensures: mms_text.lines()
            .filter_map(|line| line.trim().strip_prefix("ensures:").map(|s| s.trim().to_string()))
            .collect(),
        math: mms_text.lines()
            .filter_map(|line| line.trim().strip_prefix("math:").map(|s| s.trim().to_string()))
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

fn bind_item_contracts(item: &mut Item, contracts: &HashMap<String, Contract>, errors: &mut Vec<String>) {
    match item {
        Item::Func(func) => {
            if let Some(contract) = contracts.get(&func.name) {
                let mut prefix = Vec::new();
                for req in &contract.requires {
                    match parse_condition(req) {
                        Ok(expr) => prefix.push(Stmt::Requires(expr, contract.span)),
                        Err(e) => errors.push(format!("parse error in requires for '{}': {}", func.name, e)),
                    }
                }
                for ens in &contract.ensures {
                    match parse_condition(ens) {
                        Ok(expr) => prefix.push(Stmt::Ensures(expr, contract.span)),
                        Err(e) => errors.push(format!("parse error in ensures for '{}': {}", func.name, e)),
                    }
                }
                if !contract.math.is_empty() {
                    let mut math_exprs = Vec::new();
                    for m in &contract.math {
                        match parse_condition(m) {
                            Ok(expr) => math_exprs.push(expr),
                            Err(e) => errors.push(format!("parse error in math for '{}': {}", func.name, e)),
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

/// Map inline `rule "..."` statements to structured requires/ensures contracts.
/// Runs AFTER `bind_contracts` (MimiSpec block contracts) and BEFORE type checking.
///
/// Mapping rules (from AGENTS.mimi.md §10):
/// 1. Direct expression match: `"result > 0"` → ensures: result > 0
/// 2. Colon-separated: `"幂等: result == old"` → ensures: result == old
/// 3. Prefix: `"requires: x > 0"` → requires: x > 0
/// 4. Unmappable: kept as Desc("rule: ...") metadata
pub fn map_rule_contracts(file: &mut File) {
    for item in &mut file.items {
        match item {
            Item::Func(func) => {
                transform_rules_in_block(&mut func.body);
            }
            Item::Module(module) => {
                for inner in &mut module.items {
                    match inner {
                        Item::Func(func) => {
                            transform_rules_in_block(&mut func.body);
                        }
                        Item::Module(_) => {
                            map_rule_contracts_inner(inner);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

fn transform_rules_in_block(stmts: &mut [Stmt]) {
    let mut i = 0;
    while i < stmts.len() {
        // Phase 1: Transform Stmt::Rule (needs separate handling due to borrow rules)
        if let Stmt::Rule(text, span) = &stmts[i] {
            let span = *span;
            let text = text.clone();
            match map_rule_text(&text, span) {
                Some(contract_stmt) => stmts[i] = contract_stmt,
                None => stmts[i] = Stmt::Desc(format!("rule: {}", text), span),
            }
        }

        // Phase 2: Recurse into inner blocks (uses &mut to pass to recursive call)
        match &mut stmts[i] {
            Stmt::Block(block) | Stmt::While { body: block, .. }
            | Stmt::For { body: block, .. }
            | Stmt::Loop(block)
            | Stmt::Arena(block)
            | Stmt::Unsafe(block)
            | Stmt::Parasteps(block)
            | Stmt::OnFailure(block) => {
                transform_rules_in_block(block.as_mut_slice());
            }
            Stmt::WhileLet { body, .. } => {
                transform_rules_in_block(body.as_mut_slice());
            }
            Stmt::Alloc { body, .. } => {
                transform_rules_in_block(body.as_mut_slice());
            }
            Stmt::If { then_, else_, .. } => {
                transform_rules_in_block(then_.as_mut_slice());
                if let Some(else_) = else_ {
                    transform_rules_in_block(else_.as_mut_slice());
                }
            }
            _ => {}
        }
        i += 1;
    }
}

fn map_rule_text(text: &str, span: Span) -> Option<Stmt> {
    // Rule 3: requires:/ensures: prefix
    if let Some(rest) = text.strip_prefix("requires:") {
        let expr_str = rest.trim();
        if !expr_str.is_empty() {
            return parse_condition(expr_str).ok().map(|expr| Stmt::Requires(expr, span));
        }
    }
    if let Some(rest) = text.strip_prefix("ensures:") {
        let expr_str = rest.trim();
        if !expr_str.is_empty() {
            return parse_condition(expr_str).ok().map(|expr| Stmt::Ensures(expr, span));
        }
    }

    // Rule 1: Try whole text as expression (default → ensures).
    // Must consume ALL tokens — partial matches (e.g. "this" from "this is natural language") are rejected.
    if let Ok((expr, consumed_all)) = parse_condition_full(text) {
        if consumed_all {
            return Some(Stmt::Ensures(expr, span));
        }
    }

    // Rule 2: Colon separator (flexible whitespace) → second part as ensures expression
    if let Some(colon_idx) = text.find(':') {
        let expr_str = text[colon_idx + 1..].trim();
        if !expr_str.is_empty() {
            if let Ok((expr, true)) = parse_condition_full(expr_str) {
                return Some(Stmt::Ensures(expr, span));
            }
        }
    }

    // Rule 4: Unmappable
    None
}

/// Recursive helper for map_rule_contracts to handle nested modules.
fn map_rule_contracts_inner(item: &mut Item) {
    if let Item::Module(module) = item {
        for inner in &mut module.items {
            match inner {
                Item::Func(func) => {
                    transform_rules_in_block(&mut func.body);
                }
                Item::Module(_) => {
                    map_rule_contracts_inner(inner);
                }
                _ => {}
            }
        }
    }
}

/// Like parse_condition, but also indicates whether ALL tokens were consumed.
fn parse_condition_full(text: &str) -> Result<(Expr, bool), String> {
    let tokens = crate::lexer::Lexer::new(text)
        .tokenize()
        .map_err(|e| format!("lex error: {}", e))?;
    let total = tokens.len();
    let mut parser = crate::parser::Parser::new(tokens);
    let expr = parser.parse_expr(0).map_err(|e| format!("parse error: {}", e))?;
    // tokens include EOF; parser stops before EOF
    let consumed_all = total > 0 && parser.pos() >= total - 1;
    Ok((expr, consumed_all))
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
