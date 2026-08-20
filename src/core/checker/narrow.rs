//! Phase A (0.1.8): view / mutate / ref may not cross a task boundary.
//!
//! Single predicate: `TaskBoundaryKind::may_cross_task_boundary`.
//! Default deny for `view`, `mutate`, `&T` / `&mut T` entering spawn,
//! Channel elements, Future captures, and actor mailboxes.
//! Synchronous `func` parameters (the DSP mutate hot path) are not a
//! task boundary and stay legal.

use crate::ast::{Expr, Item, Param, ParamBorrow, Type, UnOp};
use crate::diagnostic::codes;
use std::collections::HashMap;

use super::Checker;

/// Permission a value carries when it is about to leave the current task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskBoundaryKind {
    Owned,
    View,
    Mutate,
    Ref,
}

impl TaskBoundaryKind {
    /// Owned values may cross. view / mutate / ref stay task-local.
    pub(crate) fn may_cross_task_boundary(self) -> bool {
        matches!(self, Self::Owned)
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::View => "view",
            Self::Mutate => "mutate",
            Self::Ref => "ref",
        }
    }
}

impl<'a> Checker<'a> {
    pub(crate) fn kind_of_type(ty: &Type) -> TaskBoundaryKind {
        match ty.unlocated() {
            Type::Ref(_, _) | Type::RefMut(_, _) | Type::Slice(_) => TaskBoundaryKind::Ref,
            Type::Option(inner) | Type::Shared(inner) | Type::Weak(inner) => {
                Self::kind_of_type(inner)
            }
            _ => TaskBoundaryKind::Owned,
        }
    }

    fn kind_of_ident(&self, name: &str, scopes: &[HashMap<String, Type>]) -> TaskBoundaryKind {
        if self.view_params.contains(name) {
            return TaskBoundaryKind::View;
        }
        if self.mutate_params.contains(name) {
            return TaskBoundaryKind::Mutate;
        }
        for scope in scopes.iter().rev() {
            if let Some(ty) = scope.get(name) {
                return Self::kind_of_type(ty);
            }
        }
        TaskBoundaryKind::Owned
    }

    fn kind_of_expr(&self, expr: &Expr, scopes: &[HashMap<String, Type>]) -> TaskBoundaryKind {
        match expr.unlocated() {
            Expr::Ident(name) => self.kind_of_ident(name, scopes),
            Expr::Unary(UnOp::Ref | UnOp::RefMut, _) => TaskBoundaryKind::Ref,
            Expr::Unary(_, inner)
            | Expr::Await(inner)
            | Expr::Try(inner)
            | Expr::Cast(inner, _) => self.kind_of_expr(inner, scopes),
            _ => TaskBoundaryKind::Owned,
        }
    }

    fn lookup_func_params(&self, name: &str) -> Option<&[Param]> {
        if let Some(params) = self.nested_func_params.get(name) {
            return Some(params.as_slice());
        }
        self.file.items.iter().find_map(|item| match item {
            Item::Func(f) if f.name == name => Some(f.params.as_slice()),
            _ => None,
        })
    }

    fn reject_kind_across(&mut self, kind: TaskBoundaryKind, boundary: &str, detail: &str) {
        if kind.may_cross_task_boundary() {
            return;
        }
        self.emit_code(
            codes::E0442,
            format!(
                "{} cannot cross task boundary ({}){}",
                kind.label(),
                boundary,
                detail
            ),
        );
    }

    /// Spawn / Future env: reject captured view/mutate/ref and callees whose
    /// parameters are view/mutate (the borrow would be of the parent frame).
    pub(crate) fn reject_narrow_across_spawn(
        &mut self,
        inner: &Expr,
        scopes: &[HashMap<String, Type>],
    ) {
        if let Expr::Call(callee, args) = inner.unlocated() {
            if let Expr::Ident(name) = callee.unlocated() {
                self.reject_callee_borrow_params("spawn", name);
            }
            for arg in args {
                self.reject_kind_across(self.kind_of_expr(arg, scopes), "spawn", "");
            }
        }
        self.walk_idents_across("spawn", inner, scopes);
    }

    fn reject_callee_borrow_params(&mut self, boundary: &str, name: &str) {
        let Some(params) = self.lookup_func_params(name) else {
            return;
        };
        for p in params {
            let kind = match p.borrow {
                Some(ParamBorrow::View) => TaskBoundaryKind::View,
                Some(ParamBorrow::Mutate) => TaskBoundaryKind::Mutate,
                None => Self::kind_of_type(&p.ty),
            };
            if !kind.may_cross_task_boundary() {
                self.reject_kind_across(
                    kind,
                    boundary,
                    &format!(": callee '{}' takes a {} parameter", name, kind.label()),
                );
                return;
            }
        }
    }

    /// Channel element / payload: Channel<T> and channel_send values.
    pub(crate) fn reject_narrow_channel_element(&mut self, ty: &Type) {
        self.reject_kind_across(Self::kind_of_type(ty), "Channel", "");
    }

    pub(crate) fn reject_narrow_across_channel_send(
        &mut self,
        value: &Expr,
        scopes: &[HashMap<String, Type>],
    ) {
        self.reject_kind_across(self.kind_of_expr(value, scopes), "Channel", "");
    }

    /// Actor mailbox: method parameters and arguments travel as messages.
    pub(crate) fn reject_narrow_mailbox_params(
        &mut self,
        params: &[Param],
        actor: &str,
        method: &str,
    ) {
        for p in params {
            if p.name == "self" {
                continue;
            }
            let kind = match p.borrow {
                Some(ParamBorrow::View) => TaskBoundaryKind::View,
                Some(ParamBorrow::Mutate) => TaskBoundaryKind::Mutate,
                None => Self::kind_of_type(&p.ty),
            };
            if !kind.may_cross_task_boundary() {
                self.reject_kind_across(
                    kind,
                    "mailbox",
                    &format!(
                        ": actor '{}::{}' parameter '{}' is {}",
                        actor,
                        method,
                        p.name,
                        kind.label()
                    ),
                );
            }
        }
    }

    pub(crate) fn reject_narrow_across_mailbox_arg(
        &mut self,
        arg: &Expr,
        scopes: &[HashMap<String, Type>],
    ) {
        self.reject_kind_across(self.kind_of_expr(arg, scopes), "mailbox", "");
    }

    fn walk_idents_across(
        &mut self,
        boundary: &str,
        expr: &Expr,
        scopes: &[HashMap<String, Type>],
    ) {
        match expr.unlocated() {
            Expr::Ident(name) => {
                self.reject_kind_across(self.kind_of_ident(name, scopes), boundary, "");
            }
            Expr::Unary(_, inner)
            | Expr::Field(inner, _)
            | Expr::Await(inner)
            | Expr::Spawn(inner)
            | Expr::Try(inner)
            | Expr::Cast(inner, _)
            | Expr::Old(inner)
            | Expr::TypeOf(inner)
            | Expr::TupleIndex(inner, _)
            | Expr::OptionalChain(inner, _) => {
                self.walk_idents_across(boundary, inner, scopes);
            }
            Expr::Binary(_, l, r) | Expr::Index(l, r) => {
                self.walk_idents_across(boundary, l, scopes);
                self.walk_idents_across(boundary, r, scopes);
            }
            Expr::Call(callee, args) => {
                self.walk_idents_across(boundary, callee, scopes);
                for arg in args {
                    self.walk_idents_across(boundary, arg, scopes);
                }
            }
            Expr::Tuple(elems) | Expr::List(elems) => {
                for e in elems {
                    self.walk_idents_across(boundary, e, scopes);
                }
            }
            Expr::Block(block) | Expr::Arena(block) => {
                for stmt in block {
                    if let crate::ast::Stmt::Expr(e) = stmt.unlocated() {
                        self.walk_idents_across(boundary, e, scopes);
                    }
                }
            }
            Expr::If { cond, then_, else_ } => {
                self.walk_idents_across(boundary, cond, scopes);
                for stmt in then_ {
                    if let crate::ast::Stmt::Expr(e) = stmt.unlocated() {
                        self.walk_idents_across(boundary, e, scopes);
                    }
                }
                if let Some(els) = else_ {
                    for stmt in els {
                        if let crate::ast::Stmt::Expr(e) = stmt.unlocated() {
                            self.walk_idents_across(boundary, e, scopes);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
