use crate::ast::*;
use crate::core::helpers::*;
use std::collections::HashMap;

use super::Checker;

impl<'a> Checker<'a> {
    pub(crate) fn type_uses_type_param(&self, ty: &Type, type_param: &str) -> bool {
        match ty {
            Type::Name(name, _) => name == type_param,
            Type::Ref(_, inner) | Type::RefMut(_, inner) | Type::Option(inner) | Type::Shared(inner) | Type::LocalShared(inner) | Type::Weak(inner) | Type::WeakLocal(inner) => {
                self.type_uses_type_param(inner, type_param)
            }
            Type::Result(ok, err) => {
                self.type_uses_type_param(ok, type_param) || self.type_uses_type_param(err, type_param)
            }
            Type::Tuple(elems) => {
                elems.iter().any(|e| self.type_uses_type_param(e, type_param))
            }
            Type::Func(args, ret) => {
                args.iter().any(|a| self.type_uses_type_param(a, type_param)) || self.type_uses_type_param(ret, type_param)
            }
            Type::Newtype(_, inner) => self.type_uses_type_param(inner, type_param),
            _ => false,
        }
    }

    /// Check if a type variable name occurs within a type (occurs check).
    /// Prevents infinite types like `T = List<T>`.
    pub(crate) fn occurs_check(name: &str, ty: &Type) -> bool {
        match ty {
            Type::Name(n, args) => n == name || args.iter().any(|a| Self::occurs_check(name, a)),
            Type::Ref(_, inner) | Type::RefMut(_, inner) => Self::occurs_check(name, inner),
            Type::Option(inner) => Self::occurs_check(name, inner),
            Type::Result(ok, err) => Self::occurs_check(name, ok) || Self::occurs_check(name, err),
            Type::Tuple(elems) => elems.iter().any(|e| Self::occurs_check(name, e)),
            Type::Func(args, ret) => args.iter().any(|a| Self::occurs_check(name, a)) || Self::occurs_check(name, ret),
            Type::Shared(inner) | Type::LocalShared(inner) | Type::Weak(inner) | Type::WeakLocal(inner) => Self::occurs_check(name, inner),
            Type::Newtype(_, inner) => Self::occurs_check(name, inner),
            Type::Array(inner, _) | Type::Slice(inner) => Self::occurs_check(name, inner),
            Type::ExternFunc(args, ret) => args.iter().any(|a| Self::occurs_check(name, a)) || Self::occurs_check(name, ret),
            Type::CBuffer(inner) | Type::RawPtr(inner) | Type::RawPtrMut(inner) | Type::CShared(inner) | Type::CBorrow(inner) | Type::CBorrowMut(inner) => Self::occurs_check(name, inner),
            _ => false,
        }
    }

    /// Infer type parameter bindings from a parameter type and actual argument type
    pub(crate) fn infer_type_params(
        &self,
        param: &Type,
        actual: &Type,
        generics: &[GenericParam],
        type_map: &mut HashMap<String, Type>,
    ) {
        match param {
            Type::Name(name, _) if is_type_param(name, generics) => {
                if !Self::occurs_check(name, actual) {
                    type_map.entry(name.clone()).or_insert_with(|| actual.clone());
                }
            }
            Type::Name(name, p_args) => {
                if is_type_param(name, generics) {
                    if !Self::occurs_check(name, actual) {
                        type_map.entry(name.clone()).or_insert_with(|| actual.clone());
                    }
                } else if !p_args.is_empty() {
                    if let Type::Name(_, a_args) = actual {
                        if p_args.len() == a_args.len() {
                            for (pa, aa) in p_args.iter().zip(a_args.iter()) {
                                self.infer_type_params(pa, aa, generics, type_map);
                            }
                        }
                    }
                }
            }
            Type::Option(inner) => {
                if let Type::Option(a_inner) = actual {
                    self.infer_type_params(inner, a_inner, generics, type_map);
                }
            }
            Type::Result(p_ok, p_err) => {
                if let Type::Result(a_ok, a_err) = actual {
                    self.infer_type_params(p_ok, a_ok, generics, type_map);
                    self.infer_type_params(p_err, a_err, generics, type_map);
                }
            }
            Type::Tuple(p_elems) => {
                if let Type::Tuple(a_elems) = actual {
                    for (pe, ae) in p_elems.iter().zip(a_elems.iter()) {
                        self.infer_type_params(pe, ae, generics, type_map);
                    }
                }
            }
            _ => {}
        }
    }
}
