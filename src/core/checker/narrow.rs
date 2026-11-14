//! Phase A (0.1.8): view / mutate / ref may not cross a task boundary.
//!
//! Single predicate: `TaskBoundaryKind::may_cross_task_boundary`.
//! Default deny for `view`, `mutate`, `&T` / `&mut T` entering spawn,
//! Channel elements, Future captures, and actor mailboxes.
//! Synchronous `func` parameters (the DSP mutate hot path) are not a
//! task boundary and stay legal.

use crate::ast::{Expr, Item, Param, ParamBorrow, Type, UnOp};
use crate::core::helpers::subst_type_params;
use crate::diagnostic::codes;
use std::collections::{HashMap, HashSet};

use super::Checker;

/// Permission a value carries when it is about to leave the current task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskBoundaryKind {
    Owned,
    View,
    Mutate,
    Ref,
    /// A value whose ownership is linear, but whose transfer is explicit.
    /// The task boundary itself is allowed to move it; the canonical resource
    /// ledger still enforces exactly-once consumption at the call site.
    TransferOnly,
    /// A value tied to the creating task/thread (for example MutexGuard).
    ThreadAffine,
}

impl TaskBoundaryKind {
    /// Owned and transfer-only values may cross. Borrowed values and
    /// thread-affine guards stay task-local. The transfer-only distinction is
    /// intentional: ownership is checked by the canonical resource ledger,
    /// while this predicate only rejects aliases that would retain a parent
    /// frame or thread-local guard.
    pub(crate) fn may_cross_task_boundary(self) -> bool {
        matches!(self, Self::Owned | Self::TransferOnly)
    }

    /// Join nested fields. A borrow hidden anywhere in a product must remain a
    /// borrow at the boundary; a thread-affine guard is even more restrictive.
    /// Transfer-only is weaker than a borrow because an owned linear value may
    /// be moved into the task (the ownership ledger records that move).
    fn join(self, other: Self) -> Self {
        use TaskBoundaryKind::{Mutate, Ref, ThreadAffine, TransferOnly, View};
        let rank = |kind| match kind {
            ThreadAffine => 6,
            Mutate => 5,
            View => 4,
            Ref => 3,
            TransferOnly => 2,
            Self::Owned => 1,
        };
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::View => "view",
            Self::Mutate => "mutate",
            Self::Ref => "ref",
            Self::TransferOnly => "transfer-only",
            Self::ThreadAffine => "thread-affine",
        }
    }
}

impl<'a> Checker<'a> {
    /// Classify the complete surface type before it enters a task boundary.
    ///
    /// The old classifier only looked through references and a few wrappers;
    /// named products therefore hid `&T`/`&mut T`, and a generic record could
    /// silently turn a borrowed field into an owned task capture. Follow
    /// aliases, newtypes, generic record/enum payloads, and all ordinary
    /// containers recursively. This is deliberately independent from the
    /// linear resource analysis: a `TransferOnly` value may cross when moved,
    /// while the canonical ownership ledger proves that the source is consumed
    /// exactly once.
    pub(crate) fn kind_of_type(&self, ty: &Type) -> TaskBoundaryKind {
        self.kind_of_type_inner(ty, &HashMap::new(), &mut HashSet::new())
    }

    fn kind_of_type_inner(
        &self,
        ty: &Type,
        substitutions: &HashMap<String, Type>,
        visiting: &mut HashSet<String>,
    ) -> TaskBoundaryKind {
        match ty.unlocated() {
            Type::Located { ty, .. } => self.kind_of_type_inner(ty, substitutions, visiting),
            // Generic record fields are stored as `Type::Name("T", [])` until
            // the declaration is instantiated. Resolve a binder before the
            // fallback `Owned` case.
            Type::Name(name, args) if args.is_empty() => {
                if let Some(substituted) = substitutions.get(name) {
                    return self.kind_of_type_inner(substituted, substitutions, visiting);
                }
                self.kind_of_named_type(name, args, substitutions, visiting)
            }
            Type::Name(name, args) => {
                let mut kind = args.iter().fold(TaskBoundaryKind::Owned, |acc, arg| {
                    acc.join(self.kind_of_type_inner(arg, substitutions, visiting))
                });
                if let Some(substituted) = substitutions.get(name) {
                    kind = kind.join(self.kind_of_type_inner(substituted, substitutions, visiting));
                } else {
                    kind = kind.join(self.kind_of_named_type(name, args, substitutions, visiting));
                }
                kind
            }
            Type::Ref(_, _) | Type::RefMut(_, _) | Type::Slice(_) => TaskBoundaryKind::Ref,
            Type::Option(inner)
            | Type::Shared(inner)
            | Type::Weak(inner)
            | Type::CBuffer(inner)
            | Type::Newtype(_, inner)
            | Type::Array(inner, _) => self.kind_of_type_inner(inner, substitutions, visiting),
            // Raw C pointers are explicitly unsafe, non-borrowing values. A
            // pointee reference does not retain a Mimi frame through this ABI
            // handle, so retain the historical owned classification.
            Type::RawPtr(_) | Type::RawPtrMut(_) => TaskBoundaryKind::Owned,
            Type::Result(ok, err) => self
                .kind_of_type_inner(ok, substitutions, visiting)
                .join(self.kind_of_type_inner(err, substitutions, visiting)),
            Type::Tuple(items) => items.iter().fold(TaskBoundaryKind::Owned, |acc, item| {
                acc.join(self.kind_of_type_inner(item, substitutions, visiting))
            }),
            Type::Func(args, ret) | Type::ExternFunc(args, ret) => args.iter().fold(
                self.kind_of_type_inner(ret, substitutions, visiting),
                |acc, arg| acc.join(self.kind_of_type_inner(arg, substitutions, visiting)),
            ),
            Type::Cap(_) | Type::CapAtom(_) => TaskBoundaryKind::TransferOnly,
            // An opaque callable/trait object does not expose a source-frame
            // borrow here. Closure capture checking has its own precise rule;
            // retain the historical owned classification for this boundary.
            Type::ImplTrait(_) | Type::DynTrait(_) => TaskBoundaryKind::Owned,
            Type::Nothing | Type::Infer | Type::TypeVar(_) | Type::ForAll(_, _) | Type::TyErr => {
                TaskBoundaryKind::Owned
            }
        }
    }

    fn kind_of_named_type(
        &self,
        name: &str,
        args: &[Type],
        substitutions: &HashMap<String, Type>,
        visiting: &mut HashSet<String>,
    ) -> TaskBoundaryKind {
        if self.flow_state_type_names.contains(name)
            || self.declared_caps.contains(name)
            || name == "SystemToken"
            || name == "TokenChannel"
            || name == "SessionChan"
            || name == "session_chan"
        {
            return TaskBoundaryKind::TransferOnly;
        }
        if name == "MutexGuard" {
            return TaskBoundaryKind::ThreadAffine;
        }

        // Type aliases and newtypes carry their inner boundary obligations.
        if let Some(alias) = self.aliases.get(name) {
            let body = self.instantiate_named_body(name, alias, args);
            return self.kind_of_type_inner(&body, substitutions, visiting);
        }
        if let Some(inner) = self.newtypes.get(name) {
            let body = self.instantiate_named_body(name, inner, args);
            return self.kind_of_type_inner(&body, substitutions, visiting);
        }

        let Some(type_def) = self.types.get(name) else {
            return TaskBoundaryKind::Owned;
        };
        // Recursive records are legal. Re-entering the same nominal while
        // walking its fields contributes no additional boundary restriction;
        // any concrete argument restrictions were already joined at the use
        // site above.
        if !visiting.insert(name.to_owned()) {
            return TaskBoundaryKind::Owned;
        }

        let mut local_substitutions = substitutions.clone();
        if let Some(generics) = self.type_generics.get(name) {
            for (generic, arg) in generics.iter().zip(args.iter()) {
                local_substitutions.insert(generic.name.clone(), arg.clone());
            }
        }
        let mut kind = TaskBoundaryKind::Owned;
        match &type_def.kind {
            crate::ast::TypeDefKind::Alias(inner) | crate::ast::TypeDefKind::Newtype(inner) => {
                let body = self.instantiate_named_body(name, inner, args);
                kind = kind.join(self.kind_of_type_inner(&body, &local_substitutions, visiting));
            }
            crate::ast::TypeDefKind::Record(fields) | crate::ast::TypeDefKind::Union(fields) => {
                for field in fields {
                    kind = kind.join(self.kind_of_type_inner(
                        &field.ty,
                        &local_substitutions,
                        visiting,
                    ));
                }
            }
            crate::ast::TypeDefKind::Enum(variants) => {
                for variant in variants {
                    if let Some(payload) = &variant.payload {
                        match payload {
                            crate::ast::VariantPayload::Tuple(items) => {
                                for item in items {
                                    kind = kind.join(self.kind_of_type_inner(
                                        item,
                                        &local_substitutions,
                                        visiting,
                                    ));
                                }
                            }
                            crate::ast::VariantPayload::Record(fields) => {
                                for field in fields {
                                    kind = kind.join(self.kind_of_type_inner(
                                        &field.ty,
                                        &local_substitutions,
                                        visiting,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        visiting.remove(name);
        kind
    }

    fn instantiate_named_body(&self, name: &str, body: &Type, args: &[Type]) -> Type {
        let Some(generics) = self.type_generics.get(name) else {
            return body.clone();
        };
        if generics.len() != args.len() {
            return body.clone();
        }
        let type_map: HashMap<String, Type> = generics
            .iter()
            .zip(args.iter())
            .map(|(generic, arg)| (generic.name.clone(), arg.clone()))
            .collect();
        subst_type_params(body, generics, &type_map)
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
                return self.kind_of_type(ty);
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
                None => self.kind_of_type(&p.ty),
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
        self.reject_kind_across(self.kind_of_type(ty), "Channel", "");
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
                None => self.kind_of_type(&p.ty),
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
