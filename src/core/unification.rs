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

    /// Find the root representative of a type variable (with path compression).
    fn find(&mut self, id: u32) -> u32 {
        let parent = *self.parent.get(&id).unwrap_or(&id);
        if parent == id {
            id
        } else {
            let root = self.find(parent);
            self.parent.insert(id, root);
            root
        }
    }

    /// Union two type variables. Returns the root.
    fn union(&mut self, a: u32, b: u32) -> u32 {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return ra;
        }
        // Prefer binding to the variable with lower ID (heuristic for readability)
        if ra < rb {
            self.parent.insert(rb, ra);
            ra
        } else {
            self.parent.insert(ra, rb);
            rb
        }
    }

    /// Check if a type variable occurs inside a type (for occur check).
    fn occurs_in(var: u32, ty: &Type) -> bool {
        match ty {
            Type::TypeVar(id) => *id == var,
            Type::ForAll(_, body) => Self::occurs_in(var, body),
            Type::Option(inner) => Self::occurs_in(var, inner),
            Type::Result(ok, err) => Self::occurs_in(var, ok) || Self::occurs_in(var, err),
            Type::Tuple(elems) => elems.iter().any(|e| Self::occurs_in(var, e)),
            Type::Func(args, ret) | Type::ExternFunc(args, ret) => {
                args.iter().any(|a| Self::occurs_in(var, a)) || Self::occurs_in(var, ret)
            }
            Type::Ref(_, inner) | Type::RefMut(_, inner) | Type::Shared(inner)
            | Type::LocalShared(inner) | Type::Weak(inner) | Type::WeakLocal(inner)
            | Type::RawPtr(inner) | Type::RawPtrMut(inner) | Type::CShared(inner)
            | Type::CBorrow(inner) | Type::CBorrowMut(inner) | Type::CBuffer(inner)
            | Type::Array(inner, _) | Type::Slice(inner) => Self::occurs_in(var, inner),
            Type::Newtype(_, inner) => Self::occurs_in(var, inner),
            Type::Name(_, args) => args.iter().any(|a| Self::occurs_in(var, a)),
            Type::Infer | Type::Nothing | Type::Allocator | Type::RawString
            | Type::Cap(_) | Type::ImplTrait(_) | Type::DynTrait(_) => false,
        }
    }

    /// Resolve a type: replace all TypeVars with their bindings.
    pub fn resolve(&mut self, ty: &Type) -> Type {
        match ty {
            Type::TypeVar(id) => {
                let root = self.find(*id);
                if let Some(bound) = self.binding.get(&root).cloned() {
                    self.resolve(&bound)
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
            Type::Newtype(name, inner) => Type::Newtype(name.clone(), Box::new(self.resolve(inner))),
            Type::Name(name, args) => {
                Type::Name(name.clone(), args.iter().map(|a| self.resolve(a)).collect())
            }
            Type::ForAll(params, body) => Type::ForAll(params.clone(), Box::new(self.resolve(body))),
            // Leaf types — no TypeVars inside
            Type::Infer | Type::Nothing | Type::Allocator | Type::RawString
            | Type::Cap(_) | Type::ImplTrait(_) | Type::DynTrait(_) => ty.clone(),
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

            // Name("_") — inference placeholder, compatible with anything
            (Type::Name(n, _), _) if n == "_" => Ok(()),
            (_, Type::Name(n, _)) if n == "_" => Ok(()),

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
