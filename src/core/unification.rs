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
    /// CO-H1: depth limit prevents stack overflow on deeply-nested/cyclic types.
    const MAX_RESOLVE_DEPTH: u32 = 256;
    pub fn resolve(&mut self, ty: &Type) -> Type {
        self.resolve_with_depth(ty, 0)
    }

    fn resolve_with_depth(&mut self, ty: &Type, depth: u32) -> Type {
        if depth >= Self::MAX_RESOLVE_DEPTH {
            return ty.clone();
        }
        let next = depth + 1;
        match ty {
            Type::TypeVar(id) => {
                let root = self.find(*id);
                if let Some(bound) = self.binding.get(&root).cloned() {
                    // Recursively resolve, then cache the result (path compression for type values)
                    let resolved = self.resolve_with_depth(&bound, next);
                    self.binding.insert(root, resolved.clone());
                    resolved
                } else {
                    Type::TypeVar(root)
                }
            }
            Type::Option(inner) => Type::Option(Box::new(self.resolve_with_depth(inner, next))),
            Type::Result(ok, err) => Type::Result(
                Box::new(self.resolve_with_depth(ok, next)),
                Box::new(self.resolve_with_depth(err, next)),
            ),
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|e| self.resolve_with_depth(e, next))
                    .collect(),
            ),
            Type::Func(args, ret) => Type::Func(
                args.iter()
                    .map(|a| self.resolve_with_depth(a, next))
                    .collect(),
                Box::new(self.resolve_with_depth(ret, next)),
            ),
            Type::ExternFunc(args, ret) => Type::ExternFunc(
                args.iter()
                    .map(|a| self.resolve_with_depth(a, next))
                    .collect(),
                Box::new(self.resolve_with_depth(ret, next)),
            ),
            Type::Ref(lt, inner) => {
                Type::Ref(lt.clone(), Box::new(self.resolve_with_depth(inner, next)))
            }
            Type::RefMut(lt, inner) => {
                Type::RefMut(lt.clone(), Box::new(self.resolve_with_depth(inner, next)))
            }
            Type::Shared(inner) => Type::Shared(Box::new(self.resolve_with_depth(inner, next))),
            Type::LocalShared(inner) => {
                Type::LocalShared(Box::new(self.resolve_with_depth(inner, next)))
            }
            Type::Weak(inner) => Type::Weak(Box::new(self.resolve_with_depth(inner, next))),
            Type::WeakLocal(inner) => {
                Type::WeakLocal(Box::new(self.resolve_with_depth(inner, next)))
            }
            Type::RawPtr(inner) => Type::RawPtr(Box::new(self.resolve_with_depth(inner, next))),
            Type::RawPtrMut(inner) => {
                Type::RawPtrMut(Box::new(self.resolve_with_depth(inner, next)))
            }
            Type::CShared(inner) => Type::CShared(Box::new(self.resolve_with_depth(inner, next))),
            Type::CBorrow(inner) => Type::CBorrow(Box::new(self.resolve_with_depth(inner, next))),
            Type::CBorrowMut(inner) => {
                Type::CBorrowMut(Box::new(self.resolve_with_depth(inner, next)))
            }
            Type::CBuffer(inner) => Type::CBuffer(Box::new(self.resolve_with_depth(inner, next))),
            Type::Array(inner, n) => {
                Type::Array(Box::new(self.resolve_with_depth(inner, next)), *n)
            }
            Type::Slice(inner) => Type::Slice(Box::new(self.resolve_with_depth(inner, next))),
            Type::Newtype(name, inner) => {
                Type::Newtype(name.clone(), Box::new(self.resolve_with_depth(inner, next)))
            }
            Type::Name(name, args) => Type::Name(
                name.clone(),
                args.iter()
                    .map(|a| self.resolve_with_depth(a, next))
                    .collect(),
            ),
            Type::ForAll(params, body) => Type::ForAll(
                params.clone(),
                Box::new(self.resolve_with_depth(body, next)),
            ),
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

    /// Stable unification entrypoint. Escape placeholders are rejected here;
    /// callers at an explicit inference boundary must use `unify_inference`.
    pub fn unify(&mut self, a: &Type, b: &Type) -> Result<(), UnifyError> {
        let a_resolved = self.resolve(a);
        let b_resolved = self.resolve(b);
        if is_escape_type(&a_resolved) || is_escape_type(&b_resolved) {
            return Err(UnifyError::Mismatch(format!(
                "checked unification rejects escape types {} and {}",
                crate::core::helpers::fmt_type(&a_resolved),
                crate::core::helpers::fmt_type(&b_resolved)
            )));
        }
        self.unify_inference(&a_resolved, &b_resolved)
    }

    /// Permissive unification for explicit local inference boundaries only.
    pub fn unify_inference(&mut self, a: &Type, b: &Type) -> Result<(), UnifyError> {
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
                self.unify_inference(&args[0], inner)
            }
            (Type::Option(inner), Type::Name(n, args)) if n == "Option" && args.len() == 1 => {
                self.unify_inference(inner, &args[0])
            }
            (Type::Name(n, args), Type::Result(ok, err)) if n == "Result" && args.len() == 2 => {
                self.unify_inference(&args[0], ok)?;
                self.unify_inference(&args[1], err)
            }
            (Type::Result(ok, err), Type::Name(n, args)) if n == "Result" && args.len() == 2 => {
                self.unify_inference(ok, &args[0])?;
                self.unify_inference(err, &args[1])
            }
            // Same constructors — unify structurally
            (Type::Name(na, aa), Type::Name(nb, ab)) if na == nb && aa.len() == ab.len() => {
                for (a, b) in aa.iter().zip(ab.iter()) {
                    self.unify_inference(a, b)?;
                }
                Ok(())
            }
            (Type::Option(a), Type::Option(b)) => self.unify_inference(a, b),
            (Type::Result(a1, a2), Type::Result(b1, b2)) => {
                self.unify_inference(a1, b1)?;
                self.unify_inference(a2, b2)
            }
            (Type::Tuple(a), Type::Tuple(b)) if a.len() == b.len() => {
                for (a, b) in a.iter().zip(b.iter()) {
                    self.unify_inference(a, b)?;
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
                    self.unify_inference(a, b)?;
                }
                self.unify_inference(a_ret, b_ret)
            }
            (Type::Ref(_, a), Type::Ref(_, b)) => self.unify_inference(a, b),
            (Type::RefMut(_, a), Type::RefMut(_, b)) => self.unify_inference(a, b),
            (Type::Shared(a), Type::Shared(b)) => self.unify_inference(a, b),
            (Type::LocalShared(a), Type::LocalShared(b)) => self.unify_inference(a, b),
            (Type::Weak(a), Type::Weak(b)) => self.unify_inference(a, b),
            (Type::WeakLocal(a), Type::WeakLocal(b)) => self.unify_inference(a, b),
            (Type::RawPtr(a), Type::RawPtr(b)) => self.unify_inference(a, b),
            (Type::RawPtrMut(a), Type::RawPtrMut(b)) => self.unify_inference(a, b),
            (Type::CShared(a), Type::CShared(b)) => self.unify_inference(a, b),
            (Type::CBorrow(a), Type::CBorrow(b)) => self.unify_inference(a, b),
            (Type::CBorrowMut(a), Type::CBorrowMut(b)) => self.unify_inference(a, b),
            (Type::CBuffer(a), Type::CBuffer(b)) => self.unify_inference(a, b),
            (Type::Slice(a), Type::Slice(b)) => self.unify_inference(a, b),
            (Type::Array(a, na), Type::Array(b, nb)) if na == nb => self.unify_inference(a, b),
            (Type::Newtype(na, a), Type::Newtype(nb, b)) if na == nb => self.unify_inference(a, b),
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
                self.unify_inference(inner, other)
            }
            (other, Type::Newtype(_, inner)) if !matches!(other, Type::Newtype(..)) => {
                self.unify_inference(inner, other)
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
}

fn is_escape_type(ty: &Type) -> bool {
    match ty {
        Type::Infer => true,
        Type::Name(name, args) => name == "Any" || name == "_" || args.iter().any(is_escape_type),
        Type::Ref(_, inner)
        | Type::RefMut(_, inner)
        | Type::Option(inner)
        | Type::Shared(inner)
        | Type::LocalShared(inner)
        | Type::Weak(inner)
        | Type::WeakLocal(inner)
        | Type::Array(inner, _)
        | Type::Slice(inner)
        | Type::Newtype(_, inner)
        | Type::CBuffer(inner)
        | Type::RawPtr(inner)
        | Type::RawPtrMut(inner)
        | Type::CShared(inner)
        | Type::CBorrow(inner)
        | Type::CBorrowMut(inner)
        | Type::ForAll(_, inner) => is_escape_type(inner),
        Type::Result(ok, err) => is_escape_type(ok) || is_escape_type(err),
        Type::Tuple(items) => items.iter().any(is_escape_type),
        Type::Func(args, ret) | Type::ExternFunc(args, ret) => {
            args.iter().any(is_escape_type) || is_escape_type(ret)
        }
        Type::TypeVar(_)
        | Type::Cap(_)
        | Type::DynTrait(_)
        | Type::ImplTrait(_)
        | Type::Nothing
        | Type::Allocator
        | Type::RawString => false,
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
    fn checked_unify_rejects_wildcard_placeholder() {
        let mut table = UnificationTable::new();
        let wildcard = Type::Name("_".into(), vec![]);
        assert!(table.unify(&wildcard, &i32_ty()).is_err());
        assert!(table.unify_inference(&wildcard, &i32_ty()).is_ok());
    }

    #[test]
    fn checked_unify_rejects_nested_escape_types() {
        let mut table = UnificationTable::new();
        let option_any = Type::Option(Box::new(Type::Name("Any".into(), vec![])));
        let option_i32 = Type::Option(Box::new(i32_ty()));
        assert!(table.unify(&option_any, &option_i32).is_err());
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
        let any = Type::Name("Any".into(), vec![]);
        let infer = Type::Infer;
        let underscore = Type::Name("_".into(), vec![]);
        for escape in [&any, &infer, &underscore] {
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
}
