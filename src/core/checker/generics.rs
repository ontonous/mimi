use crate::ast::*;
use std::collections::HashMap;

use super::Checker;

impl<'a> Checker<'a> {
    pub(crate) fn type_uses_type_param(&self, ty: &Type, type_param: &str) -> bool {
        match ty.unlocated() {
            Type::Name(name, args) => {
                name == type_param
                    || args
                        .iter()
                        .any(|arg| self.type_uses_type_param(arg, type_param))
            }
            Type::Ref(_, inner)
            | Type::RefMut(_, inner)
            | Type::Option(inner)
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
            | Type::Slice(inner)
            | Type::Array(inner, _) => self.type_uses_type_param(inner, type_param),
            Type::Result(ok, err) => {
                self.type_uses_type_param(ok, type_param)
                    || self.type_uses_type_param(err, type_param)
            }
            Type::Tuple(elems) => elems
                .iter()
                .any(|e| self.type_uses_type_param(e, type_param)),
            Type::Func(args, ret) => {
                args.iter()
                    .any(|a| self.type_uses_type_param(a, type_param))
                    || self.type_uses_type_param(ret, type_param)
            }
            Type::ExternFunc(args, ret) => {
                args.iter()
                    .any(|a| self.type_uses_type_param(a, type_param))
                    || self.type_uses_type_param(ret, type_param)
            }
            Type::Newtype(_, inner) => self.type_uses_type_param(inner, type_param),
            _ => false,
        }
    }

    /// Instantiate a surface generic signature with one fresh inference variable
    /// per binder. Repeated occurrences share the same variable and therefore
    /// cannot take the old first-wins path.
    pub(crate) fn instantiate_generic_signature(
        &mut self,
        params: &[Type],
        ret: &Type,
        generics: &[GenericParam],
    ) -> (Vec<Type>, Type, HashMap<String, Type>) {
        let substitutions: HashMap<String, Type> = generics
            .iter()
            .map(|generic| {
                (
                    generic.name.clone(),
                    Type::TypeVar(self.unification.fresh_var()),
                )
            })
            .collect();
        let instantiate = |ty: &Type| {
            let mut folder =
                crate::core::type_folder::NamedSubstitutionFolder::new(substitutions.clone());
            crate::core::type_folder::walk_type(ty.clone(), &mut folder)
        };
        (
            params.iter().map(instantiate).collect(),
            instantiate(ret),
            substitutions,
        )
    }
}
