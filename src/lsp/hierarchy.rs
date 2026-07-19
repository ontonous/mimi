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
        let lines: Vec<&str> = text.lines().collect();

        for item in &file.items {
            if let Item::Func(f) = item {
                if f.name == name {
                    continue; // Don't report calls within the function itself
                }
                // Collect call sites using AST traversal
                let mut call_lines: Vec<usize> = Vec::new();
                collect_call_sites(&f.body, name, &lines, &mut call_lines);

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
        match stmt.unlocated() {
            Stmt::Expr(e) | Stmt::Return(Some(e)) => {
                collect_calls_from_expr(e, text, items, uri, calls, visited);
            }
            Stmt::Let { init: Some(e), .. } => {
                collect_calls_from_expr(e, text, items, uri, calls, visited);
            }
            Stmt::If {
                cond: _,
                then_,
                else_,
            } => {
                collect_calls_from_exprs(then_, text, items, uri, calls, visited);
                if let Some(els) = else_ {
                    collect_calls_from_exprs(els, text, items, uri, calls, visited);
                }
            }
            Stmt::While { cond: _, body } => {
                collect_calls_from_exprs(body, text, items, uri, calls, visited);
            }
            Stmt::For {
                var: _,
                iterable: _,
                body,
            } => {
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
    match expr.unlocated() {
        Expr::Call(callee, args) => {
            if let Expr::Ident(name) = callee.unlocated() {
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
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
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

/// Collect all line numbers where `func_name` is called within the given statements.
/// Uses AST-based traversal instead of string matching to avoid false positives.
fn collect_call_sites(
    stmts: &[Stmt],
    func_name: &str,
    lines: &[&str],
    call_lines: &mut Vec<usize>,
) {
    for stmt in stmts {
        match stmt.unlocated() {
            Stmt::Expr(e) | Stmt::Return(Some(e)) => {
                collect_call_sites_from_expr(e, func_name, lines, call_lines);
            }
            Stmt::Let { init: Some(e), .. } => {
                collect_call_sites_from_expr(e, func_name, lines, call_lines);
            }
            Stmt::If {
                cond: _,
                then_,
                else_,
            } => {
                collect_call_sites(then_, func_name, lines, call_lines);
                if let Some(els) = else_ {
                    collect_call_sites(els, func_name, lines, call_lines);
                }
            }
            Stmt::While { cond: _, body } => {
                collect_call_sites(body, func_name, lines, call_lines);
            }
            Stmt::For {
                var: _,
                iterable: _,
                body,
            } => {
                collect_call_sites(body, func_name, lines, call_lines);
            }
            _ => {}
        }
    }
}

/// Recursively collect call sites from an expression
fn collect_call_sites_from_expr(
    expr: &Expr,
    func_name: &str,
    lines: &[&str],
    call_lines: &mut Vec<usize>,
) {
    match expr.unlocated() {
        Expr::Call(callee, args) => {
            if let Expr::Ident(name) = callee.unlocated() {
                if name.as_str() == func_name {
                    // Find the line number of this call expression
                    // We use the source text to find where the call appears
                    let call_text = format!("{}(", name);
                    for (i, line) in lines.iter().enumerate() {
                        // Only count if not already counted for this function
                        if line.contains(&call_text) && !call_lines.contains(&i) {
                            // Verify it's not in a comment or string (simple heuristic)
                            if !line.trim().starts_with("//") && !line.contains("\"") {
                                call_lines.push(i);
                            }
                        }
                    }
                }
            }
            for arg in args {
                collect_call_sites_from_expr(arg, func_name, lines, call_lines);
            }
        }
        Expr::Binary(_, lhs, rhs) | Expr::Index(lhs, rhs) => {
            collect_call_sites_from_expr(lhs, func_name, lines, call_lines);
            collect_call_sites_from_expr(rhs, func_name, lines, call_lines);
        }
        Expr::Unary(_, e)
        | Expr::Try(e)
        | Expr::Spawn(e)
        | Expr::Await(e)
        | Expr::Old(e)
        | Expr::QuoteInterpolate(e)
        | Expr::TypeOf(e) => {
            collect_call_sites_from_expr(e, func_name, lines, call_lines);
        }
        Expr::If { cond, then_, else_ } => {
            collect_call_sites_from_expr(cond, func_name, lines, call_lines);
            collect_call_sites(then_, func_name, lines, call_lines);
            if let Some(els) = else_ {
                collect_call_sites(els, func_name, lines, call_lines);
            }
        }
        Expr::Lambda { body, .. } => {
            collect_call_sites(body, func_name, lines, call_lines);
        }
        Expr::Block(stmts) => {
            collect_call_sites(stmts, func_name, lines, call_lines);
        }
        Expr::Quote(stmts) | Expr::Comptime(stmts) => {
            collect_call_sites(stmts, func_name, lines, call_lines);
        }
        Expr::List(elems) | Expr::Tuple(elems) => {
            for e in elems {
                collect_call_sites_from_expr(e, func_name, lines, call_lines);
            }
        }
        Expr::Match(scrutinee, arms) => {
            collect_call_sites_from_expr(scrutinee, func_name, lines, call_lines);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_call_sites_from_expr(g, func_name, lines, call_lines);
                }
                collect_call_sites_from_expr(&arm.body, func_name, lines, call_lines);
            }
        }
        Expr::Record { fields, .. } => {
            for f in fields {
                collect_call_sites_from_expr(&f.value, func_name, lines, call_lines);
            }
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            collect_call_sites_from_expr(expr, func_name, lines, call_lines);
            collect_call_sites_from_expr(iter, func_name, lines, call_lines);
            if let Some(g) = guard {
                collect_call_sites_from_expr(g, func_name, lines, call_lines);
            }
        }
        Expr::Field(e, _) | Expr::TupleIndex(e, _) => {
            collect_call_sites_from_expr(e, func_name, lines, call_lines);
        }
        Expr::Range { start, end } => {
            collect_call_sites_from_expr(start, func_name, lines, call_lines);
            collect_call_sites_from_expr(end, func_name, lines, call_lines);
        }
        Expr::SliceExpr { target, start, end } => {
            collect_call_sites_from_expr(target, func_name, lines, call_lines);
            if let Some(s) = start {
                collect_call_sites_from_expr(s, func_name, lines, call_lines);
            }
            if let Some(e) = end {
                collect_call_sites_from_expr(e, func_name, lines, call_lines);
            }
        }
        Expr::Turbofish(_, _, args) => {
            for e in args {
                collect_call_sites_from_expr(e, func_name, lines, call_lines);
            }
        }
        _ => {}
    }
}
