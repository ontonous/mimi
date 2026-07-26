use crate::ast::Type;

// ---------------------------------------------------------------------------
// Predicate-based type visitors (AD-5: 6 independent walkers → shared traversal)
// ---------------------------------------------------------------------------

/// Visit all sub-types of `ty`, calling `pred` on each node.
/// Returns `true` if `pred` returns `true` for any node (short-circuit).
///
/// Structural traversal is shared — callers only provide the predicate.
/// This replaces 6 independent recursive walkers that duplicated the same
/// match structure (occurs_in, is_escape_type, contains_infer_artifacts,
/// contains_inference_variables, contains_unresolved_type, scan_residual).
pub fn type_any(ty: &Type, pred: &dyn Fn(&Type) -> bool) -> bool {
    if pred(ty) {
        return true;
    }
    match ty {
        Type::Located { ty, .. } => type_any(ty, pred),
        Type::Name(_, args) => args.iter().any(|a| type_any(a, pred)),
        Type::Tuple(elems) => elems.iter().any(|e| type_any(e, pred)),
        Type::Result(ok, err) => type_any(ok, pred) || type_any(err, pred),
        Type::Func(args, ret) | Type::ExternFunc(args, ret) => {
            args.iter().any(|a| type_any(a, pred)) || type_any(ret, pred)
        }
        Type::Option(inner)
        | Type::Ref(_, inner)
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
        | Type::Slice(inner)
        | Type::Newtype(_, inner) => type_any(inner, pred),
        Type::ForAll(_, body) => type_any(body, pred),
        // Leaf types with no children
        Type::Infer
        | Type::TypeVar(_)
        | Type::Nothing
        | Type::Allocator
        | Type::RawString
        | Type::Cap(_)
        | Type::ImplTrait(_)
        | Type::DynTrait(_)
        | Type::TyErr => false,
    }
}

/// Visit all sub-types of `ty`, calling `pred` on each node.
/// Returns `Err` if `pred` returns `Err` for any node (short-circuit).
///
/// Used by `scan_residual` which needs to report *which* node failed.
pub fn type_try_visit<E>(ty: &Type, pred: &dyn Fn(&Type) -> Result<(), E>) -> Result<(), E> {
    pred(ty)?;
    match ty {
        Type::Located { ty, .. } => type_try_visit(ty, pred),
        Type::Name(_, args) => {
            for arg in args {
                type_try_visit(arg, pred)?;
            }
            Ok(())
        }
        Type::Tuple(elems) => {
            for elem in elems {
                type_try_visit(elem, pred)?;
            }
            Ok(())
        }
        Type::Result(ok, err) => {
            type_try_visit(ok, pred)?;
            type_try_visit(err, pred)
        }
        Type::Func(args, ret) | Type::ExternFunc(args, ret) => {
            for arg in args {
                type_try_visit(arg, pred)?;
            }
            type_try_visit(ret, pred)
        }
        Type::Option(inner)
        | Type::Ref(_, inner)
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
        | Type::Slice(inner)
        | Type::Newtype(_, inner) => type_try_visit(inner, pred),
        Type::ForAll(_, body) => type_try_visit(body, pred),
        // Leaf types with no children
        Type::Infer
        | Type::TypeVar(_)
        | Type::Nothing
        | Type::Allocator
        | Type::RawString
        | Type::Cap(_)
        | Type::ImplTrait(_)
        | Type::DynTrait(_)
        | Type::TyErr => Ok(()),
    }
}

/// Walk a type with a folder, applying the folder's operations.
pub fn walk_type(ty: Type, folder: &mut dyn TypeFolder) -> Type {
    match ty {
        Type::Located { meta, ty } => {
            let inner = walk_type(*ty, folder);
            Type::Located {
                meta,
                ty: Box::new(inner),
            }
        }
        Type::Name(name, args) => {
            let args = args.into_iter().map(|a| walk_type(a, folder)).collect();
            folder.fold_name(name, args)
        }
        Type::Tuple(elems) => {
            let elems = elems.into_iter().map(|e| walk_type(e, folder)).collect();
            folder.fold_tuple(elems)
        }
        Type::Result(ok, err) => {
            let ok = walk_type(*ok, folder);
            let err = walk_type(*err, folder);
            folder.fold_result(ok, err)
        }
        Type::Func(args, ret) => {
            let args = args.into_iter().map(|a| walk_type(a, folder)).collect();
            let ret = walk_type(*ret, folder);
            folder.fold_func(args, ret)
        }
        Type::ExternFunc(args, ret) => {
            let args = args.into_iter().map(|a| walk_type(a, folder)).collect();
            let ret = walk_type(*ret, folder);
            folder.fold_extern_func(args, ret)
        }
        Type::Ref(region, inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_ref(region, inner)
        }
        Type::RefMut(region, inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_ref_mut(region, inner)
        }
        Type::Option(inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_option(inner)
        }
        Type::Shared(inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_shared(inner)
        }
        Type::LocalShared(inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_local_shared(inner)
        }
        Type::Weak(inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_weak(inner)
        }
        Type::WeakLocal(inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_weak_local(inner)
        }
        Type::RawPtr(inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_raw_ptr(inner)
        }
        Type::RawPtrMut(inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_raw_ptr_mut(inner)
        }
        Type::CShared(inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_c_shared(inner)
        }
        Type::CBorrow(inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_c_borrow(inner)
        }
        Type::CBorrowMut(inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_c_borrow_mut(inner)
        }
        Type::CBuffer(inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_c_buffer(inner)
        }
        Type::Array(inner, size) => {
            let inner = walk_type(*inner, folder);
            folder.fold_array(inner, size)
        }
        Type::Slice(inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_slice(inner)
        }
        Type::Newtype(name, inner) => {
            let inner = walk_type(*inner, folder);
            folder.fold_newtype(name, inner)
        }
        Type::ForAll(params, inner) => {
            folder.enter_forall(&params);
            let inner = walk_type(*inner, folder);
            folder.exit_forall();
            folder.fold_forall(params, inner)
        }
        Type::Infer
        | Type::TypeVar(_)
        | Type::Nothing
        | Type::Allocator
        | Type::RawString
        | Type::Cap(_)
        | Type::ImplTrait(_)
        | Type::DynTrait(_)
        | Type::TyErr => folder.fold_leaf(ty),
    }
}

/// Trait for type folder/visitor operations.
pub trait TypeFolder {
    fn enter_forall(&mut self, _params: &[String]) {}
    fn exit_forall(&mut self) {}

    fn fold_name(&mut self, name: String, args: Vec<Type>) -> Type {
        Type::Name(name, args)
    }
    fn fold_tuple(&mut self, elems: Vec<Type>) -> Type {
        Type::Tuple(elems)
    }
    fn fold_result(&mut self, ok: Type, err: Type) -> Type {
        Type::Result(Box::new(ok), Box::new(err))
    }
    fn fold_func(&mut self, args: Vec<Type>, ret: Type) -> Type {
        Type::Func(args, Box::new(ret))
    }
    fn fold_extern_func(&mut self, args: Vec<Type>, ret: Type) -> Type {
        Type::ExternFunc(args, Box::new(ret))
    }
    fn fold_ref(&mut self, region: Option<String>, inner: Type) -> Type {
        Type::Ref(region, Box::new(inner))
    }
    fn fold_ref_mut(&mut self, region: Option<String>, inner: Type) -> Type {
        Type::RefMut(region, Box::new(inner))
    }
    fn fold_option(&mut self, inner: Type) -> Type {
        Type::Option(Box::new(inner))
    }
    fn fold_shared(&mut self, inner: Type) -> Type {
        Type::Shared(Box::new(inner))
    }
    fn fold_local_shared(&mut self, inner: Type) -> Type {
        Type::LocalShared(Box::new(inner))
    }
    fn fold_weak(&mut self, inner: Type) -> Type {
        Type::Weak(Box::new(inner))
    }
    fn fold_weak_local(&mut self, inner: Type) -> Type {
        Type::WeakLocal(Box::new(inner))
    }
    fn fold_raw_ptr(&mut self, inner: Type) -> Type {
        Type::RawPtr(Box::new(inner))
    }
    fn fold_raw_ptr_mut(&mut self, inner: Type) -> Type {
        Type::RawPtrMut(Box::new(inner))
    }
    fn fold_c_shared(&mut self, inner: Type) -> Type {
        Type::CShared(Box::new(inner))
    }
    fn fold_c_borrow(&mut self, inner: Type) -> Type {
        Type::CBorrow(Box::new(inner))
    }
    fn fold_c_borrow_mut(&mut self, inner: Type) -> Type {
        Type::CBorrowMut(Box::new(inner))
    }
    fn fold_c_buffer(&mut self, inner: Type) -> Type {
        Type::CBuffer(Box::new(inner))
    }
    fn fold_array(&mut self, inner: Type, size: usize) -> Type {
        Type::Array(Box::new(inner), size)
    }
    fn fold_slice(&mut self, inner: Type) -> Type {
        Type::Slice(Box::new(inner))
    }
    fn fold_newtype(&mut self, name: String, inner: Type) -> Type {
        Type::Newtype(name, Box::new(inner))
    }
    fn fold_forall(&mut self, params: Vec<String>, inner: Type) -> Type {
        Type::ForAll(params, Box::new(inner))
    }
    fn fold_leaf(&mut self, ty: Type) -> Type {
        ty
    }
}

/// Collect all TypeVar IDs in a type.
pub struct CollectVarsFolder {
    pub vars: Vec<u32>,
    shadowed: Vec<std::collections::HashSet<u32>>,
}

impl CollectVarsFolder {
    pub fn new() -> Self {
        Self {
            vars: Vec::new(),
            shadowed: Vec::new(),
        }
    }
}

impl Default for CollectVarsFolder {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeFolder for CollectVarsFolder {
    fn enter_forall(&mut self, params: &[String]) {
        self.shadowed.push((0..params.len() as u32).collect());
    }

    fn exit_forall(&mut self) {
        self.shadowed.pop();
    }

    fn fold_leaf(&mut self, ty: Type) -> Type {
        if let Type::TypeVar(id) = ty {
            if !self.shadowed.iter().rev().any(|scope| scope.contains(&id)) {
                self.vars.push(id);
            }
        }
        ty
    }
}

/// Remap TypeVar IDs according to a mapping.
pub struct RemapFolder {
    mapping: std::collections::HashMap<u32, u32>,
    shadowed: Vec<std::collections::HashSet<u32>>,
}

impl RemapFolder {
    pub fn new(mapping: std::collections::HashMap<u32, u32>) -> Self {
        Self {
            mapping,
            shadowed: Vec::new(),
        }
    }
}

impl TypeFolder for RemapFolder {
    fn enter_forall(&mut self, params: &[String]) {
        self.shadowed.push((0..params.len() as u32).collect());
    }

    fn exit_forall(&mut self) {
        self.shadowed.pop();
    }

    fn fold_leaf(&mut self, ty: Type) -> Type {
        match ty {
            Type::TypeVar(id) => {
                if self.shadowed.iter().rev().any(|scope| scope.contains(&id)) {
                    return Type::TypeVar(id);
                }
                if let Some(&new_id) = self.mapping.get(&id) {
                    Type::TypeVar(new_id)
                } else {
                    Type::TypeVar(id)
                }
            }
            other => other,
        }
    }
}

/// Substitute named surface generic parameters while respecting nested binders.
pub struct NamedSubstitutionFolder {
    mapping: std::collections::HashMap<String, Type>,
    shadowed: Vec<std::collections::HashSet<String>>,
    /// P1-13: cycle detection for chain substitutions ({T→U, U→T}).
    substituting: std::collections::HashSet<String>,
}

impl NamedSubstitutionFolder {
    pub fn new(mapping: std::collections::HashMap<String, Type>) -> Self {
        Self {
            mapping,
            shadowed: Vec::new(),
            substituting: std::collections::HashSet::new(),
        }
    }
}

impl TypeFolder for NamedSubstitutionFolder {
    fn enter_forall(&mut self, params: &[String]) {
        self.shadowed.push(params.iter().cloned().collect());
    }

    fn exit_forall(&mut self) {
        self.shadowed.pop();
    }

    fn fold_name(&mut self, name: String, args: Vec<Type>) -> Type {
        if args.is_empty()
            && !self
                .shadowed
                .iter()
                .rev()
                .any(|scope| scope.contains(&name))
            && !self.substituting.contains(&name)
        {
            if let Some(replacement) = self.mapping.get(&name) {
                // P1-13: Re-traverse the replacement to handle chain
                // substitutions ({T→U, U→i32} → T resolves to i32).
                // Cycle detection via `substituting` set prevents
                // infinite loops on cyclic mappings ({T→U, U→T}).
                self.substituting.insert(name.clone());
                let result = walk_type(replacement.clone(), self);
                self.substituting.remove(&name);
                return result;
            }
        }
        Type::Name(name, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === RemapFolder tests ===

    #[test]
    fn remap_preserves_nested_forall_binders() {
        let ty = Type::Tuple(vec![
            Type::TypeVar(0),
            Type::ForAll(vec!["U".into()], Box::new(Type::TypeVar(0))),
        ]);
        let mut folder = RemapFolder::new([(0, 7)].into_iter().collect());

        assert_eq!(
            walk_type(ty, &mut folder),
            Type::Tuple(vec![
                Type::TypeVar(7),
                Type::ForAll(vec!["U".into()], Box::new(Type::TypeVar(0))),
            ])
        );
    }

    #[test]
    fn remap_basic_typevar() {
        let ty = Type::TypeVar(3);
        let mut folder = RemapFolder::new([(3, 10)].into_iter().collect());
        assert_eq!(walk_type(ty, &mut folder), Type::TypeVar(10));
    }

    #[test]
    fn remap_unmapped_typevar_unchanged() {
        let ty = Type::TypeVar(5);
        let mut folder = RemapFolder::new([(3, 10)].into_iter().collect());
        assert_eq!(walk_type(ty, &mut folder), Type::TypeVar(5));
    }

    #[test]
    fn remap_nested_in_containers() {
        let ty = Type::Option(Box::new(Type::Result(
            Box::new(Type::TypeVar(1)),
            Box::new(Type::TypeVar(2)),
        )));
        let mut folder = RemapFolder::new([(1, 100), (2, 200)].into_iter().collect());
        assert_eq!(
            walk_type(ty, &mut folder),
            Type::Option(Box::new(Type::Result(
                Box::new(Type::TypeVar(100)),
                Box::new(Type::TypeVar(200)),
            )))
        );
    }

    #[test]
    fn remap_deeply_nested_forall_shadowing() {
        // ForAll shadowing at multiple levels:
        // outer: TypeVar(0) should be remapped
        // inner ForAll shadows 0: TypeVar(0) should NOT be remapped
        let ty = Type::ForAll(
            vec!["A".into()],
            Box::new(Type::Tuple(vec![
                Type::TypeVar(0), // shadowed by ForAll
                Type::TypeVar(1), // not shadowed
            ])),
        );
        let mut folder = RemapFolder::new([(0, 50), (1, 60)].into_iter().collect());
        assert_eq!(
            walk_type(ty, &mut folder),
            Type::ForAll(
                vec!["A".into()],
                Box::new(Type::Tuple(vec![
                    Type::TypeVar(0),  // shadowed, unchanged
                    Type::TypeVar(60), // remapped
                ])),
            )
        );
    }

    // === CollectVarsFolder tests ===

    #[test]
    fn collect_vars_basic() {
        let ty = Type::Tuple(vec![Type::TypeVar(1), Type::TypeVar(2), Type::TypeVar(3)]);
        let mut folder = CollectVarsFolder::new();
        walk_type(ty, &mut folder);
        assert_eq!(folder.vars, vec![1, 2, 3]);
    }

    #[test]
    fn collect_vars_skips_shadowed() {
        let ty = Type::ForAll(
            vec!["A".into(), "B".into()],
            Box::new(Type::Tuple(vec![
                Type::TypeVar(0), // shadowed
                Type::TypeVar(1), // shadowed
                Type::TypeVar(2), // free
            ])),
        );
        let mut folder = CollectVarsFolder::new();
        walk_type(ty, &mut folder);
        assert_eq!(folder.vars, vec![2]);
    }

    #[test]
    fn collect_vars_nested_forall() {
        // Nested ForAll: inner shadows 0, outer doesn't
        let ty = Type::Tuple(vec![
            Type::TypeVar(0),                                           // free
            Type::ForAll(vec!["X".into()], Box::new(Type::TypeVar(0))), // shadowed
        ]);
        let mut folder = CollectVarsFolder::new();
        walk_type(ty, &mut folder);
        assert_eq!(folder.vars, vec![0]); // only the free one
    }

    #[test]
    fn collect_vars_in_containers() {
        let ty = Type::Result(
            Box::new(Type::Option(Box::new(Type::TypeVar(5)))),
            Box::new(Type::TypeVar(6)),
        );
        let mut folder = CollectVarsFolder::new();
        walk_type(ty, &mut folder);
        assert_eq!(folder.vars, vec![5, 6]);
    }

    #[test]
    fn collect_vars_no_duplicates() {
        let ty = Type::Tuple(vec![Type::TypeVar(1), Type::TypeVar(1), Type::TypeVar(1)]);
        let mut folder = CollectVarsFolder::new();
        walk_type(ty, &mut folder);
        // CollectVarsFolder collects all occurrences, not unique
        assert_eq!(folder.vars, vec![1, 1, 1]);
    }

    // === NamedSubstitutionFolder tests ===

    #[test]
    fn named_substitution_preserves_nested_forall_binders() {
        let ty = Type::Tuple(vec![
            Type::Name("T".into(), vec![]),
            Type::ForAll(vec!["T".into()], Box::new(Type::Name("T".into(), vec![]))),
        ]);
        let mut folder = NamedSubstitutionFolder::new(
            [("T".to_string(), Type::Name("i32".into(), vec![]))]
                .into_iter()
                .collect(),
        );

        assert_eq!(
            walk_type(ty, &mut folder),
            Type::Tuple(vec![
                Type::Name("i32".into(), vec![]),
                Type::ForAll(vec!["T".into()], Box::new(Type::Name("T".into(), vec![])),),
            ])
        );
    }

    #[test]
    fn named_substitution_basic() {
        let ty = Type::Name("T".into(), vec![]);
        let mut folder = NamedSubstitutionFolder::new(
            [("T".to_string(), Type::Name("String".into(), vec![]))]
                .into_iter()
                .collect(),
        );
        assert_eq!(
            walk_type(ty, &mut folder),
            Type::Name("String".into(), vec![])
        );
    }

    #[test]
    fn named_substitution_chain() {
        // P1-13: chain substitution {T→U, U→i32} → T resolves to i32
        let ty = Type::Name("T".into(), vec![]);
        let mut folder = NamedSubstitutionFolder::new(
            [
                ("T".to_string(), Type::Name("U".into(), vec![])),
                ("U".to_string(), Type::Name("i32".into(), vec![])),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(walk_type(ty, &mut folder), Type::Name("i32".into(), vec![]));
    }

    #[test]
    fn named_substitution_cycle_detection() {
        // P1-13: cycle {T→U, U→T} should not infinite loop.
        // Trace: T→U→T (cycle detected)→ returns T unchanged.
        let ty = Type::Name("T".into(), vec![]);
        let mut folder = NamedSubstitutionFolder::new(
            [
                ("T".to_string(), Type::Name("U".into(), vec![])),
                ("U".to_string(), Type::Name("T".into(), vec![])),
            ]
            .into_iter()
            .collect(),
        );
        // Should terminate (cycle detection). T→U→T(cycle)→T.
        let result = walk_type(ty, &mut folder);
        assert_eq!(result, Type::Name("T".into(), vec![]));
    }

    #[test]
    fn named_substitution_in_containers() {
        let ty = Type::Option(Box::new(Type::Name("T".into(), vec![])));
        let mut folder = NamedSubstitutionFolder::new(
            [("T".to_string(), Type::Name("bool".into(), vec![]))]
                .into_iter()
                .collect(),
        );
        assert_eq!(
            walk_type(ty, &mut folder),
            Type::Option(Box::new(Type::Name("bool".into(), vec![])))
        );
    }

    #[test]
    fn named_substitution_with_args_not_substituted() {
        // Name with args should NOT be substituted (only bare names)
        let ty = Type::Name("List".into(), vec![Type::Name("T".into(), vec![])]);
        let mut folder = NamedSubstitutionFolder::new(
            [("List".to_string(), Type::Name("Vec".into(), vec![]))]
                .into_iter()
                .collect(),
        );
        // List<T> should NOT become Vec (because List has args)
        assert_eq!(
            walk_type(ty, &mut folder),
            Type::Name("List".into(), vec![Type::Name("T".into(), vec![])])
        );
    }

    // === type_any / type_try_visit tests ===

    #[test]
    fn type_any_finds_nested() {
        let ty = Type::Option(Box::new(Type::Result(
            Box::new(Type::Name("i32".into(), vec![])),
            Box::new(Type::Name("String".into(), vec![])),
        )));
        assert!(type_any(
            &ty,
            &|t| matches!(t, Type::Name(n, _) if n == "String")
        ));
        assert!(!type_any(
            &ty,
            &|t| matches!(t, Type::Name(n, _) if n == "bool")
        ));
    }

    #[test]
    fn type_try_visit_short_circuits() {
        let ty = Type::Tuple(vec![
            Type::Name("i32".into(), vec![]),
            Type::Name("String".into(), vec![]),
        ]);
        let result = type_try_visit(&ty, &|t| {
            if matches!(t, Type::Name(n, _) if n == "String") {
                Err("found String")
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err("found String"));
    }
}
