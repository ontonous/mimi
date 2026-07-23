use crate::ast::*;

use super::Checker;

impl<'a> Checker<'a> {
    /// Collect variable names for the test-only raw-AST CFG oracle.
    pub(crate) fn collect_uses_in_expr(expr: &Expr, uses: &mut Vec<String>) {
        match expr.unlocated() {
            Expr::Ident(name) => uses.push(name.clone()),
            Expr::Unary(_, inner) => Self::collect_uses_in_expr(inner, uses),
            Expr::Binary(_, l, r) => {
                Self::collect_uses_in_expr(l, uses);
                Self::collect_uses_in_expr(r, uses);
            }
            Expr::Call(callee, args) => {
                Self::collect_uses_in_expr(callee, uses);
                for arg in args {
                    Self::collect_uses_in_expr(arg, uses);
                }
            }
            Expr::Field(obj, _) => Self::collect_uses_in_expr(obj, uses),
            Expr::TupleIndex(obj, _) => Self::collect_uses_in_expr(obj, uses),
            Expr::Index(obj, idx) => {
                Self::collect_uses_in_expr(obj, uses);
                Self::collect_uses_in_expr(idx, uses);
            }
            Expr::Block(block) => {
                for s in block {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Expr::If { cond, then_, else_ } => {
                Self::collect_uses_in_expr(cond, uses);
                for s in then_ {
                    Self::collect_uses_in_stmt(s, uses);
                }
                if let Some(e) = else_ {
                    for s in e {
                        Self::collect_uses_in_stmt(s, uses);
                    }
                }
            }
            Expr::Tuple(elems) => {
                for e in elems {
                    Self::collect_uses_in_expr(e, uses);
                }
            }
            Expr::List(elems) => {
                for e in elems {
                    Self::collect_uses_in_expr(e, uses);
                }
            }
            Expr::Lambda { body, .. } => {
                for s in body {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Expr::Match(scrutinee, arms) => {
                Self::collect_uses_in_expr(scrutinee, uses);
                for arm in arms {
                    if let Some(ref guard) = arm.guard {
                        Self::collect_uses_in_expr(guard, uses);
                    }
                    Self::collect_uses_in_expr(&arm.body, uses);
                }
            }
            Expr::Try(inner) => Self::collect_uses_in_expr(inner, uses),
            Expr::OptionalChain(inner, _) => Self::collect_uses_in_expr(inner, uses),
            Expr::Spawn(inner) => Self::collect_uses_in_expr(inner, uses),
            Expr::Await(inner) => Self::collect_uses_in_expr(inner, uses),
            Expr::Range { start, end } => {
                Self::collect_uses_in_expr(start, uses);
                Self::collect_uses_in_expr(end, uses);
            }
            Expr::SliceExpr { target, start, end } => {
                Self::collect_uses_in_expr(target, uses);
                if let Some(s) = start {
                    Self::collect_uses_in_expr(s, uses);
                }
                if let Some(e) = end {
                    Self::collect_uses_in_expr(e, uses);
                }
            }
            Expr::Turbofish(_, _, args) => {
                for a in args {
                    Self::collect_uses_in_expr(a, uses);
                }
            }
            Expr::Arena(block) => {
                for s in block {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Expr::Literal(_)
            | Expr::Old(_)
            | Expr::Comptime(_)
            | Expr::Quote(_)
            | Expr::QuoteInterpolate(_)
            | Expr::TypeInfo(_)
            | Expr::TypeOf(_) => {}
            Expr::Record { fields, .. } => {
                for f in fields {
                    Self::collect_uses_in_expr(&f.value, uses);
                }
            }
            Expr::Comprehension {
                expr, iter, guard, ..
            } => {
                Self::collect_uses_in_expr(expr, uses);
                Self::collect_uses_in_expr(iter, uses);
                if let Some(g) = guard {
                    Self::collect_uses_in_expr(g, uses);
                }
            }
            Expr::MapLiteral { entries } => {
                for (k, v) in entries {
                    Self::collect_uses_in_expr(k, uses);
                    Self::collect_uses_in_expr(v, uses);
                }
            }
            Expr::SetLiteral(elems) => {
                for e in elems {
                    Self::collect_uses_in_expr(e, uses);
                }
            }
            Expr::NamedArg(_, value) => Self::collect_uses_in_expr(value, uses),
            Expr::Cast(inner, _) => Self::collect_uses_in_expr(inner, uses),
            Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
        }
    }

    /// Collect all variable names used in a statement
    pub(crate) fn collect_uses_in_stmt(stmt: &Stmt, uses: &mut Vec<String>) {
        match stmt.unlocated() {
            Stmt::Expr(e) => Self::collect_uses_in_expr(e, uses),
            Stmt::Return(Some(e)) => Self::collect_uses_in_expr(e, uses),
            Stmt::Return(None) => {}
            Stmt::Let { init: Some(e), .. } => Self::collect_uses_in_expr(e, uses),
            Stmt::Let { init: None, .. } => {}
            Stmt::Assign { target, value } => {
                Self::collect_uses_in_expr(target, uses);
                Self::collect_uses_in_expr(value, uses);
            }
            Stmt::If { cond, then_, else_ } => {
                Self::collect_uses_in_expr(cond, uses);
                for s in then_ {
                    Self::collect_uses_in_stmt(s, uses);
                }
                if let Some(e) = else_ {
                    for s in e {
                        Self::collect_uses_in_stmt(s, uses);
                    }
                }
            }
            Stmt::While { cond, body } => {
                Self::collect_uses_in_expr(cond, uses);
                for s in body {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Stmt::WhileLet { init, body, .. } => {
                Self::collect_uses_in_expr(init, uses);
                for s in body {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Stmt::For { iterable, body, .. } => {
                Self::collect_uses_in_expr(iterable, uses);
                for s in body {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Stmt::Block(block) => {
                for s in block {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Stmt::Break(Some(e)) => Self::collect_uses_in_expr(e, uses),
            Stmt::Break(None) | Stmt::Continue => {}
            Stmt::Requires(e, _) | Stmt::Ensures(e, _) | Stmt::Invariant(e, _) | Stmt::Drop(e) => {
                Self::collect_uses_in_expr(e, uses)
            }
            Stmt::SharedLet { init, .. } => Self::collect_uses_in_expr(init, uses),
            Stmt::Arena(block)
            | Stmt::OnFailure(block)
            | Stmt::Parasteps(block)
            | Stmt::Unsafe(block) => {
                for s in block {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Stmt::Math(exprs) => {
                for e in exprs {
                    Self::collect_uses_in_expr(e, uses);
                }
            }
            Stmt::Alloc { body, .. } => {
                for s in body {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Stmt::Func(func) => {
                for s in &func.body {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Stmt::MmsBlock { .. } | Stmt::Ellipsis | Stmt::Desc(..) | Stmt::Rule(..) | Stmt::Stay => {}
            Stmt::Become(e) => Self::collect_uses_in_expr(e, uses),
            Stmt::Loop(body) => {
                for s in body {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Stmt::Do(body) => {
                for s in body {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Stmt::Delegate { expr, .. } => {
                Self::collect_uses_in_expr(expr, uses);
            }
            Stmt::Pinned { expr, body, .. } => {
                Self::collect_uses_in_expr(expr, uses);
                for s in body {
                    Self::collect_uses_in_stmt(s, uses);
                }
            }
            Stmt::Located { .. } => unreachable!("Stmt::unlocated returned Located"),
        }
    }
}
