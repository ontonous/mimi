use crate::ast::Type;

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
        | Type::DynTrait(_) => folder.fold_leaf(ty),
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
}

impl NamedSubstitutionFolder {
    pub fn new(mapping: std::collections::HashMap<String, Type>) -> Self {
        Self {
            mapping,
            shadowed: Vec::new(),
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
        {
            if let Some(replacement) = self.mapping.get(&name) {
                return replacement.clone();
            }
        }
        Type::Name(name, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
