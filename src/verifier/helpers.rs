#![allow(dead_code)]

use crate::ast::*;
use crate::verifier::ctx::{VerificationResult, VerifStatus};

/// Extract the final value-producing expression from a block.
/// Used in `expr_to_z3_*` to evaluate the tail expression of an if-else branch.
pub(crate) fn block_tail_expr(block: &[Stmt]) -> Option<Expr> {
    for stmt in block.iter().rev() {
        match stmt {
            Stmt::Expr(e) => return Some(e.clone()),
            Stmt::Return(Some(e)) => return Some(e.clone()),
            Stmt::Return(None) => return Some(Expr::Literal(Lit::Unit)),
            _ => {}
        }
    }
    None
}

/// Check if a comparison is between a string ident and an empty string literal.
pub(crate) fn is_string_empty_cmp(lhs: &Expr, rhs: &Expr, op: &BinOp) -> bool {
    matches!(op, BinOp::EqCmp | BinOp::NeCmp)
        && match (lhs, rhs) {
            (Expr::Ident(_), Expr::Literal(Lit::String(s)))
            | (Expr::Literal(Lit::String(s)), Expr::Ident(_)) => s.is_empty(),
            _ => false,
        }
}

/// Extract the string ident name from a string emptiness comparison.
/// Assumes `is_string_empty_cmp` returned `true`.
pub(crate) fn extract_string_empty_cmp(lhs: &Expr, rhs: &Expr, op: &BinOp) -> (String, BinOp) {
    match (lhs, rhs) {
        (Expr::Ident(name), Expr::Literal(Lit::String(_))) => (name.clone(), *op),
        (Expr::Literal(Lit::String(_)), Expr::Ident(name)) => (name.clone(), *op),
        _ => (String::new(), *op),
    }
}

/// Extract the return/tail expression from a function body, handling if-else branching.
/// Uses `Expr::If` to represent conditional paths so the Z3 layer can encode them via `ite`.
pub(crate) fn extract_body_return(block: &[Stmt]) -> Option<Expr> {
    // First pass: look for explicit returns and if-else expressions
    for stmt in block.iter().rev() {
        match stmt {
            Stmt::Return(Some(expr)) => return Some(expr.clone()),
            Stmt::Return(None) => return Some(Expr::Literal(Lit::Unit)),
            Stmt::If { cond, then_, else_ } => {
                return extract_if_return(cond, then_, else_);
            }
            _ => {}
        }
    }
    // Second pass: look for implicit return (tail expression).
    // Also skip let/assign statements so `let x = ...; expr` patterns are found.
    for stmt in block.iter().rev() {
        match stmt {
            Stmt::Expr(expr) => return Some(expr.clone()),
            Stmt::If { cond, then_, else_ } => {
                return extract_if_return(cond, then_, else_);
            }
            Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::Invariant(_, _) | Stmt::Math(_)
            | Stmt::Desc(..) | Stmt::Rule(..) | Stmt::MmsBlock { .. }
            | Stmt::Let { .. } | Stmt::Assign { .. } => continue,
            _ => break,
        }
    }
    None
}

/// Build an `Expr::If` from the condition and both branches' return expressions.
fn extract_if_return(cond: &Expr, then_: &[Stmt], else_: &Option<Block>) -> Option<Expr> {
    let then_expr = extract_body_return(then_)?;
    let else_expr = else_
        .as_ref()
        .and_then(|b| extract_body_return(b))
        .unwrap_or(Expr::Literal(Lit::Unit));
    Some(Expr::If {
        cond: Box::new(cond.clone()),
        then_: vec![Stmt::Expr(then_expr)],
        else_: Some(vec![Stmt::Expr(else_expr)]),
    })
}

pub(crate) fn format_expr(expr: &Expr) -> String {
    match expr {
        Expr::Literal(Lit::Int(n)) => format!("{}", n),
        Expr::Literal(Lit::Float(f)) => format!("{}", f),
        Expr::Literal(Lit::Bool(b)) => format!("{}", b),
        Expr::Literal(Lit::String(s)) => format!("\"{}\"", s),
        Expr::Literal(Lit::Unit) => "()".to_string(),
        Expr::Literal(Lit::FString(parts)) => {
            let inner: String = parts
                .iter()
                .map(|p| match p {
                    FStringPart::Text(t) => t.clone(),
                    FStringPart::Interp(e) => format!("{}", format_expr(e)),
                })
                .collect();
            format!("f\"{}\"", inner)
        }
        Expr::Ident(name) => name.clone(),
        Expr::Old(inner) => format!("old({})", format_expr(inner)),
        Expr::Binary(op, l, r) => {
            let op_str = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Mod => "%",
                BinOp::EqCmp => "==",
                BinOp::NeCmp => "!=",
                BinOp::Lt => "<",
                BinOp::Gt => ">",
                BinOp::Le => "<=",
                BinOp::Ge => ">=",
                BinOp::And => "&&",
                BinOp::Or => "||",
                _ => "?",
            };
            format!("{} {} {}", format_expr(l), op_str, format_expr(r))
        }
        Expr::Unary(UnOp::Neg, inner) => format!("-{}", format_expr(inner)),
        Expr::Unary(UnOp::Not, inner) => format!("!{}", format_expr(inner)),
        Expr::Block(block) => {
            let s: Vec<String> = block.iter().map(|s| format_stmt(s)).collect();
            format!("{{ {} }}", s.join("; "))
        }
        _ => "<expr>".to_string(),
    }
}

fn format_stmt(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Let { pat, .. } => format!("let {:?}", pat),
        Stmt::Expr(expr) => format_expr(expr),
        Stmt::Return(Some(expr)) => format!("return {}", format_expr(expr)),
        Stmt::Return(None) => "return".to_string(),
        Stmt::If { cond, .. } => format!("if {}", format_expr(cond)),
        Stmt::While { cond, .. } => format!("while {}", format_expr(cond)),
        Stmt::Requires(e, _) => format!("requires {}", format_expr(e)),
        Stmt::Ensures(e, _) => format!("ensures {}", format_expr(e)),
        Stmt::Invariant(e, _) => format!("invariant {}", format_expr(e)),
        _ => "<stmt>".to_string(),
    }
}

pub(crate) fn collect_idents_in_expr(expr: &Expr, idents: &mut Vec<String>) {
    match expr {
        Expr::Ident(name) => {
            if !idents.contains(name) {
                idents.push(name.clone());
            }
        }
        Expr::Binary(_, lhs, rhs) => {
            collect_idents_in_expr(lhs, idents);
            collect_idents_in_expr(rhs, idents);
        }
        Expr::Unary(_, inner) => collect_idents_in_expr(inner, idents),
        Expr::Old(inner) => collect_idents_in_expr(inner, idents),
        Expr::Call(callee, args) => {
            collect_idents_in_expr(callee, idents);
            for arg in args {
                collect_idents_in_expr(arg, idents);
            }
        }
        Expr::Field(obj, _) => collect_idents_in_expr(obj, idents),
        Expr::Index(obj, idx) => {
            collect_idents_in_expr(obj, idents);
            collect_idents_in_expr(idx, idents);
        }
        Expr::Tuple(elems) => {
            for e in elems {
                collect_idents_in_expr(e, idents);
            }
        }
        Expr::List(elems) => {
            for e in elems {
                collect_idents_in_expr(e, idents);
            }
        }
        Expr::Record { fields, .. } => {
            for f in fields {
                collect_idents_in_expr(&f.value, idents);
            }
        }
        Expr::Block(block) => {
            for s in block {
                collect_idents_in_stmt(s, idents);
            }
        }
        Expr::If { cond, then_, else_ } => {
            collect_idents_in_expr(cond, idents);
            for s in then_ {
                collect_idents_in_stmt(s, idents);
            }
            if let Some(e) = else_ {
                for s in e {
                    collect_idents_in_stmt(s, idents);
                }
            }
        }
        Expr::Match(scrutinee, arms) => {
            collect_idents_in_expr(scrutinee, idents);
            for arm in arms {
                collect_idents_in_expr(&arm.body, idents);
            }
        }
        Expr::Lambda { body, .. } => {
            for s in body {
                collect_idents_in_stmt(s, idents);
            }
        }
        Expr::Comprehension { expr, iter, guard, .. } => {
            collect_idents_in_expr(expr, idents);
            collect_idents_in_expr(iter, idents);
            if let Some(g) = guard {
                collect_idents_in_expr(g, idents);
            }
        }
        Expr::Range { start, end } => {
            collect_idents_in_expr(start, idents);
            collect_idents_in_expr(end, idents);
        }
        Expr::SliceExpr { target, start, end } => {
            collect_idents_in_expr(target, idents);
            if let Some(s) = start {
                collect_idents_in_expr(s, idents);
            }
            if let Some(e) = end {
                collect_idents_in_expr(e, idents);
            }
        }
        Expr::Turbofish(_, _, args) => {
            for a in args {
                collect_idents_in_expr(a, idents);
            }
        }
        Expr::Try(inner)
        | Expr::Spawn(inner)
        | Expr::Await(inner)
        | Expr::QuoteInterpolate(inner)
        | Expr::TypeOf(inner) => {
            collect_idents_in_expr(inner, idents);
        }
        Expr::Comptime(body) | Expr::Quote(body) => {
            for s in body {
                collect_idents_in_stmt(s, idents);
            }
        }
        Expr::TupleIndex(obj, _) => collect_idents_in_expr(obj, idents),
        _ => {}
    }
}

pub(crate) fn collect_idents_in_stmt(stmt: &Stmt, idents: &mut Vec<String>) {
    match stmt {
        Stmt::Expr(e) | Stmt::Return(Some(e)) | Stmt::Drop(e) => collect_idents_in_expr(e, idents),
        Stmt::Return(None) | Stmt::Break(None) | Stmt::Continue => {}
        Stmt::Break(Some(e)) => collect_idents_in_expr(e, idents),
        Stmt::Let { init: Some(e), .. } | Stmt::SharedLet { init: e, .. } => {
            collect_idents_in_expr(e, idents)
        }
        Stmt::Let { init: None, .. } => {}
        Stmt::Assign { target, value } => {
            collect_idents_in_expr(target, idents);
            collect_idents_in_expr(value, idents);
        }
        Stmt::If { cond, then_, else_ } => {
            collect_idents_in_expr(cond, idents);
            for s in then_ {
                collect_idents_in_stmt(s, idents);
            }
            if let Some(e) = else_ {
                for s in e {
                    collect_idents_in_stmt(s, idents);
                }
            }
        }
        Stmt::While { cond, body } | Stmt::For { iterable: cond, body, .. } => {
            collect_idents_in_expr(cond, idents);
            for s in body {
                collect_idents_in_stmt(s, idents);
            }
        }
        Stmt::Block(block)
        | Stmt::Arena(block)
        | Stmt::OnFailure(block)
        | Stmt::Parasteps(block)
        | Stmt::Unsafe(block) => {
            for s in block {
                collect_idents_in_stmt(s, idents);
            }
        }
        Stmt::Alloc { body, .. } => {
            for s in body {
                collect_idents_in_stmt(s, idents);
            }
        }
        Stmt::Requires(e, _) | Stmt::Ensures(e, _) | Stmt::Invariant(e, _) => collect_idents_in_expr(e, idents),
        Stmt::Math(exprs) => {
            for e in exprs {
                collect_idents_in_expr(e, idents);
            }
        }
        _ => {}
    }
}

pub(crate) fn parse_contract_expr(text: &str) -> Result<Expr, String> {
    let tokens = crate::lexer::Lexer::new(text).tokenize()?;
    let expr = crate::parser::Parser::new(tokens)
        .parse_expr(0)
        .map_err(|e| e.message)?;
    Ok(expr)
}

/// Return Unknown for all functions when Z3 is not available.
pub(crate) fn mock_verify_file(file: &crate::ast::File) -> Vec<VerificationResult> {
    let mut results = Vec::new();
    mock_verify_items(&file.items, &mut results);
    results
}

fn mock_verify_items(items: &[crate::ast::Item], results: &mut Vec<VerificationResult>) {
    for item in items {
        match item {
            crate::ast::Item::Func(f) => {
                if !f.body.is_empty() {
                    let has_contracts = f.body.iter().any(|s| {
                        matches!(
                            s,
                            Stmt::Requires(_, _)
                                | Stmt::Ensures(_, _)
                                | Stmt::Invariant(_, _)
                                | Stmt::MmsBlock { .. }
                        )
                    });
                    results.push(VerificationResult {
                        func_name: f.name.clone(),
                        status: VerifStatus::Unknown,
                        message: if has_contracts {
                            "Z3 solver not available"
                        } else {
                            "no contracts"
                        }
                        .into(),
                        diagnostic: None,
                        duration_us: 0,
                        constraint_count: 0,
                    });
                }
            }
            crate::ast::Item::Module(m) => mock_verify_items(&m.items, results),
            crate::ast::Item::ExternBlock(block) => {
                for func in &block.funcs {
                    if func.requires.is_some() || func.ensures.is_some() {
                        results.push(VerificationResult {
                            func_name: format!("extern {}", func.name),
                            status: VerifStatus::Unknown,
                            message: "Z3 solver not available".into(),
                            diagnostic: None,
                            duration_us: 0,
                            constraint_count: 0,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}
