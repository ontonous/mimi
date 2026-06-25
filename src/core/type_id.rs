use crate::ast::Type;
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Arena-backed type identifier. O(1) equality, clone, and hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeId(pub(crate) usize);

impl fmt::Debug for TypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "T{}", self.0)
    }
}

/// Arena for storing types with hash-consing (structural deduplication).
pub struct TypeArena {
    types: Vec<Type>,
    /// Map from type hash → list of (type_value, TypeId) for dedup
    index: HashMap<u64, Vec<(Type, TypeId)>>,
}

impl TypeArena {
    pub fn new() -> Self {
        Self {
            types: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Intern a type into the arena. Returns an existing TypeId if an
    /// structurally equal type already exists (hash-consing).
    pub fn intern(&mut self, ty: Type) -> TypeId {
        let hash = type_hash(&ty);
        if let Some(entries) = self.index.get(&hash) {
            for (existing, id) in entries {
                if *existing == ty {
                    return *id;
                }
            }
        }
        let id = TypeId(self.types.len());
        self.types.push(ty.clone());
        self.index.entry(hash).or_default().push((ty, id));
        id
    }

    /// Look up a type by its TypeId.
    pub fn get(&self, id: TypeId) -> &Type {
        &self.types[id.0]
    }

    /// Number of types in the arena.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// Whether the arena is empty.
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

impl Default for TypeArena {
    fn default() -> Self {
        Self::new()
    }
}

/// Deterministic hash for Type values. Two structurally equal types
/// must produce the same hash.
fn type_hash(ty: &Type) -> u64 {
    use std::collections::hash_map::DefaultHasher;

    let mut hasher = DefaultHasher::new();
    type_hash_inner(ty, &mut hasher);
    hasher.finish()
}

fn type_hash_inner(ty: &Type, hasher: &mut impl Hasher) {
    let disc = std::mem::discriminant(ty);
    disc.hash(hasher);

    match ty {
        Type::Name(s, args) => {
            s.hash(hasher);
            for a in args {
                type_hash_inner(a, hasher);
            }
        }
        Type::Ref(lt, inner) | Type::RefMut(lt, inner) => {
            lt.hash(hasher);
            type_hash_inner(inner, hasher);
        }
        Type::Option(inner) => type_hash_inner(inner, hasher),
        Type::Result(ok, err) => {
            type_hash_inner(ok, hasher);
            type_hash_inner(err, hasher);
        }
        Type::Tuple(elems) => {
            for e in elems {
                type_hash_inner(e, hasher);
            }
        }
        Type::Func(params, ret) | Type::ExternFunc(params, ret) => {
            for p in params {
                type_hash_inner(p, hasher);
            }
            type_hash_inner(ret, hasher);
        }
        Type::CBuffer(inner)
        | Type::Shared(inner)
        | Type::LocalShared(inner)
        | Type::Weak(inner)
        | Type::WeakLocal(inner)
        | Type::Array(inner, _)
        | Type::Slice(inner)
        | Type::RawPtr(inner)
        | Type::RawPtrMut(inner)
        | Type::CShared(inner)
        | Type::CBorrow(inner)
        | Type::CBorrowMut(inner) => {
            type_hash_inner(inner, hasher);
        }
        Type::Cap(s) => {
            s.hash(hasher);
        }
        Type::Newtype(s, inner) => {
            s.hash(hasher);
            type_hash_inner(inner, hasher);
        }
        Type::ImplTrait(traits) | Type::DynTrait(traits) => {
            for t in traits {
                t.hash(hasher);
            }
        }
        Type::ForAll(params, body) => {
            for p in params {
                p.hash(hasher);
            }
            type_hash_inner(body, hasher);
        }
        Type::TypeVar(id) => {
            id.hash(hasher);
        }
        Type::Nothing | Type::Allocator | Type::RawString | Type::Infer => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Type;

    #[test]
    fn test_intern_dedup() {
        let mut arena = TypeArena::new();
        let t1 = arena.intern(Type::Name("i32".into(), vec![]));
        let t2 = arena.intern(Type::Name("i32".into(), vec![]));
        assert_eq!(t1, t2);
        assert_eq!(arena.len(), 1);
    }

    #[test]
    fn test_intern_distinct() {
        let mut arena = TypeArena::new();
        let t1 = arena.intern(Type::Name("i32".into(), vec![]));
        let t2 = arena.intern(Type::Name("string".into(), vec![]));
        assert_ne!(t1, t2);
        assert_eq!(arena.len(), 2);
    }

    #[test]
    fn test_intern_nested() {
        let mut arena = TypeArena::new();
        let inner = arena.intern(Type::Name("i32".into(), vec![]));
        let opt1 = arena.intern(Type::Option(Box::new(Type::Name("i32".into(), vec![]))));
        let opt2 = arena.intern(Type::Option(Box::new(Type::Name("i32".into(), vec![]))));
        assert_eq!(opt1, opt2);
        assert_eq!(arena.len(), 2); // i32 + Option<i32>
        let _ = inner;
    }

    #[test]
    fn test_type_id_debug() {
        let id = TypeId(42);
        assert_eq!(format!("{:?}", id), "T42");
    }

    #[test]
    fn test_type_var_intern() {
        let mut arena = TypeArena::new();
        let v1 = arena.intern(Type::TypeVar(0));
        let v2 = arena.intern(Type::TypeVar(0));
        let v3 = arena.intern(Type::TypeVar(1));
        assert_eq!(v1, v2);
        assert_ne!(v1, v3);
        assert_eq!(arena.len(), 2);
    }

    #[test]
    fn test_forall_intern() {
        let mut arena = TypeArena::new();
        let body = arena.intern(Type::Name("i32".into(), vec![]));
        // ForAll stores the body Type directly (not TypeId in Phase 1)
        let f1 = arena.intern(Type::ForAll(vec!["T".into()], Box::new(Type::Name("i32".into(), vec![]))));
        let f2 = arena.intern(Type::ForAll(vec!["T".into()], Box::new(Type::Name("i32".into(), vec![]))));
        assert_eq!(f1, f2);
        assert_eq!(arena.len(), 2); // i32 + ForAll
        let _ = body;
    }
}
