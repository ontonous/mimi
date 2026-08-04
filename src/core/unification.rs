use crate::ast::Type;
use std::collections::{HashMap, HashSet};

/// Error produced when two types cannot be unified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifyError {
    Mismatch(String),
    OccurCheck(u32, String),
    Resolve(ResolveError),
}

impl std::fmt::Display for UnifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnifyError::Mismatch(msg) => write!(f, "type mismatch: {}", msg),
            UnifyError::OccurCheck(var, ty) => {
                write!(f, "infinite type: T{} occurs in {}", var, ty)
            }
            UnifyError::Resolve(error) => error.fmt(f),
        }
    }
}

impl From<ResolveError> for UnifyError {
    fn from(error: ResolveError) -> Self {
        Self::Resolve(error)
    }
}

#[derive(Debug, Clone)]
enum Undo {
    Parent { id: u32, previous: Option<u32> },
    Binding { id: u32, previous: Option<Type> },
    NextVar(u32),
}

/// Union-find based unification table for type inference variables.
///
/// Each TypeVar(u32) maps to either:
/// - Another TypeVar (parent link for union-find)
/// - A concrete Type (resolved binding)
///
/// Path compression ensures near-O(1) lookups after initial unions.
pub struct UnificationTable {
    /// Parent links: TypeVar(id) -> parent (another TypeVar or self if root)
    parent: HashMap<u32, u32>,
    /// Resolved bindings: root TypeVar -> concrete type (if resolved)
    binding: HashMap<u32, Type>,
    /// Next fresh type variable ID
    next_var: u32,
    /// Undo trail shared by nested transactions. Entries are retained until the
    /// outermost transaction commits so an outer failure can undo inner success.
    trail: Vec<Undo>,
    transaction_depth: usize,
}

impl Default for UnificationTable {
    fn default() -> Self {
        Self::new()
    }
}

impl UnificationTable {
    /// Reset the unification table for a new function check.
    pub fn reset(&mut self) {
        self.parent.clear();
        self.binding.clear();
        self.next_var = 0;
        self.trail.clear();
        self.transaction_depth = 0;
    }

    /// Find the root TypeVar ID for a given variable (with path compression).
    /// P1-1: Iterative to avoid stack overflow on adversarial parent chains.
    pub fn find(&mut self, id: u32) -> u32 {
        // Phase 1: walk to root.
        let mut current = id;
        loop {
            let parent = *self.parent.get(&current).unwrap_or(&current);
            if parent == current {
                break;
            }
            current = parent;
        }
        let root = current;
        // Phase 2: path compression — point every node on the path to root.
        let mut current = id;
        while current != root {
            let parent = *self.parent.get(&current).unwrap_or(&current);
            self.set_parent(current, root);
            current = parent;
        }
        root
    }

    /// Get the binding for a resolved TypeVar root, if any.
    pub fn get_binding(&self, root: u32) -> Option<&Type> {
        self.binding.get(&root)
    }

    pub fn new() -> Self {
        Self {
            parent: HashMap::new(),
            binding: HashMap::new(),
            next_var: 0,
            trail: Vec::new(),
            transaction_depth: 0,
        }
    }

    /// Allocate a fresh type variable.
    /// P2: Guard against u32 overflow on adversarial inputs.
    pub fn fresh_var(&mut self) -> u32 {
        let id = self.next_var;
        if self.transaction_depth > 0 {
            self.trail.push(Undo::NextVar(self.next_var));
        }
        self.next_var = self.next_var.checked_add(1).unwrap_or_else(|| {
            panic!("UnificationTable: type variable ID space exhausted (u32 overflow)")
        });
        self.set_parent(id, id);
        id
    }

    fn set_parent(&mut self, id: u32, parent: u32) {
        if self.parent.get(&id) == Some(&parent) {
            return;
        }
        if self.transaction_depth > 0 {
            self.trail.push(Undo::Parent {
                id,
                previous: self.parent.get(&id).copied(),
            });
        }
        self.parent.insert(id, parent);
    }

    fn set_binding(&mut self, id: u32, ty: Type) {
        if self.binding.get(&id) == Some(&ty) {
            return;
        }
        if self.transaction_depth > 0 {
            self.trail.push(Undo::Binding {
                id,
                previous: self.binding.get(&id).cloned(),
            });
        }
        self.binding.insert(id, ty);
    }

    fn rollback_to(&mut self, checkpoint: usize) {
        while self.trail.len() > checkpoint {
            match self.trail.pop().expect("trail length checked") {
                Undo::Parent { id, previous } => match previous {
                    Some(parent) => {
                        self.parent.insert(id, parent);
                    }
                    None => {
                        self.parent.remove(&id);
                    }
                },
                Undo::Binding { id, previous } => match previous {
                    Some(ty) => {
                        self.binding.insert(id, ty);
                    }
                    None => {
                        self.binding.remove(&id);
                    }
                },
                Undo::NextVar(previous) => self.next_var = previous,
            }
        }
    }

    /// Check if a type variable occurs inside a type (for occur check).
    /// Arch-7 fix: unified to only check TypeVar (integer ID space).
    /// Type::Name is a separate string-based namespace used for user-written type
    /// parameters; it does not interact with TypeVar (integer) ID space.
    /// Type::ForAll body uses TypeVar(i) for bound parameters, not Name(i).
    pub(crate) fn occurs_in(var: u32, ty: &Type) -> bool {
        crate::core::type_folder::type_any(ty, &|t| matches!(t, Type::TypeVar(id) if *id == var))
    }

    /// Resolve a type for inference while preserving unbound variables.
    ///
    /// Unlike the legacy `resolve` compatibility wrapper this operation is
    /// fail-closed for excessive nesting and binding cycles.
    // Keep this below the default Rust test-thread stack limit even when every
    // frame carries a large `Type` match. User-facing types this deep are
    // rejected structurally instead of risking a process abort.
    const MAX_RESOLVE_DEPTH: u32 = 64;
    pub fn resolve_infer(&mut self, ty: &Type) -> Result<Type, ResolveError> {
        self.resolve_with_depth(ty, 0, &mut HashSet::new())
    }

    /// Compatibility wrapper for inference code that has not yet been migrated
    /// to structured resolution errors. Mandatory finalization uses `zonk`, not
    /// this wrapper.
    ///
    /// T-6 (0.31.50): debug assertion fires when resolution fails silently.
    /// v0.34.9: release fallback tightened from "return the unresolved type"
    /// to "return Type::unknown" — a silently-returned unresolved TypeVar
    /// poisons downstream unification; unknown is at least visibly wrong and
    /// fails subsequent checked unification (unify/constrain reject escapes).
    /// Full migration of remaining call sites to `zonk` is tracked in the
    /// 0.1.5 DX backlog (golden-document §10 0.34.22 登记表).
    pub fn resolve(&mut self, ty: &Type) -> Type {
        match self.resolve_infer(ty) {
            Ok(resolved) => resolved,
            Err(e) => {
                mimi_debug_assert!(
                    false,
                    "resolve() silently swallowed resolution error: {} for type {} \
                     (call site not yet migrated to zonk — 0.1.5 backlog)",
                    e,
                    crate::core::helpers::fmt_type(ty)
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    /// 0.1.5 (DX backlog #1): finalization entrypoint that fails *visibly*.
    /// `zonk` additionally rejects residual `Infer` escapes that `resolve`
    /// silently passed through (scan_residual), so a migrated call site that
    /// hits an escape errs here instead of returning a poisonable type. The
    /// unknown fallback matches the pre-migration `resolve` failure behaviour;
    /// the debug assertion surfaces the escape during development so the call
    /// site can be given a precise handling. Migrated call sites use this
    /// instead of `resolve`.
    pub fn zonk_or_unknown(&mut self, ty: &Type) -> Type {
        match self.zonk(ty) {
            Ok(resolved) => resolved,
            Err(e) => {
                mimi_debug_assert!(
                    false,
                    "zonk() failed at a migrated call site: {} for type {} \
                     (residual Infer escape or resolution failure)",
                    e,
                    crate::core::helpers::fmt_type(ty)
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    fn resolve_with_depth(
        &mut self,
        ty: &Type,
        depth: u32,
        resolving: &mut HashSet<u32>,
    ) -> Result<Type, ResolveError> {
        if depth >= Self::MAX_RESOLVE_DEPTH {
            return Err(ResolveError::DepthOverflow(crate::core::helpers::fmt_type(
                ty,
            )));
        }
        let next = depth + 1;
        let resolved = match ty {
            Type::Located { meta, ty } => self
                .resolve_with_depth(ty, next, resolving)?
                .with_meta(*meta),
            Type::TypeVar(id) => {
                let root = self.find(*id);
                if let Some(bound) = self.binding.get(&root).cloned() {
                    if !resolving.insert(root) {
                        return Err(ResolveError::BindingCycle(root));
                    }
                    let resolved = self.resolve_with_depth(&bound, next, resolving)?;
                    resolving.remove(&root);
                    self.set_binding(root, resolved.clone());
                    resolved
                } else {
                    Type::TypeVar(root)
                }
            }
            Type::Option(inner) => {
                Type::Option(Box::new(self.resolve_with_depth(inner, next, resolving)?))
            }
            Type::Result(ok, err) => Type::Result(
                Box::new(self.resolve_with_depth(ok, next, resolving)?),
                Box::new(self.resolve_with_depth(err, next, resolving)?),
            ),
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|e| self.resolve_with_depth(e, next, resolving))
                    .collect::<Result<_, _>>()?,
            ),
            Type::Func(args, ret) => Type::Func(
                args.iter()
                    .map(|a| self.resolve_with_depth(a, next, resolving))
                    .collect::<Result<_, _>>()?,
                Box::new(self.resolve_with_depth(ret, next, resolving)?),
            ),
            Type::ExternFunc(args, ret) => Type::ExternFunc(
                args.iter()
                    .map(|a| self.resolve_with_depth(a, next, resolving))
                    .collect::<Result<_, _>>()?,
                Box::new(self.resolve_with_depth(ret, next, resolving)?),
            ),
            Type::Ref(lt, inner) => Type::Ref(
                lt.clone(),
                Box::new(self.resolve_with_depth(inner, next, resolving)?),
            ),
            Type::RefMut(lt, inner) => Type::RefMut(
                lt.clone(),
                Box::new(self.resolve_with_depth(inner, next, resolving)?),
            ),
            Type::Shared(inner) => {
                Type::Shared(Box::new(self.resolve_with_depth(inner, next, resolving)?))
            }
            Type::LocalShared(inner) => {
                Type::LocalShared(Box::new(self.resolve_with_depth(inner, next, resolving)?))
            }
            Type::Weak(inner) => {
                Type::Weak(Box::new(self.resolve_with_depth(inner, next, resolving)?))
            }
            Type::WeakLocal(inner) => {
                Type::WeakLocal(Box::new(self.resolve_with_depth(inner, next, resolving)?))
            }
            Type::RawPtr(inner) => {
                Type::RawPtr(Box::new(self.resolve_with_depth(inner, next, resolving)?))
            }
            Type::RawPtrMut(inner) => {
                Type::RawPtrMut(Box::new(self.resolve_with_depth(inner, next, resolving)?))
            }
            Type::CShared(inner) => {
                Type::CShared(Box::new(self.resolve_with_depth(inner, next, resolving)?))
            }
            Type::CBorrow(inner) => {
                Type::CBorrow(Box::new(self.resolve_with_depth(inner, next, resolving)?))
            }
            Type::CBorrowMut(inner) => {
                Type::CBorrowMut(Box::new(self.resolve_with_depth(inner, next, resolving)?))
            }
            Type::CBuffer(inner) => {
                Type::CBuffer(Box::new(self.resolve_with_depth(inner, next, resolving)?))
            }
            Type::Array(inner, n) => Type::Array(
                Box::new(self.resolve_with_depth(inner, next, resolving)?),
                *n,
            ),
            Type::Slice(inner) => {
                Type::Slice(Box::new(self.resolve_with_depth(inner, next, resolving)?))
            }
            Type::Newtype(name, inner) => Type::Newtype(
                name.clone(),
                Box::new(self.resolve_with_depth(inner, next, resolving)?),
            ),
            Type::Name(name, args) => Type::Name(
                name.clone(),
                args.iter()
                    .map(|a| self.resolve_with_depth(a, next, resolving))
                    .collect::<Result<_, _>>()?,
            ),
            // P0-1: ForAll bound variables use TypeVar(0..params.len()) which
            // share the same ID space as inference variables. Resolving inside
            // the ForAll body would incorrectly substitute bound variables with
            // stale bindings from the unification table (e.g., TypeVar(0) bound
            // to i32 from a previous inference corrupts ∀T. T→T into i32→i32).
            // ForAll types must only be processed by instantiate(), which remaps
            // bound variables to fresh inference variables before resolution.
            Type::ForAll(..) => ty.clone(),
            // Leaf types — no TypeVars inside
            Type::Infer
            | Type::Nothing
            | Type::Allocator
            | Type::RawString
            | Type::Cap(_)
            | Type::CapAtom(_)
            | Type::ImplTrait(_)
            | Type::DynTrait(_)
            | Type::TyErr => ty.clone(),
        };
        Ok(resolved)
    }

    /// Zonk a type: resolve all TypeVars and reject residual inference artifacts.
    /// Returns Err(ResolveError) if any TypeVar remains unresolved or if the
    /// type contains ForAll/Infer/`_` placeholders.
    pub fn zonk(&mut self, ty: &Type) -> Result<Type, ResolveError> {
        let resolved = self.resolve_infer(ty)?;
        scan_residual(&resolved)?;
        Ok(resolved)
    }

    /// Stable unification entrypoint. Escape placeholders are rejected here;
    /// callers at an explicit inference boundary must use `unify_inference`.
    pub fn unify(&mut self, a: &Type, b: &Type) -> Result<(), UnifyError> {
        self.constrain(a, b)
    }

    /// Canonical checked constraint operation. All mutations are atomic.
    pub fn constrain(&mut self, a: &Type, b: &Type) -> Result<(), UnifyError> {
        self.transaction(|table| {
            let a_resolved = table.resolve_infer(a)?;
            let b_resolved = table.resolve_infer(b)?;
            if is_escape_type(&a_resolved) || is_escape_type(&b_resolved) {
                return Err(UnifyError::Mismatch(format!(
                    "checked unification rejects escape types {} and {}",
                    crate::core::helpers::fmt_type(&a_resolved),
                    crate::core::helpers::fmt_type(&b_resolved)
                )));
            }
            table.unify_inference_inner(&a_resolved, &b_resolved)
        })
    }

    /// Permissive unification for explicit local inference boundaries only.
    pub fn unify_inference(&mut self, a: &Type, b: &Type) -> Result<(), UnifyError> {
        self.transaction(|table| table.unify_inference_inner(a, b))
    }

    /// Side-effect-free compatibility probe using the exact checked semantics.
    pub fn probe_compatible(&mut self, a: &Type, b: &Type) -> bool {
        let checkpoint = self.trail.len();
        self.transaction_depth += 1;
        let compatible = self.constrain(a, b).is_ok();
        self.transaction_depth -= 1;
        self.rollback_to(checkpoint);
        if self.transaction_depth == 0 {
            self.trail.clear();
        }
        compatible
    }

    fn transaction(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<(), UnifyError>,
    ) -> Result<(), UnifyError> {
        let checkpoint = self.trail.len();
        self.transaction_depth += 1;
        let result = operation(self);
        self.transaction_depth -= 1;
        let result = match result {
            Ok(()) => Ok(()),
            Err(error) => {
                self.rollback_to(checkpoint);
                Err(error)
            }
        };
        if self.transaction_depth == 0 {
            self.trail.clear();
        }
        result
    }

    fn unify_inference_inner(&mut self, a: &Type, b: &Type) -> Result<(), UnifyError> {
        let a_resolved = self.resolve_infer(a)?;
        let b_resolved = self.resolve_infer(b)?;

        match (a_resolved.unlocated(), b_resolved.unlocated()) {
            (Type::TypeVar(a), Type::TypeVar(b)) => {
                let a_root = self.find(*a);
                let b_root = self.find(*b);
                if a_root != b_root {
                    self.set_parent(a_root, b_root);
                }
                Ok(())
            }
            // TypeVar on either side — bind to the other
            (Type::TypeVar(id), _) => {
                if Self::occurs_in(*id, &b_resolved) {
                    return Err(UnifyError::OccurCheck(
                        *id,
                        crate::core::helpers::fmt_type(&b_resolved),
                    ));
                }
                let root = self.find(*id);
                self.set_binding(root, b_resolved.clone());
                Ok(())
            }
            (_, Type::TypeVar(id)) => {
                if Self::occurs_in(*id, &a_resolved) {
                    return Err(UnifyError::OccurCheck(
                        *id,
                        crate::core::helpers::fmt_type(&a_resolved),
                    ));
                }
                let root = self.find(*id);
                self.set_binding(root, a_resolved.clone());
                Ok(())
            }

            // CO-C2 (audit): Type escape hatches — `_` / `Any` / `Infer` unify with anything.
            // SAFETY: `_` is emitted by the parser when the user writes `let x: _ = ...`.
            //         Such bindings appear ONLY at let-init positions (check_stmt.rs:626)
            //         where the inferred init_ty substitutes for the declared type.
            //         v0.34.10: `Any` removed from user syntax (golden §2.4) — the branch
            //         below only serves internal artifact paths.
            // TODO(#v0.31-type-engine): restrict these to top-level inference boundaries
            //       and surface E0431 (type escape hatch leaked past its boundary) at
            //       function call/field access sites. E0710 is reserved for "extern
            //       function not declared" — do NOT reuse it for escape scenarios.
            (Type::Name(n, _), _) if n == "_" => Ok(()),
            (_, Type::Name(n, _)) if n == "_" => Ok(()),
            (Type::Name(n, _), _) if n == "Any" => Ok(()),
            (_, Type::Name(n, _)) if n == "Any" => Ok(()),

            // 0.31.42: TyErr poison type — unifies with everything.
            // Once a type error is detected, TyErr replaces the offending type
            // to suppress cascading downstream errors. TyErr must never reach
            // codegen (zonk phase rejects it).
            (Type::TyErr, _) | (_, Type::TyErr) => Ok(()),
            // L12: single-sided "unknown" must not unify (helpers already reject);
            // both-unknown is allowed as a cascade placeholder only.
            (Type::Name(na, _), Type::Name(nb, _)) if na == "unknown" && nb != "unknown" => Err(
                UnifyError::Mismatch(format!("cannot unify unknown with {}", nb)),
            ),
            (Type::Name(na, _), Type::Name(nb, _)) if nb == "unknown" && na != "unknown" => Err(
                UnifyError::Mismatch(format!("cannot unify {} with unknown", na)),
            ),
            // CO-C2 (audit): Type::Infer placeholder — legitimate inference variable binding.
            // SAFETY: only emitted by `parse_type_atom` for `_` (parse_type.rs:67) and
            //         threaded through let-bindings. Resolved by substitution at the
            //         let-binding site (check_stmt.rs:626-628).
            (Type::Infer, _) | (_, Type::Infer) => Ok(()),

            // Dual representation normalization: Name("Option", [T]) <-> Option(T)
            (Type::Name(n, args), Type::Option(inner)) if n == "Option" && args.len() == 1 => {
                self.unify_inference_inner(&args[0], inner)
            }
            (Type::Option(inner), Type::Name(n, args)) if n == "Option" && args.len() == 1 => {
                self.unify_inference_inner(inner, &args[0])
            }
            (Type::Name(n, args), Type::Result(ok, err)) if n == "Result" && args.len() == 2 => {
                self.unify_inference_inner(&args[0], ok)?;
                self.unify_inference_inner(&args[1], err)
            }
            (Type::Result(ok, err), Type::Name(n, args)) if n == "Result" && args.len() == 2 => {
                self.unify_inference_inner(ok, &args[0])?;
                self.unify_inference_inner(err, &args[1])
            }
            // C3 (audit 2026-08-03): bare container annotations (`List`/`Set`/
            // `Map` without arguments, as spelled in std/set.mimi signatures)
            // accept any element parameterization. The stdlib set API is
            // heterogeneous (`insert(value: Any)`), so `Set<i32>` from a set
            // literal must flow into `Set` parameters without an E0211.
            (Type::Name(na, aa), Type::Name(nb, ab))
                if na == nb
                    && (aa.is_empty() || ab.is_empty())
                    && matches!(na.as_str(), "List" | "Set" | "Map") =>
            {
                Ok(())
            }
            // Same constructors — unify structurally
            (Type::Name(na, aa), Type::Name(nb, ab)) if na == nb && aa.len() == ab.len() => {
                for (a, b) in aa.iter().zip(ab.iter()) {
                    self.unify_inference_inner(a, b)?;
                }
                Ok(())
            }
            (Type::Option(a), Type::Option(b)) => self.unify_inference_inner(a, b),
            (Type::Result(a1, a2), Type::Result(b1, b2)) => {
                self.unify_inference_inner(a1, b1)?;
                self.unify_inference_inner(a2, b2)
            }
            (Type::Tuple(a), Type::Tuple(b)) if a.len() == b.len() => {
                for (a, b) in a.iter().zip(b.iter()) {
                    self.unify_inference_inner(a, b)?;
                }
                Ok(())
            }
            (Type::Func(a_args, a_ret), Type::Func(b_args, b_ret))
            | (Type::ExternFunc(a_args, a_ret), Type::ExternFunc(b_args, b_ret)) => {
                if a_args.len() != b_args.len() {
                    return Err(UnifyError::Mismatch(format!(
                        "function arity mismatch: {} vs {}",
                        a_args.len(),
                        b_args.len()
                    )));
                }
                for (a, b) in a_args.iter().zip(b_args.iter()) {
                    self.unify_inference_inner(a, b)?;
                }
                self.unify_inference_inner(a_ret, b_ret)
            }
            (Type::Ref(_, a), Type::Ref(_, b)) => self.unify_inference_inner(a, b),
            (Type::RefMut(_, a), Type::RefMut(_, b)) => self.unify_inference_inner(a, b),
            (Type::Shared(a), Type::Shared(b)) => self.unify_inference_inner(a, b),
            (Type::LocalShared(a), Type::LocalShared(b)) => self.unify_inference_inner(a, b),
            (Type::Weak(a), Type::Weak(b)) => self.unify_inference_inner(a, b),
            (Type::WeakLocal(a), Type::WeakLocal(b)) => self.unify_inference_inner(a, b),
            (Type::RawPtr(a), Type::RawPtr(b)) => self.unify_inference_inner(a, b),
            (Type::RawPtrMut(a), Type::RawPtrMut(b)) => self.unify_inference_inner(a, b),
            (Type::CShared(a), Type::CShared(b)) => self.unify_inference_inner(a, b),
            (Type::CBorrow(a), Type::CBorrow(b)) => self.unify_inference_inner(a, b),
            (Type::CBorrowMut(a), Type::CBorrowMut(b)) => self.unify_inference_inner(a, b),
            (Type::CBuffer(a), Type::CBuffer(b)) => self.unify_inference_inner(a, b),
            (Type::Slice(a), Type::Slice(b)) => self.unify_inference_inner(a, b),
            (Type::Array(a, na), Type::Array(b, nb)) if na == nb => {
                self.unify_inference_inner(a, b)
            }
            (Type::Newtype(na, a), Type::Newtype(nb, b)) if na == nb => {
                self.unify_inference_inner(a, b)
            }
            // CO-H3 (audit): Newtype is transparent — unify with inner type.
            // Guard prevents cross-newtype unification: Newtype("A",_) vs Newtype("B",_)
            // only succeeds if inner of A matches B's same-name case in the recursive call.
            //
            // v0.34.7 (golden §2.2): 1.0 keeps transparency (zero-cost
            // interop, orchestration-core positioning). The lint mitigation
            // claimed by the old comment (W012) does NOT exist — W012 is the
            // _/Any escape-hatch detector, unrelated to newtype. The
            // dedicated warning is W013 (lint module); strict-newtype opt-in
            // evaluated at 1.1 (not committed).
            (Type::Newtype(_, inner), other) if !matches!(other, Type::Newtype(..)) => {
                self.unify_inference_inner(inner, other)
            }
            (other, Type::Newtype(_, inner)) if !matches!(other, Type::Newtype(..)) => {
                self.unify_inference_inner(inner, other)
            }
            // Newtypes are distinct — different names don't unify (type safety)
            (Type::ImplTrait(a), Type::ImplTrait(b)) | (Type::DynTrait(a), Type::DynTrait(b)) => {
                if a == b {
                    Ok(())
                } else {
                    Err(UnifyError::Mismatch(format!(
                        "trait mismatch: {} vs {}",
                        a.join(", "),
                        b.join(", ")
                    )))
                }
            }

            // Literal/constant types
            (Type::Nothing, Type::Nothing)
            | (Type::Allocator, Type::Allocator)
            | (Type::RawString, Type::RawString) => Ok(()),
            (Type::Cap(a), Type::Cap(b))
            | (Type::CapAtom(a), Type::CapAtom(b))
            | (Type::CapAtom(a), Type::Cap(b))
            | (Type::Cap(a), Type::CapAtom(b)) => {
                if a == b {
                    Ok(())
                } else {
                    Err(UnifyError::Mismatch(format!(
                        "capability mismatch: {} vs {}",
                        a, b
                    )))
                }
            }

            // Mismatch
            _ => Err(UnifyError::Mismatch(format!(
                "cannot unify {} with {}",
                crate::core::helpers::fmt_type(&a_resolved),
                crate::core::helpers::fmt_type(&b_resolved),
            ))),
        }
    }
}

fn is_escape_type(ty: &Type) -> bool {
    // C3 (audit 2026-08-03): `Any` removed from this set. 0.34.10 (golden
    // §2.4) deleted `Any` from the user-facing grammar (builtin_type_names),
    // so the only producers left are internal artifacts: builtin signatures
    // (`map_get` returns `(bool, Any)`), stdlib wrappers (std/maps.mimi,
    // std/set.mimi) and unification's own Any arms (unify_inference_inner).
    // Keep rejecting `Any` here made `use std::maps` / `use std::set`
    // unusable — every call into the wrappers died on E0211 and the impl
    // signature parity check died on E0252, while the SAME operations through
    // the builtins (map_get/map_set) worked fine. The user-facing syntax
    // boundary (E0407) is the entry gate; the checked-unify rejection was the
    // 0.31-era exit gate for a user-writable escape hatch that no longer
    // exists. `_` / `Infer` / `unknown` stay rejected.
    crate::core::type_folder::type_any(ty, &|t| match t {
        Type::Infer => true,
        Type::ForAll(_, _) => true,
        Type::Name(name, _) => name == "_" || name == "unknown",
        _ => false,
    })
}

/// Error produced when a type cannot be fully resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    DepthOverflow(String),
    UnboundVar(u32),
    BindingCycle(u32),
    ResidualType(String),
    /// H4 (audit-type 2026-08-03): a user-written type escape hatch (`_` /
    /// `Infer` / `unknown`) survived to a finalization boundary instead of
    /// being substituted at its let-init site. Distinct from generic residual
    /// artifacts so the checker can surface E0431 instead of the generic E0200.
    EscapeHatch(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::DepthOverflow(msg) => write!(f, "resolve depth overflow: {}", msg),
            ResolveError::UnboundVar(id) => write!(f, "unbound type variable T{}", id),
            ResolveError::BindingCycle(id) => {
                write!(f, "cyclic binding for type variable T{}", id)
            }
            ResolveError::ResidualType(msg) => write!(f, "residual type: {}", msg),
            ResolveError::EscapeHatch(msg) => {
                write!(
                    f,
                    "type escape hatch leaked past inference boundary: {}",
                    msg
                )
            }
        }
    }
}

impl std::error::Error for ResolveError {}

/// Scan a type for residual inference artifacts (TypeVar, ForAll, Infer, `_`).
/// Returns Err(ResolveError) if any are found.
pub fn scan_residual(ty: &Type) -> Result<(), ResolveError> {
    crate::core::type_folder::type_try_visit(ty, &|t| match t {
        Type::TypeVar(id) => Err(ResolveError::UnboundVar(*id)),
        Type::ForAll(_, _) => Err(ResolveError::ResidualType(
            "unresolved ForAll quantifier".into(),
        )),
        Type::Infer => Err(ResolveError::EscapeHatch("Infer placeholder".into())),
        Type::Name(name, _) if name == "_" || name == "unknown" => Err(ResolveError::EscapeHatch(
            format!("non-final type name '{name}'"),
        )),
        Type::TyErr => Err(ResolveError::ResidualType(
            "TyErr poison type in finalized output".into(),
        )),
        _ => Ok(()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i32_ty() -> Type {
        Type::Name("i32".into(), vec![])
    }

    fn string_ty() -> Type {
        Type::Name("string".into(), vec![])
    }

    #[test]
    fn test_unify_same_types() {
        let mut table = UnificationTable::new();
        assert!(table.unify(&i32_ty(), &i32_ty()).is_ok());
    }

    #[test]
    fn test_unify_typevar_with_concrete() {
        let mut table = UnificationTable::new();
        let var = table.fresh_var();
        let var_ty = Type::TypeVar(var);
        assert!(table.unify(&var_ty, &i32_ty()).is_ok());
        let resolved = table.resolve(&var_ty);
        assert_eq!(resolved, i32_ty());
    }

    #[test]
    fn test_unify_two_typevars() {
        let mut table = UnificationTable::new();
        let v1 = table.fresh_var();
        let v2 = table.fresh_var();
        assert!(table.unify(&Type::TypeVar(v1), &Type::TypeVar(v2)).is_ok());
        assert!(table.unify(&Type::TypeVar(v1), &i32_ty()).is_ok());
        let resolved = table.resolve(&Type::TypeVar(v2));
        assert_eq!(resolved, i32_ty());
    }

    #[test]
    fn typevar_unifies_with_itself() {
        let mut table = UnificationTable::new();
        let var = table.fresh_var();

        assert!(table
            .unify(&Type::TypeVar(var), &Type::TypeVar(var))
            .is_ok());
        assert_eq!(table.resolve(&Type::TypeVar(var)), Type::TypeVar(var));
    }

    #[test]
    fn failed_unification_rolls_back_partial_bindings() {
        let mut table = UnificationTable::new();
        let var = table.fresh_var();
        let left = Type::Tuple(vec![Type::TypeVar(var), i32_ty()]);
        let right = Type::Tuple(vec![string_ty(), Type::Name("bool".into(), vec![])]);

        assert!(table.unify(&left, &right).is_err());
        assert_eq!(table.resolve(&Type::TypeVar(var)), Type::TypeVar(var));
    }

    #[test]
    fn failed_nested_transaction_rolls_back_fresh_variables() {
        let mut table = UnificationTable::new();
        let result = table.transaction(|outer| {
            let first = outer.fresh_var();
            assert_eq!(first, 0);
            outer.transaction(|inner| {
                let second = inner.fresh_var();
                assert_eq!(second, 1);
                Ok(())
            })?;
            Err(UnifyError::Mismatch("force outer rollback".into()))
        });

        assert!(result.is_err());
        assert_eq!(table.fresh_var(), 0);
    }

    #[test]
    fn compatibility_probe_never_commits_bindings() {
        let mut table = UnificationTable::new();
        let var = table.fresh_var();

        assert!(table.probe_compatible(&Type::TypeVar(var), &i32_ty()));
        assert_eq!(
            table.resolve_infer(&Type::TypeVar(var)).unwrap(),
            Type::TypeVar(var)
        );
        assert!(!table.probe_compatible(&i32_ty(), &string_ty()));
        assert_eq!(
            table.resolve_infer(&Type::TypeVar(var)).unwrap(),
            Type::TypeVar(var)
        );
    }

    #[test]
    fn fallible_resolution_rejects_binding_cycles() {
        let mut table = UnificationTable::new();
        let var = table.fresh_var();
        table.binding.insert(var, Type::TypeVar(var));

        assert_eq!(
            table.resolve_infer(&Type::TypeVar(var)),
            Err(ResolveError::BindingCycle(var))
        );
    }

    #[test]
    fn fallible_resolution_rejects_excessive_nesting() {
        let mut table = UnificationTable::new();
        let mut ty = i32_ty();
        for _ in 0..=UnificationTable::MAX_RESOLVE_DEPTH {
            ty = Type::Option(Box::new(ty));
        }

        assert!(matches!(
            table.resolve_infer(&ty),
            Err(ResolveError::DepthOverflow(_))
        ));
    }

    #[test]
    fn test_unify_nested() {
        let mut table = UnificationTable::new();
        let v = table.fresh_var();
        let opt_var = Type::Option(Box::new(Type::TypeVar(v)));
        let opt_i32 = Type::Option(Box::new(i32_ty()));
        assert!(table.unify(&opt_var, &opt_i32).is_ok());
        assert_eq!(table.resolve(&Type::TypeVar(v)), i32_ty());
    }

    #[test]
    fn test_unify_mismatch() {
        let mut table = UnificationTable::new();
        assert!(table.unify(&i32_ty(), &string_ty()).is_err());
    }

    #[test]
    fn test_occurs_check() {
        let mut table = UnificationTable::new();
        let v = table.fresh_var();
        let var_ty = Type::TypeVar(v);
        let recursive = Type::Option(Box::new(Type::TypeVar(v)));
        assert!(matches!(
            table.unify(&var_ty, &recursive),
            Err(UnifyError::OccurCheck(_, _))
        ));
    }

    #[test]
    fn checked_unify_rejects_wildcard_placeholder() {
        let mut table = UnificationTable::new();
        let wildcard = Type::Name("_".into(), vec![]);
        assert!(table.unify(&wildcard, &i32_ty()).is_err());
        assert!(table.unify_inference(&wildcard, &i32_ty()).is_ok());
    }

    #[test]
    fn checked_unify_allows_nested_any() {
        // C3 (audit 2026-08-03): nested `Any` inside containers unifies on
        // the checked path too — the stdlib wrappers (`Option<Any>`,
        // `List<Any>` in maps/set signatures) must lower through resolved
        // codegen, and the strict path is now the only path they take.
        let mut table = UnificationTable::new();
        let option_any = Type::Option(Box::new(Type::Name("Any".into(), vec![])));
        let option_i32 = Type::Option(Box::new(i32_ty()));
        assert!(table.unify(&option_any, &option_i32).is_ok());
        assert!(table.unify_inference(&option_any, &option_i32).is_ok());
    }

    #[test]
    fn unify_is_symmetric() {
        let mut table = UnificationTable::new();
        let v = table.fresh_var();
        let var_ty = Type::TypeVar(v);
        assert_eq!(table.unify(&var_ty, &i32_ty()).is_ok(), {
            let mut t2 = UnificationTable::new();
            t2.unify(&i32_ty(), &Type::TypeVar(v)).is_ok()
        });
    }

    #[test]
    fn unify_transitivity_propagates_bindings() {
        let mut table = UnificationTable::new();
        let a = table.fresh_var();
        let b = table.fresh_var();
        assert!(table.unify(&Type::TypeVar(a), &Type::TypeVar(b)).is_ok());
        assert!(table.unify(&Type::TypeVar(a), &i32_ty()).is_ok());
        assert_eq!(table.resolve(&Type::TypeVar(b)), i32_ty());
    }

    #[test]
    fn checked_never_allows_escape_on_either_side() {
        let mut table = UnificationTable::new();
        // C3 (audit 2026-08-03): `Any` is no longer an escape type — it is
        // the internal polymorphic marker for builtin/stdlib heterogeneous
        // values (map_get returns `(bool, Any)`), and 0.34.10 (golden §2.4)
        // removed it from user syntax (E0407 gates the boundary). Checked
        // unify therefore ACCEPTS Any↔concrete; `_` / `Infer` stay rejected.
        let any = Type::Name("Any".into(), vec![]);
        let infer = Type::Infer;
        let underscore = Type::Name("_".into(), vec![]);
        assert!(table.unify(&any, &i32_ty()).is_ok());
        assert!(table.unify(&i32_ty(), &any).is_ok());
        for escape in [&infer, &underscore] {
            assert!(table.unify(escape, &i32_ty()).is_err());
            assert!(table.unify(&i32_ty(), escape).is_err());
        }
    }

    #[test]
    fn resolve_is_idempotent_on_concrete_types() {
        let mut table = UnificationTable::new();
        let ty = Type::Option(Box::new(i32_ty()));
        assert_eq!(table.resolve(&ty), ty);
        assert_eq!(table.resolve(&ty), ty);
    }

    #[test]
    fn path_compression_does_not_lose_binding() {
        let mut table = UnificationTable::new();
        let a = table.fresh_var();
        let b = table.fresh_var();
        let c = table.fresh_var();
        table.unify(&Type::TypeVar(a), &Type::TypeVar(b)).unwrap();
        table.unify(&Type::TypeVar(b), &Type::TypeVar(c)).unwrap();
        table.unify(&Type::TypeVar(c), &i32_ty()).unwrap();
        assert_eq!(table.resolve(&Type::TypeVar(a)), i32_ty());
        assert_eq!(table.resolve(&Type::TypeVar(b)), i32_ty());
        assert_eq!(table.resolve(&Type::TypeVar(c)), i32_ty());
    }

    #[test]
    fn zonk_preserves_extern_function_constructor() {
        let mut table = UnificationTable::new();
        let var = table.fresh_var();
        table.unify(&Type::TypeVar(var), &i32_ty()).unwrap();
        let extern_func = Type::ExternFunc(vec![Type::TypeVar(var)], Box::new(i32_ty()));

        assert_eq!(
            table.zonk(&extern_func).unwrap(),
            Type::ExternFunc(vec![i32_ty()], Box::new(i32_ty()))
        );
    }

    #[test]
    fn zonk_rejects_unknown_cascade_placeholder() {
        let mut table = UnificationTable::new();
        // H4: `unknown` is an escape hatch, classified distinctly from generic
        // residual artifacts.
        assert!(matches!(
            table.zonk(&Type::Name("unknown".into(), vec![])),
            Err(ResolveError::EscapeHatch(_))
        ));
    }

    #[test]
    fn scan_residual_classifies_escape_hatches() {
        // H4 (audit-type): `_` / Infer / unknown residuals surface as
        // EscapeHatch so the checker emits E0431; TypeVar/ForAll/TyErr stay
        // generic residuals (E0200).
        assert!(matches!(
            scan_residual(&Type::Infer),
            Err(ResolveError::EscapeHatch(_))
        ));
        assert!(matches!(
            scan_residual(&Type::Name("_".into(), vec![])),
            Err(ResolveError::EscapeHatch(_))
        ));
        assert!(matches!(
            scan_residual(&Type::TyErr),
            Err(ResolveError::ResidualType(_))
        ));
        assert!(scan_residual(&i32_ty()).is_ok());
    }

    // ── 0.31.42: TyErr poison type tests ──

    #[test]
    fn tyerr_unifies_with_any_type() {
        let mut table = UnificationTable::new();
        // TyErr unifies with primitives
        assert!(table.unify(&Type::TyErr, &i32_ty()).is_ok());
        assert!(table.unify(&string_ty(), &Type::TyErr).is_ok());
        // TyErr unifies with complex types
        assert!(table
            .unify(&Type::TyErr, &Type::Option(Box::new(i32_ty())))
            .is_ok());
        // TyErr unifies with TyErr
        assert!(table.unify(&Type::TyErr, &Type::TyErr).is_ok());
    }

    #[test]
    fn tyerr_unifies_with_typevar() {
        let mut table = UnificationTable::new();
        let var = table.fresh_var();
        // TyErr binds to TypeVar
        assert!(table.unify(&Type::TypeVar(var), &Type::TyErr).is_ok());
        // Resolving the var gives TyErr
        assert_eq!(table.resolve(&Type::TypeVar(var)), Type::TyErr);
    }

    #[test]
    fn zonk_rejects_tyerr() {
        let mut table = UnificationTable::new();
        // TyErr must not survive zonk (invariant: never reaches codegen)
        assert!(matches!(
            table.zonk(&Type::TyErr),
            Err(ResolveError::ResidualType(_))
        ));
    }

    #[test]
    fn zonk_rejects_nested_tyerr() {
        let mut table = UnificationTable::new();
        // TyErr nested inside Option must also be rejected
        let nested = Type::Option(Box::new(Type::TyErr));
        assert!(matches!(
            table.zonk(&nested),
            Err(ResolveError::ResidualType(_))
        ));
    }

    #[test]
    fn tyerr_displays_as_error() {
        assert_eq!(crate::core::helpers::fmt_type(&Type::TyErr), "«error»");
    }

    #[test]
    fn tyerr_equality() {
        assert_eq!(Type::TyErr, Type::TyErr);
        assert_ne!(Type::TyErr, Type::Nothing);
        assert_ne!(Type::TyErr, i32_ty());
    }

    #[test]
    fn tyerr_is_infer_artifact() {
        assert!(crate::core::phase::contains_infer_artifacts(&Type::TyErr));
        assert!(crate::core::phase::contains_infer_artifacts(&Type::Option(
            Box::new(Type::TyErr)
        )));
    }
}
