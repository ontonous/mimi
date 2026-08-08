#![allow(dead_code)]

use crate::ast::*;
use crate::verifier::ctx::{VerifStatus, VerificationResult};

/// Extract the final value-producing expression from a block.
/// Used in `expr_to_z3_*` to evaluate the tail expression of an if-else branch.
pub(crate) fn block_tail_expr(block: &[Stmt]) -> Option<Expr> {
    for stmt in block.iter().rev() {
        match stmt.unlocated() {
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
        && match (lhs.unlocated(), rhs.unlocated()) {
            (Expr::Ident(_), Expr::Literal(Lit::String(s)))
            | (Expr::Literal(Lit::String(s)), Expr::Ident(_)) => s.is_empty(),
            _ => false,
        }
}

/// Extract the string ident name from a string emptiness comparison.
/// Assumes `is_string_empty_cmp` returned `true`.
pub(crate) fn extract_string_empty_cmp(lhs: &Expr, rhs: &Expr, op: &BinOp) -> (String, BinOp) {
    match (lhs.unlocated(), rhs.unlocated()) {
        (Expr::Ident(name), Expr::Literal(Lit::String(_))) => (name.clone(), *op),
        (Expr::Literal(Lit::String(_)), Expr::Ident(name)) => (name.clone(), *op),
        _ => (String::new(), *op),
    }
}

/// Extract the return/tail expression from a function body, handling if-else branching.
/// Uses `Expr::If` to represent conditional paths so the Z3 layer can encode them via `ite`.
///
/// V-C3: scan **forward** for the first reachable explicit `return` / top-level
/// `if`. Reverse search previously picked dead code after an early return
/// (e.g. `return 0; return 1` → wrongly chose `1`).
///
/// C-6 (full-audit-2026-08-05-0656 §1): an `if` whose branches yield NO
/// extractable return value (e.g. `if c { let y2 = y + 1 }`) no longer
/// propagates `None` as the overall result. Extraction failure means the
/// `if` produces no value and execution continues — the scan MUST keep
/// looking at subsequent statements. Previously the swallowed tail made
/// `ensures: result == 0` a fake Proven (func.rs binds result to 0 when
/// extraction fails) even though the runtime always returned the tail `y`.
pub(crate) fn extract_body_return(block: &[Stmt]) -> Option<Expr> {
    // First pass (forward): first explicit return or value-producing if wins.
    if let Some(early) = extract_forward_return(block) {
        return Some(early);
    }
    // Second pass (reverse): implicit return = last expression, skipping lets.
    for stmt in block.iter().rev() {
        match stmt.unlocated() {
            Stmt::Expr(expr) => return Some(expr.clone()),
            Stmt::If { cond, then_, else_ } => {
                if let Some(expr) = extract_if_return(cond, then_, else_) {
                    return Some(expr);
                }
                // #40 (full-audit-2026-08-05 §11): an `if let` whose branch
                // yields no extractable value (e.g. `if let Some(x) = opt {
                // y += x }` — zero matches produce no value) must NOT fall
                // through to `None`. Mirrors the C-6 forward-scan fix: keep
                // looking for the true tail. Previously the swallowed tail
                // made func.rs bind `result = 0`, faking a Proven.
            }
            // C-6: a tail bare block (including unsafe/ieee_float-style
            // wrapper blocks) contributes its own implicit value. Previously
            // `_ => break` discarded it, binding result to 0 → fake verdicts.
            Stmt::Block(inner)
            | Stmt::Arena(inner)
            | Stmt::Unsafe(inner)
            | Stmt::IeeeFloat(inner) => {
                return extract_body_return(inner);
            }
            Stmt::Alloc { body, .. } => {
                return extract_body_return(body);
            }
            Stmt::Requires(_, _)
            | Stmt::Ensures(_, _)
            | Stmt::Invariant(_, _)
            | Stmt::Math(_)
            | Stmt::Let { .. }
            | Stmt::Assign { .. } => continue,
            _ => break,
        }
    }
    None
}

/// Forward scan for guaranteed early returns: the first explicit `return`, or
/// an `if` whose branches BOTH yield extractable return expressions (i.e. no
/// fall-through path), or an early return nested inside an unconditional block
/// wrapper.
///
/// C-6: extraction failure on an `if` (a branch without a return/tail value)
/// means the statement produces no value and control falls through — the scan
/// continues instead of returning `None`. Block wrappers are recursed for
/// EARLY RETURNS ONLY: their tail expressions are discarded values unless the
/// block itself is the tail statement (handled by the reverse pass in
/// `extract_body_return`).
fn extract_forward_return(block: &[Stmt]) -> Option<Expr> {
    for stmt in block.iter() {
        match stmt.unlocated() {
            Stmt::Return(Some(expr)) => return Some(expr.clone()),
            Stmt::Return(None) => return Some(Expr::Literal(Lit::Unit)),
            Stmt::If { cond, then_, else_ } => {
                // C-6: only a fully-extractable if (value on every branch)
                // can stand in for the block's result here. On extraction
                // failure keep scanning — the tail statements after the if
                // are the true result.
                if let Some(expr) = extract_if_return(cond, then_, else_) {
                    return Some(expr);
                }
            }
            // Unconditional block wrappers: an early return inside them ends
            // the whole function. Tail expressions do NOT (they are the
            // wrapper's discarded value when the wrapper is not the tail).
            Stmt::Block(inner)
            | Stmt::Arena(inner)
            | Stmt::Unsafe(inner)
            | Stmt::IeeeFloat(inner)
            | Stmt::OnFailure(inner)
            | Stmt::Parasteps(inner) => {
                if let Some(expr) = extract_forward_return(inner) {
                    return Some(expr);
                }
            }
            Stmt::Alloc { body, .. } => {
                if let Some(expr) = extract_forward_return(body) {
                    return Some(expr);
                }
            }
            _ => continue,
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
    let desugared_stmt = |expr: Expr| {
        let meta = expr
            .meta()
            .map(|meta| AstNodeMeta::new(meta.span, AstOrigin::Desugared("verifier.if_return")))
            .unwrap_or_else(|| AstNodeMeta::synthetic(AstOrigin::Desugared("verifier.if_return")));
        Stmt::Expr(expr).with_meta(meta)
    };
    Some(Expr::If {
        cond: Box::new(cond.clone()),
        then_: vec![desugared_stmt(then_expr)],
        else_: Some(vec![desugared_stmt(else_expr)]),
    })
}

pub(crate) fn format_expr(expr: &Expr) -> String {
    match expr.unlocated() {
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
                    FStringPart::Interp(e) => format_expr(e).to_string(),
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
            let s: Vec<String> = block.iter().map(format_stmt).collect();
            format!("{{ {} }}", s.join("; "))
        }
        _ => "<expr>".to_string(),
    }
}

fn format_stmt(stmt: &Stmt) -> String {
    match stmt.unlocated() {
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
    match expr.unlocated() {
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
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            collect_idents_in_expr(expr, idents);
            collect_idents_in_expr(iter, idents);
            if let Some(g) = guard {
                collect_idents_in_expr(g, idents);
            }
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
    match stmt.unlocated() {
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
        Stmt::While { cond, body }
        | Stmt::For {
            iterable: cond,
            body,
            ..
        } => {
            collect_idents_in_expr(cond, idents);
            for s in body {
                collect_idents_in_stmt(s, idents);
            }
        }
        Stmt::Loop(body) => {
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
        Stmt::Requires(e, _) | Stmt::Ensures(e, _) | Stmt::Invariant(e, _) => {
            collect_idents_in_expr(e, idents)
        }
        Stmt::Math(exprs) => {
            for e in exprs {
                collect_idents_in_expr(e, idents);
            }
        }
        _ => {}
    }
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
                            s.unlocated(),
                            Stmt::Requires(_, _)
                                | Stmt::Ensures(_, _)
                                | Stmt::Invariant(_, _)
                                | Stmt::Math(_)
                        )
                    });
                    results.push(VerificationResult {
                        func_name: f.name.clone(),
                        status: VerifStatus::InfrastructureError,
                        message: if has_contracts {
                            "Z3 solver not available"
                        } else {
                            "no contracts"
                        }
                        .into(),
                        diagnostic: None,
                        duration_us: 0,
                        constraint_count: 0,
                        artifact: None,
                        trusted_subset_domain: None,
                    });
                }
            }
            crate::ast::Item::Module(m) => mock_verify_items(&m.items, results),
            crate::ast::Item::ExternBlock(block) => {
                for func in &block.funcs {
                    if func.requires.is_some() || func.ensures.is_some() {
                        results.push(VerificationResult {
                            func_name: format!("extern {}", func.name),
                            status: VerifStatus::InfrastructureError,
                            message: "Z3 solver not available".into(),
                            diagnostic: None,
                            duration_us: 0,
                            constraint_count: 0,
                            artifact: None,
                            trusted_subset_domain: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}
