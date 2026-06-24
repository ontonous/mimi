use crate::ast::*;
use crate::core::borrow::BorrowState;
use crate::span::Span;
use std::collections::HashMap;

use super::Checker;

impl<'a> Checker<'a> {
    // ─── Whole-variable borrow scope management ───────────────
    pub(crate) fn push_borrow_scope(&mut self) {
        self.borrows.push(HashMap::new());
        self.field_borrows.push(HashMap::new());
    }

    pub(crate) fn pop_borrow_scope(&mut self) {
        self.borrows.pop();
        self.field_borrows.pop();
    }

    // ─── Whole-variable borrow tracking ──────────────────────
    pub(crate) fn lookup_borrow(&self, name: &str) -> Option<&BorrowState> {
        for scope in self.borrows.iter().rev() {
            if let Some(state) = scope.get(name) {
                return Some(state);
            }
        }
        None
    }

    pub(crate) fn set_borrow(&mut self, name: &str, state: BorrowState) {
        if let Some(scope) = self.borrows.last_mut() {
            scope.insert(name.into(), state);
        }
    }

    /// Release a borrow (set back to Unborrowed) — NLL last-use release
    pub(crate) fn release_borrow(&mut self, name: &str) {
        if let Some(scope) = self.borrows.last_mut() {
            scope.insert(name.into(), BorrowState::Unborrowed);
        }
    }

    // ─── Field-level borrow tracking (path-sensitive) ────────

    /// Check if a specific field path on a variable is borrowed.
    /// Returns true if the field path (or the whole variable) is borrowed
    /// in a way that conflicts with `mutable`.
    pub(crate) fn is_field_borrowed(&self, var: &str, field: &str, mutable: bool) -> Option<Span> {
        let key = (var.to_string(), vec![field.to_string()]);
        for scope in self.field_borrows.iter().rev() {
            if let Some(state) = scope.get(&key) {
                return match state {
                    BorrowState::BorrowedMut { span } => Some(*span),
                    BorrowState::BorrowedImm { span } if mutable => Some(*span),
                    _ => None,
                };
            }
        }
        // Also check if the whole variable is borrowed
        if let Some(state) = self.lookup_borrow(var) {
            return match state {
                BorrowState::BorrowedMut { span } => Some(*span),
                BorrowState::BorrowedImm { span } if mutable => Some(*span),
                _ => None,
            };
        }
        None
    }

    /// Set a borrow on a specific field path.
    pub(crate) fn set_field_borrow(&mut self, var: &str, field: &str, state: BorrowState) {
        if let Some(scope) = self.field_borrows.last_mut() {
            let key = (var.to_string(), vec![field.to_string()]);
            scope.insert(key, state);
        }
    }

    /// Check if any field of a variable is borrowed (for whole-variable borrow checks).
    /// Returns true if any field is actively borrowed.
    pub(crate) fn any_field_borrowed(&self, var: &str) -> bool {
        if let Some(scope) = self.field_borrows.last() {
            scope.iter().any(|((v, _), state)| {
                v == var && !matches!(state, BorrowState::Unborrowed)
            })
        } else {
            false
        }
    }

    /// Collect all variable names used in an expression (shallow)
    pub(crate) fn collect_uses_in_expr(expr: &Expr, uses: &mut Vec<String>) {
        match expr {
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
                for s in block { Self::collect_uses_in_stmt(s, uses); }
            }
            Expr::If { cond, then_, else_ } => {
                Self::collect_uses_in_expr(cond, uses);
                for s in then_ { Self::collect_uses_in_stmt(s, uses); }
                if let Some(e) = else_ { for s in e { Self::collect_uses_in_stmt(s, uses); } }
            }
            Expr::Tuple(elems) => { for e in elems { Self::collect_uses_in_expr(e, uses); } }
            Expr::List(elems) => { for e in elems { Self::collect_uses_in_expr(e, uses); } }
            Expr::Lambda { body, .. } => { for s in body { Self::collect_uses_in_stmt(s, uses); } }
            Expr::Match(scrutinee, arms) => {
                Self::collect_uses_in_expr(scrutinee, uses);
                for arm in arms { Self::collect_uses_in_expr(&arm.body, uses); }
            }
            Expr::Try(inner) => Self::collect_uses_in_expr(inner, uses),
            Expr::Spawn(inner) => Self::collect_uses_in_expr(inner, uses),
            Expr::Await(inner) => Self::collect_uses_in_expr(inner, uses),
            Expr::Range { start, end } => {
                Self::collect_uses_in_expr(start, uses);
                Self::collect_uses_in_expr(end, uses);
            }
            Expr::SliceExpr { target, start, end } => {
                Self::collect_uses_in_expr(target, uses);
                if let Some(s) = start { Self::collect_uses_in_expr(s, uses); }
                if let Some(e) = end { Self::collect_uses_in_expr(e, uses); }
            }
            Expr::Turbofish(_, _, args) => { for a in args { Self::collect_uses_in_expr(a, uses); } }
            Expr::Arena(block) => { for s in block { Self::collect_uses_in_stmt(s, uses); } }
            Expr::Literal(_) | Expr::Old(_) | Expr::Comptime(_) | Expr::Quote(_) | Expr::QuoteInterpolate(_) | Expr::TypeInfo(_) | Expr::TypeOf(_) => {}
            Expr::Record { fields, .. } => { for f in fields { Self::collect_uses_in_expr(&f.value, uses); } }
            Expr::Comprehension { expr, iter, guard, .. } => {
                Self::collect_uses_in_expr(expr, uses);
                Self::collect_uses_in_expr(iter, uses);
                if let Some(g) = guard { Self::collect_uses_in_expr(g, uses); }
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
        }
    }

    /// Collect all variable names used in a statement
    pub(crate) fn collect_uses_in_stmt(stmt: &Stmt, uses: &mut Vec<String>) {
        match stmt {
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
                for s in then_ { Self::collect_uses_in_stmt(s, uses); }
                if let Some(e) = else_ { for s in e { Self::collect_uses_in_stmt(s, uses); } }
            }
            Stmt::While { cond, body } => {
                Self::collect_uses_in_expr(cond, uses);
                for s in body { Self::collect_uses_in_stmt(s, uses); }
            }
            Stmt::For { iterable, body, .. } => {
                Self::collect_uses_in_expr(iterable, uses);
                for s in body { Self::collect_uses_in_stmt(s, uses); }
            }
            Stmt::Block(block) => { for s in block { Self::collect_uses_in_stmt(s, uses); } }
            Stmt::Break(Some(e)) => Self::collect_uses_in_expr(e, uses),
            Stmt::Break(None) | Stmt::Continue => {}
            Stmt::Requires(e, _) | Stmt::Ensures(e, _) | Stmt::Invariant(e, _) | Stmt::Drop(e) => Self::collect_uses_in_expr(e, uses),
            Stmt::SharedLet { init, .. } => Self::collect_uses_in_expr(init, uses),
            Stmt::Arena(block) | Stmt::OnFailure(block) | Stmt::Parasteps(block) | Stmt::Unsafe(block) => {
                for s in block { Self::collect_uses_in_stmt(s, uses); }
            }
            Stmt::Math(exprs) => { for e in exprs { Self::collect_uses_in_expr(e, uses); } }
            Stmt::Alloc { body, .. } => { for s in body { Self::collect_uses_in_stmt(s, uses); } }
            Stmt::MmsBlock { .. } | Stmt::Ellipsis | Stmt::Desc(..) | Stmt::Rule(..) => {}
            Stmt::Loop(body) => { for s in body { Self::collect_uses_in_stmt(s, uses); } }
        }
    }

    /// NLL: Release borrows at their last use within a block.
    /// Called before checking statement `current_idx`. Releases any borrow whose
    /// borrow reference variable is NOT used in the current or any later statement.
    pub(crate) fn release_borrows_at_last_use(&mut self, block: &[Stmt], current_idx: usize) {
        // Collect currently borrowed variables and their borrow reference names
        let borrows: Vec<(String, String)> = {
            if let Some(scope) = self.borrows.last() {
                scope.iter()
                    .filter(|(_, state)| !matches!(state, BorrowState::Unborrowed))
                    .map(|(name, _)| {
                        // Find the borrow reference variable name
                        // It's typically: let r = &x  -> borrow_ref = "r", borrowed_var = "x"
                        let borrow_ref = self.find_borrow_ref(name, block, current_idx);
                        (name.clone(), borrow_ref)
                    })
                    .collect()
            } else {
                vec![]
            }
        };

        for (borrowed_var, borrow_ref) in &borrows {
            if matches!(self.lookup_borrow(borrowed_var), Some(BorrowState::Unborrowed) | None) {
                continue;
            }

            // NLL: Release borrow if the reference variable is completely unused from now on.
            // Check: is the reference used in any statement from current_idx onward?
            let ref_used_after = block[current_idx..].iter().any(|s| {
                let mut uses = Vec::new();
                Self::collect_uses_in_stmt(s, &mut uses);
                uses.contains(borrow_ref)
            });

            // Release only if ref is not used from current point onward
            if !ref_used_after {
                self.release_borrow(borrowed_var);
            }
        }
    }

    /// Find the name of the variable that holds a borrow reference to `borrowed_var`.
    /// Scans earlier statements for `let ref_name = &borrowed_var` patterns.
    pub(crate) fn find_borrow_ref(&self, borrowed_var: &str, block: &[Stmt], current_idx: usize) -> String {
        for stmt in &block[..current_idx] {
            if let Stmt::Let { pat, init: Some(Expr::Unary(UnOp::Ref, inner)), .. } = stmt {
                if let Expr::Ident(name) = inner.as_ref() {
                    if name == borrowed_var {
                        if let Pattern::Variable(ref_name) = pat {
                            return ref_name.clone();
                        }
                    }
                }
            }
            if let Stmt::Let { pat, init: Some(Expr::Unary(UnOp::RefMut, inner)), .. } = stmt {
                if let Expr::Ident(name) = inner.as_ref() {
                    if name == borrowed_var {
                        if let Pattern::Variable(ref_name) = pat {
                            return ref_name.clone();
                        }
                    }
                }
            }
        }
        borrowed_var.to_string()
    }
}
