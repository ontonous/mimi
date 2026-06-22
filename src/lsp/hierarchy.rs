use std::collections::HashSet;

use serde_json::Value;

use crate::ast::{Expr, Item, Stmt};
use crate::lsp::LspServer;

impl LspServer {
    /// Compute incoming calls: who calls the given function
    pub fn compute_incoming_calls(&self, text: &str, uri: &str, name: &str) -> Vec<Value> {
        let file = match self.parse_with_recovery(text) {
            Some(f) => f,
            None => return vec![],
        };
        let mut calls = Vec::new();
        // Find the definition line of `name` so we can exclude it
        let def_line = text.lines().position(|l| l.contains(&format!("func {}", name)));
        for item in &file.items {
            if let Item::Func(f) = item {
                if f.name == name {
                    continue;
                }
                let call_lines: Vec<usize> = text
                    .lines()
                    .enumerate()
                    .filter(|(i, l)| {
                        if Some(*i) == def_line {
                            return false;
                        }
                        l.contains(&format!("{}(", name))
                    })
                    .map(|(i, _)| i)
                    .collect();
                if !call_lines.is_empty() {
                    let func_line = text
                        .lines()
                        .position(|l| l.contains(&format!("func {}", f.name)))
                        .unwrap_or(0);
                    calls.push(serde_json::json!({
                        "from": {
                            "name": f.name,
                            "kind": 12,
                            "uri": uri,
                            "range": {
                                "start": { "line": func_line, "character": 0 },
                                "end": { "line": func_line, "character": 0 }
                            },
                            "selectionRange": {
                                "start": { "line": func_line, "character": 5 },
                                "end": { "line": func_line, "character": 5 + f.name.len() }
                            }
                        },
                        "fromRanges": call_lines.iter().map(|&l| serde_json::json!({
                            "start": { "line": l, "character": 0 },
                            "end": { "line": l, "character": 0 }
                        })).collect::<Vec<_>>()
                    }));
                }
            }
        }
        calls
    }

    /// Compute outgoing calls: which functions does the given function call
    pub fn compute_outgoing_calls(&self, text: &str, uri: &str, name: &str) -> Vec<Value> {
        let file = match self.parse_with_recovery(text) {
            Some(f) => f,
            None => return vec![],
        };
        // Find the function body
        let mut calls = Vec::new();
        for item in &file.items {
            if let Item::Func(f) = item {
                if f.name != name {
                    continue;
                }
                // Scan this function's body for function calls
                let mut visited = HashSet::new();
                collect_calls_from_exprs(&f.body, text, &file.items, uri, &mut calls, &mut visited);
            }
        }
        calls
    }
}

/// Collect all function calls from a list of statements into the given result vector
fn collect_calls_from_exprs(
    stmts: &[Stmt],
    text: &str,
    items: &[Item],
    uri: &str,
    calls: &mut Vec<Value>,
    visited: &mut HashSet<String>,
) {
    for stmt in stmts {
        match stmt {
            Stmt::Expr(e) | Stmt::Return(Some(e)) => {
                collect_calls_from_expr(e, text, items, uri, calls, visited);
            }
            Stmt::Let { init: Some(e), .. } => {
                collect_calls_from_expr(e, text, items, uri, calls, visited);
            }
            Stmt::If { cond: _, then_, else_ } => {
                collect_calls_from_exprs(then_, text, items, uri, calls, visited);
                if let Some(els) = else_ {
                    collect_calls_from_exprs(els, text, items, uri, calls, visited);
                }
            }
            Stmt::While { cond: _, body } => {
                collect_calls_from_exprs(body, text, items, uri, calls, visited);
            }
            Stmt::For { var: _, iterable: _, body } => {
                collect_calls_from_exprs(body, text, items, uri, calls, visited);
            }
            _ => {}
        }
    }
}

/// Recursively collect function calls from an expression
fn collect_calls_from_expr(
    expr: &Expr,
    text: &str,
    items: &[Item],
    uri: &str,
    calls: &mut Vec<Value>,
    visited: &mut HashSet<String>,
) {
    match expr {
        Expr::Call(callee, args) => {
            if let Expr::Ident(name) = callee.as_ref() {
                if !visited.contains(name.as_str()) {
                    visited.insert(name.clone());
                    let callee_line = text
                        .lines()
                        .position(|l| l.contains(&format!("func {}", name)))
                        .unwrap_or(0);
                    let call_line = text
                        .lines()
                        .position(|l| l.contains(&format!("{}(", name)))
                        .unwrap_or(0);
                    calls.push(serde_json::json!({
                        "to": {
                            "name": name,
                            "kind": 12,
                            "uri": uri,
                            "range": {
                                "start": { "line": callee_line, "character": 0 },
                                "end": { "line": callee_line, "character": 0 }
                            },
                            "selectionRange": {
                                "start": { "line": callee_line, "character": 5 },
                                "end": { "line": callee_line, "character": 5 + name.len() }
                            }
                        },
                        "fromRanges": [{
                            "start": { "line": call_line, "character": 0 },
                            "end": { "line": call_line, "character": 0 }
                        }]
                    }));
                }
            }
            for arg in args {
                collect_calls_from_expr(arg, text, items, uri, calls, visited);
            }
        }
        Expr::Binary(_, lhs, rhs) | Expr::Index(lhs, rhs) => {
            collect_calls_from_expr(lhs, text, items, uri, calls, visited);
            collect_calls_from_expr(rhs, text, items, uri, calls, visited);
        }
        Expr::Unary(_, e)
        | Expr::Try(e)
        | Expr::Spawn(e)
        | Expr::Await(e)
        | Expr::Old(e)
        | Expr::QuoteInterpolate(e)
        | Expr::TypeOf(e) => {
            collect_calls_from_expr(e, text, items, uri, calls, visited);
        }
        Expr::If { cond, then_, else_ } => {
            collect_calls_from_expr(cond, text, items, uri, calls, visited);
            collect_calls_from_exprs(then_, text, items, uri, calls, visited);
            if let Some(els) = else_ {
                collect_calls_from_exprs(els, text, items, uri, calls, visited);
            }
        }
        Expr::Lambda { body, .. } => {
            collect_calls_from_exprs(body, text, items, uri, calls, visited);
        }
        Expr::Block(stmts) => {
            collect_calls_from_exprs(stmts, text, items, uri, calls, visited);
        }
        Expr::Quote(stmts) | Expr::Comptime(stmts) => {
            collect_calls_from_exprs(stmts, text, items, uri, calls, visited);
        }
        Expr::List(elems) | Expr::Tuple(elems) => {
            for e in elems {
                collect_calls_from_expr(e, text, items, uri, calls, visited);
            }
        }
        Expr::Match(scrutinee, arms) => {
            collect_calls_from_expr(scrutinee, text, items, uri, calls, visited);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_calls_from_expr(g, text, items, uri, calls, visited);
                }
                collect_calls_from_expr(&arm.body, text, items, uri, calls, visited);
            }
        }
        Expr::Record { fields, .. } => {
            for f in fields {
                collect_calls_from_expr(&f.value, text, items, uri, calls, visited);
            }
        }
        Expr::Comprehension { expr, iter, guard, .. } => {
            collect_calls_from_expr(expr, text, items, uri, calls, visited);
            collect_calls_from_expr(iter, text, items, uri, calls, visited);
            if let Some(g) = guard {
                collect_calls_from_expr(g, text, items, uri, calls, visited);
            }
        }
        Expr::Field(e, _) | Expr::TupleIndex(e, _) => {
            collect_calls_from_expr(e, text, items, uri, calls, visited);
        }
        Expr::Range { start, end } => {
            collect_calls_from_expr(start, text, items, uri, calls, visited);
            collect_calls_from_expr(end, text, items, uri, calls, visited);
        }
        Expr::SliceExpr { target, start, end } => {
            collect_calls_from_expr(target, text, items, uri, calls, visited);
            if let Some(s) = start {
                collect_calls_from_expr(s, text, items, uri, calls, visited);
            }
            if let Some(e) = end {
                collect_calls_from_expr(e, text, items, uri, calls, visited);
            }
        }
        Expr::Turbofish(_, _, args) => {
            for e in args {
                collect_calls_from_expr(e, text, items, uri, calls, visited);
            }
        }
        _ => {}
    }
}
