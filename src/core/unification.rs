use crate::ast::Type;
use std::collections::HashMap;

/// Error produced when two types cannot be unified.
#[derive(Debug, Clone)]
pub enum UnifyError {
    Mismatch(String),
    OccurCheck(u32, String),
}

impl std::fmt::Display for UnifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnifyError::Mismatch(msg) => write!(f, "type mismatch: {}", msg),
            UnifyError::OccurCheck(var, ty) => {
                write!(f, "infinite type: T{} occurs in {}", var, ty)
            }
        }
    }
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
}

impl UnificationTable {
    /// Reset the unification table for a new function check.
    pub fn reset(&mut self) {
        self.parent.clear();
        self.binding.clear();
        self.next_var = 0;
    }

    /// Find the root TypeVar ID for a given variable (with path compression).
    pub fn find(&mut self, id: u32) -> u32 {
        let parent = *self.parent.get(&id).unwrap_or(&id);
        if parent == id {
            id
        } else {
            let root = self.find(parent);
            self.parent.insert(id, root);
            root
        }
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
        }
    }

    /// Allocate a fresh type variable.
    pub fn fresh_var(&mut self) -> u32 {
        let id = self.next_var;
        self.next_var += 1;
        self.parent.insert(id, id);
        id
    }

    /// Check if a type variable occurs inside a type (for occur check).
    /// Arch-7 fix: unified to only check TypeVar (integer ID space).
    /// Type::Name is a separate string-based namespace used for user-written type
    /// parameters; it does not interact with TypeVar (integer) ID space.
    /// Type::ForAll body uses TypeVar(i) for bound parameters, not Name(i).
    pub(crate) fn occurs_in(var: u32, ty: &Type) -> bool {
        match ty {
            Type::TypeVar(id) => *id == var,
            Type::ForAll(_, body) => Self::occurs_in(var, body),
            Type::Option(inner) => Self::occurs_in(var, inner),
            Type::Result(ok, err) => Self::occurs_in(var, ok) || Self::occurs_in(var, err),
            Type::Tuple(elems) => elems.iter().any(|e| Self::occurs_in(var, e)),
            Type::Func(args, ret) | Type::ExternFunc(args, ret) => {
                args.iter().any(|a| Self::occurs_in(var, a)) || Self::occurs_in(var, ret)
            }
            Type::Ref(_, inner)
            | Type::RefMut(_, inner)
            | Type::Shared(inner)
            | Type::LocalShared(inner)
            | Type::Weak(inner)
            | Type::WeakLocal(inner)
            | Type::RawPtr(inner)
            | Type::RawPtrMut(inner)
            | Type::CShared(inner)
            | Type::CBorrow(inner)
            | Type::CBorrowMut(inner)
            | Type::CBuffer(inner)
            | Type::Array(inner, _)
            | Type::Slice(inner) => Self::occurs_in(var, inner),
            Type::Newtype(_, inner) => Self::occurs_in(var, inner),
            // Type::Name is string-based; TypeVar is integer-based — no cross-check needed.
            // ForAll params are stored as strings in ForAll but referenced as TypeVar(i)
            // in the body after remap. instantiate() handles TypeVar substitution correctly.
            Type::Name(_, args) => args.iter().any(|a| Self::occurs_in(var, a)),
            Type::Infer
            | Type::Nothing
            | Type::Allocator
            | Type::RawString
            | Type::Cap(_)
            | Type::ImplTrait(_)
            | Type::DynTrait(_) => false,
        }
    }

    /// Resolve a type: replace all TypeVars with their bindings.
    /// Arch-6 fix: cache resolved types in the binding table (path compression for
    /// type values) to avoid O(N²) repeated cloning when the same TypeVar is
    /// resolved multiple times.
    pub fn resolve(&mut self, ty: &Type) -> Type {
        match ty {
            Type::TypeVar(id) => {
                let root = self.find(*id);
                if let Some(bound) = self.binding.get(&root).cloned() {
                    // Recursively resolve, then cache the result (path compression for type values)
                    let resolved = self.resolve(&bound);
                    self.binding.insert(root, resolved.clone());
                    resolved
                } else {
                    Type::TypeVar(root)
                }
            }
            Type::Option(inner) => Type::Option(Box::new(self.resolve(inner))),
            Type::Result(ok, err) => {
                Type::Result(Box::new(self.resolve(ok)), Box::new(self.resolve(err)))
            }
            Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| self.resolve(e)).collect()),
            Type::Func(args, ret) => Type::Func(
                args.iter().map(|a| self.resolve(a)).collect(),
                Box::new(self.resolve(ret)),
            ),
            Type::ExternFunc(args, ret) => Type::ExternFunc(
                args.iter().map(|a| self.resolve(a)).collect(),
                Box::new(self.resolve(ret)),
            ),
            Type::Ref(lt, inner) => Type::Ref(lt.clone(), Box::new(self.resolve(inner))),
            Type::RefMut(lt, inner) => Type::RefMut(lt.clone(), Box::new(self.resolve(inner))),
            Type::Shared(inner) => Type::Shared(Box::new(self.resolve(inner))),
            Type::LocalShared(inner) => Type::LocalShared(Box::new(self.resolve(inner))),
            Type::Weak(inner) => Type::Weak(Box::new(self.resolve(inner))),
            Type::WeakLocal(inner) => Type::WeakLocal(Box::new(self.resolve(inner))),
            Type::RawPtr(inner) => Type::RawPtr(Box::new(self.resolve(inner))),
            Type::RawPtrMut(inner) => Type::RawPtrMut(Box::new(self.resolve(inner))),
            Type::CShared(inner) => Type::CShared(Box::new(self.resolve(inner))),
            Type::CBorrow(inner) => Type::CBorrow(Box::new(self.resolve(inner))),
            Type::CBorrowMut(inner) => Type::CBorrowMut(Box::new(self.resolve(inner))),
            Type::CBuffer(inner) => Type::CBuffer(Box::new(self.resolve(inner))),
            Type::Array(inner, n) => Type::Array(Box::new(self.resolve(inner)), *n),
            Type::Slice(inner) => Type::Slice(Box::new(self.resolve(inner))),
            Type::Newtype(name, inner) => {
                Type::Newtype(name.clone(), Box::new(self.resolve(inner)))
            }
            Type::Name(name, args) => {
                Type::Name(name.clone(), args.iter().map(|a| self.resolve(a)).collect())
            }
            Type::ForAll(params, body) => {
                Type::ForAll(params.clone(), Box::new(self.resolve(body)))
            }
            // Leaf types — no TypeVars inside
            Type::Infer
            | Type::Nothing
            | Type::Allocator
            | Type::RawString
            | Type::Cap(_)
            | Type::ImplTrait(_)
            | Type::DynTrait(_) => ty.clone(),
        }
    }

    /// Unify two types. Adds constraints to the unification table.
    ///
    /// Both types should be resolved before calling this (or this will resolve them).
    pub fn unify(&mut self, a: &Type, b: &Type) -> Result<(), UnifyError> {
        let a_resolved = self.resolve(a);
        let b_resolved = self.resolve(b);

        match (&a_resolved, &b_resolved) {
            // TypeVar on either side — bind to the other
            (Type::TypeVar(id), _) => {
                if Self::occurs_in(*id, &b_resolved) {
                    return Err(UnifyError::OccurCheck(
                        *id,
                        crate::core::helpers::fmt_type(&b_resolved),
                    ));
                }
                let root = self.find(*id);
                self.binding.insert(root, b_resolved.clone());
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
                self.binding.insert(root, a_resolved.clone());
                Ok(())
            }

            // CO-C2 (audit): Type escape hatches — `_` / `Any` / `Infer` unify with anything.
            // SAFETY: `_` is emitted by the parser when the user writes `let x: _ = ...`.
            //         Such bindings appear ONLY at let-init positions (check_stmt.rs:626)
            //         where the inferred init_ty substitutes for the declared type.
            //         `Any` is user-authored for gradual-typing / FFI; lint W012 warns when
            //         it is used as a let-binding declared type.
            // TODO(#v0.31-type-engine): restrict these to top-level inference boundaries
            //       and surface E0710 at function call/field access sites.
            (Type::Name(n, _), _) if n == "_" => Ok(()),
            (_, Type::Name(n, _)) if n == "_" => Ok(()),
            (Type::Name(n, _), _) if n == "Any" => Ok(()),
            (_, Type::Name(n, _)) if n == "Any" => Ok(()),
            // L12: single-sided "unknown" must not unify (helpers already reject);
            // both-unknown is allowed as a cascade placeholder only.
            (Type::Name(na, _), Type::Name(nb, _)) if na == "unknown" && nb != "unknown" => {
                Err(UnifyError::Mismatch(format!(
                    "cannot unify unknown with {}",
                    nb
                )))
            }
            (Type::Name(na, _), Type::Name(nb, _)) if nb == "unknown" && na != "unknown" => {
                Err(UnifyError::Mismatch(format!(
                    "cannot unify {} with unknown",
                    na
                )))
            }
            // CO-C2 (audit): Type::Infer placeholder — legitimate inference variable binding.
            // SAFETY: only emitted by `parse_type_atom` for `_` (parse_type.rs:67) and
            //         threaded through let-bindings. Resolved by substitution at the
            //         let-binding site (check_stmt.rs:626-628).
            (Type::Infer, _) | (_, Type::Infer) => Ok(()),

            // Dual representation normalization: Name("Option", [T]) <-> Option(T)
            (Type::Name(n, args), Type::Option(inner)) if n == "Option" && args.len() == 1 => {
                self.unify(&args[0], inner)
            }
            (Type::Option(inner), Type::Name(n, args)) if n == "Option" && args.len() == 1 => {
                self.unify(inner, &args[0])
            }
            (Type::Name(n, args), Type::Result(ok, err)) if n == "Result" && args.len() == 2 => {
                self.unify(&args[0], ok)?;
                self.unify(&args[1], err)
            }
            (Type::Result(ok, err), Type::Name(n, args)) if n == "Result" && args.len() == 2 => {
                self.unify(ok, &args[0])?;
                self.unify(err, &args[1])
            }
            // Same constructors — unify structurally
            (Type::Name(na, aa), Type::Name(nb, ab)) if na == nb && aa.len() == ab.len() => {
                for (a, b) in aa.iter().zip(ab.iter()) {
                    self.unify(a, b)?;
                }
                Ok(())
            }
            (Type::Option(a), Type::Option(b)) => self.unify(a, b),
            (Type::Result(a1, a2), Type::Result(b1, b2)) => {
                self.unify(a1, b1)?;
                self.unify(a2, b2)
            }
            (Type::Tuple(a), Type::Tuple(b)) if a.len() == b.len() => {
                for (a, b) in a.iter().zip(b.iter()) {
                    self.unify(a, b)?;
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
                    self.unify(a, b)?;
                }
                self.unify(a_ret, b_ret)
            }
            (Type::Ref(_, a), Type::Ref(_, b)) => self.unify(a, b),
            (Type::RefMut(_, a), Type::RefMut(_, b)) => self.unify(a, b),
            (Type::Shared(a), Type::Shared(b)) => self.unify(a, b),
            (Type::LocalShared(a), Type::LocalShared(b)) => self.unify(a, b),
            (Type::Weak(a), Type::Weak(b)) => self.unify(a, b),
            (Type::WeakLocal(a), Type::WeakLocal(b)) => self.unify(a, b),
            (Type::RawPtr(a), Type::RawPtr(b)) => self.unify(a, b),
            (Type::RawPtrMut(a), Type::RawPtrMut(b)) => self.unify(a, b),
            (Type::CShared(a), Type::CShared(b)) => self.unify(a, b),
            (Type::CBorrow(a), Type::CBorrow(b)) => self.unify(a, b),
            (Type::CBorrowMut(a), Type::CBorrowMut(b)) => self.unify(a, b),
            (Type::CBuffer(a), Type::CBuffer(b)) => self.unify(a, b),
            (Type::Slice(a), Type::Slice(b)) => self.unify(a, b),
            (Type::Array(a, na), Type::Array(b, nb)) if na == nb => self.unify(a, b),
            (Type::Newtype(na, a), Type::Newtype(nb, b)) if na == nb => self.unify(a, b),
            // CO-H3 (audit): Newtype is transparent — unify with inner type.
            // Guard prevents cross-newtype unification: Newtype("A",_) vs Newtype("B",_)
            // only succeeds if inner of A matches B's same-name case in the recursive call.
            //
            // SAFETY (audit §21 red line 3 — escape hatch): newtypes are a
            // type-safety escape hatch by design — they provide nominal typing
            // with zero runtime cost (the value IS the inner type). Strict
            // nominal typing would require an explicit `.0` deref or cast
            // at every call site, breaking the v0.26 transparent-newtype
            // contract relied on by user code (see
            // tests::typecheck::v026_newtype_transparent and
            // tests::dual_backend::dual_newtype_pattern).
            //
            // Tradeoff: distinct newtypes with the same inner type are
            // technically interchangeable here, which weakens nominal type
            // safety. We mitigate this by emitting W012-style warnings in the
            // linter when a `let x: UserId = ...` is later used as a raw
            // `i32` in a function call. A future v0.31 stricter-newtype pass
            // may add E0259 for cross-newtype coercion.
            (Type::Newtype(_, inner), other) if !matches!(other, Type::Newtype(..)) => {
                self.unify(inner, other)
            }
            (other, Type::Newtype(_, inner)) if !matches!(other, Type::Newtype(..)) => {
                self.unify(inner, other)
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
            (Type::Cap(a), Type::Cap(b)) => {
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

    /// A5: Strict unification — rejects escape hatches (Any, _, Infer).
    ///
    /// Use this at function call sites, field access sites, and other
    /// positions where escape hatches should NOT silently unify. The
    /// regular `unify` is for let-binding inference boundaries where
    /// `_` and `Any` are legitimate.
    ///
    /// Returns Err if either type is an escape hatch, otherwise delegates
    /// to `unify`.
    pub fn unify_strict(&mut self, a: &Type, b: &Type) -> Result<(), UnifyError> {
        let a_resolved = self.resolve(a);
        let b_resolved = self.resolve(b);
        let is_escape = |t: &Type| -> bool {
            matches!(t, Type::Infer)
                || matches!(t, Type::Name(n, _) if n == "Any" || n == "_")
        };
        if is_escape(&a_resolved) {
            return Err(UnifyError::Mismatch(format!(
                "strict unification rejects escape type {}",
                crate::core::helpers::fmt_type(&a_resolved)
            )));
        }
        if is_escape(&b_resolved) {
            return Err(UnifyError::Mismatch(format!(
                "strict unification rejects escape type {}",
                crate::core::helpers::fmt_type(&b_resolved)
            )));
        }
        self.unify(&a_resolved, &b_resolved)
    }
}

impl Default for UnificationTable {
    fn default() -> Self {
        Self::new()
    }
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
    fn test_wildcard_placeholder() {
        let mut table = UnificationTable::new();
        let wildcard = Type::Name("_".into(), vec![]);
        assert!(table.unify(&wildcard, &i32_ty()).is_ok());
    }
}
