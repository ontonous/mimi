use crate::ast::*;
use crate::core::helpers::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

use super::Checker;

impl<'a> Checker<'a> {
    pub(crate) fn resolve_type(&self, ty: &Type) -> Type {
        match ty {
            Type::Name(name, args) => {
                if let Some(aliased) = self.aliases.get(name) {
                    if let Some(generics) = self.type_generics.get(name) {
                        if !args.is_empty() && args.len() == generics.len() {
                            let type_map: HashMap<String, Type> = generics.iter()
                                .zip(args.iter())
                                .map(|(g, a)| (g.name.clone(), a.clone()))
                                .collect();
                            return subst_type_params(aliased, generics, &type_map);
                        }
                    }
                    aliased.clone()
                } else if let Some(inner_ty) = self.newtypes.get(name) {
                    // This is a newtype - wrap the resolved inner type in Type::Newtype with name
                    Type::Newtype(name.clone(), Box::new(self.resolve_type(inner_ty)))
                } else {
                    Type::Name(name.clone(), args.clone())
                }
            }
            Type::Ref(lt, inner) => Type::Ref(lt.clone(), Box::new(self.resolve_type(inner))),
            Type::RefMut(lt, inner) => Type::RefMut(lt.clone(), Box::new(self.resolve_type(inner))),
            Type::Option(inner) => Type::Option(Box::new(self.resolve_type(inner))),
            Type::Result(ok, err) => Type::Result(
                Box::new(self.resolve_type(ok)),
                Box::new(self.resolve_type(err)),
            ),
            Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| self.resolve_type(e)).collect()),
            Type::Func(args, ret) => Type::Func(
                args.iter().map(|a| self.resolve_type(a)).collect(),
                Box::new(self.resolve_type(ret)),
            ),
            Type::ExternFunc(args, ret) => Type::ExternFunc(
                args.iter().map(|a| self.resolve_type(a)).collect(),
                Box::new(self.resolve_type(ret)),
            ),
            Type::Cap(_) | Type::Shared(_) | Type::LocalShared(_) | Type::Weak(_) | Type::WeakLocal(_)
                | Type::CShared(_) | Type::CBorrow(_) | Type::CBorrowMut(_)
                | Type::RawPtr(_) | Type::RawPtrMut(_) | Type::RawString | Type::Allocator => ty.clone(),
            Type::CBuffer(inner) => Type::CBuffer(Box::new(self.resolve_type(inner))),
            Type::Newtype(name, inner) => Type::Newtype(name.clone(), Box::new(self.resolve_type(inner))),
            Type::Array(inner, size) => Type::Array(Box::new(self.resolve_type(inner)), *size),
            Type::Slice(inner) => Type::Slice(Box::new(self.resolve_type(inner))),
            Type::Nothing => Type::Nothing,
            Type::Infer => Type::Infer,
        Type::ImplTrait(traits) => Type::ImplTrait(traits.clone()),
        Type::DynTrait(traits) => Type::DynTrait(traits.clone()),
        }
    }

    /// Check whether a type is allowed to cross the C ABI boundary in an
    /// extern function signature.
    pub(crate) fn is_valid_extern_type(&self, ty: &Type, _in_pointer: bool) -> bool {
        match ty {
            // Scalars and #[repr(C)] user types (Enum, Record, Union)
            Type::Name(name, _) => {
                matches!(name.as_str(), "i32" | "i64" | "f64" | "bool" | "string" | "unit")
                || (self.types.get(name).map(|t| t.attributes.contains(&TypeAttribute::ReprC)).unwrap_or(false)
                    && matches!(self.types.get(name).map(|t| &t.kind),
                        Some(TypeDefKind::Enum(_)) | Some(TypeDefKind::Record(_)) | Some(TypeDefKind::Union(_))))
            }
            // Capabilities
            Type::Cap(_) => true,
            // Raw pointers and FFI passport types
            Type::RawPtr(_) | Type::RawPtrMut(_) | Type::CShared(_) | Type::CBorrow(_) | Type::CBorrowMut(_) => true,
            // Raw string ownership transfer
            Type::RawString => true,
            // C function pointers
            Type::ExternFunc(_, _) => true,
            // C buffer with automatic memory management
            Type::CBuffer(_) => true,
            // References are not allowed directly; must use c_borrow / c_borrow_mut
            Type::Ref(_, _) | Type::RefMut(_, _) => false,
            // Shared ownership is not allowed directly; must use c_shared
            Type::Shared(_) | Type::LocalShared(_) | Type::Weak(_) | Type::WeakLocal(_) => false,
            // Composite Mimi types
            // Tuple is allowed — serialized as JSON over FFI boundary
            Type::Tuple(_) => true,
            Type::Option(_) | Type::Result(_, _) => false,
            Type::Array(_, _) | Type::Slice(_) => false,
            // G1b: Accept closures (Type::Func) as extern callback params
            Type::Func(_, _) => true,
            Type::Newtype(name, inner) => {
                if let Some(tdef) = self.types.get(name) {
                    if tdef.attributes.contains(&TypeAttribute::ReprC) {
                        return self.is_valid_extern_type(inner, _in_pointer);
                    }
                }
                false
            }
            Type::ImplTrait(_) => false,
            Type::DynTrait(_) => false,
            Type::Nothing | Type::Allocator | Type::Infer => false,
        }
    }

    pub(crate) fn is_builtin_type(name: &str) -> bool {
        Self::builtin_type_names().contains(&name.to_string())
    }

    pub(crate) fn builtin_type_names() -> Vec<String> {
        vec![
            "i32".into(), "i64".into(), "f64".into(), "bool".into(),
            "string".into(), "unit".into(), "List".into(), "Set".into(), "Future".into(),
            "Result".into(), "Option".into(),
        ]
    }

    pub(crate) fn check_type_well_formed(&mut self, ty: &Type, context: &str) {
        self.check_type_well_formed_inner(ty, context, false);
    }

    #[allow(dead_code)]
    pub(crate) fn check_type_well_formed_allow_passport(&mut self, ty: &Type, context: &str) {
        self.check_type_well_formed_inner(ty, context, true);
    }

    pub(crate) fn check_type_well_formed_inner(&mut self, ty: &Type, context: &str, allow_passport: bool) {
        if !allow_passport && Self::type_contains_passport(ty) {
            self.emit_code(crate::diagnostic::codes::E0231, format!(
                "FFI passport type '{}' is not allowed in {}",
                fmt_type(ty), context
            ));
            return;
        }
        match ty {
            Type::Name(name, args) => {
                if !Self::is_builtin_type(name)
                    && !self.types.contains_key(name)
                    && !self.generic_scope.contains(name)
                {
                    let mut candidates: Vec<String> = self.types.keys().cloned().collect();
                    candidates.extend(self.generic_scope.clone());
                    candidates.extend(Self::builtin_type_names());
                    let suggestion = suggest_name(name, &candidates, 3);
                    let help = if let Some(suggested) = suggestion {
                        format!("type '{}' not found — did you mean '{}'?", name, suggested)
                    } else {
                        "check the type name spelling or add a 'type' declaration".to_string()
                    };
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0407,
                            format!("unknown type '{}' in {}", name, context),
                            Span::single(self.current_line, self.current_col),
                        ).with_help(help)
                    );
                }
                for arg in args {
                    self.check_type_well_formed_inner(arg, context, allow_passport);
                }
            }
            Type::Ref(_, inner) | Type::RefMut(_, inner) | Type::Option(inner)
                | Type::Shared(inner) | Type::LocalShared(inner) | Type::Weak(inner)
                | Type::WeakLocal(inner)
                | Type::RawPtr(inner) | Type::RawPtrMut(inner)
                | Type::CShared(inner) | Type::CBorrow(inner) | Type::CBorrowMut(inner) => {
                self.check_type_well_formed_inner(inner, context, allow_passport);
            }
            Type::RawString => { /* no inner type to check */ }
            Type::Result(ok, err) => {
                self.check_type_well_formed_inner(ok, context, allow_passport);
                self.check_type_well_formed_inner(err, context, allow_passport);
            }
            Type::Tuple(elems) => {
                for elem in elems {
                    self.check_type_well_formed_inner(elem, context, allow_passport);
                }
            }
            Type::Func(args, ret) => {
                for arg in args {
                    self.check_type_well_formed_inner(arg, context, allow_passport);
                }
                self.check_type_well_formed_inner(ret, context, allow_passport);
            }
            Type::ExternFunc(args, ret) => {
                for arg in args {
                    self.check_type_well_formed_inner(arg, context, allow_passport);
                }
                self.check_type_well_formed_inner(ret, context, allow_passport);
            }
            Type::CBuffer(inner) => {
                self.check_type_well_formed_inner(inner, context, allow_passport);
            }
            Type::Newtype(name, inner) => {
                if !self.types.contains_key(name) && !self.newtypes.contains_key(name) {
                    self.emit_code(crate::diagnostic::codes::E0407, format!("unknown newtype '{}' in {}", name, context));
                }
                self.check_type_well_formed_inner(inner, context, allow_passport);
            }
            Type::Cap(_) | Type::Nothing | Type::Allocator | Type::Infer => {}
            Type::Array(inner, _) | Type::Slice(inner) => {
                self.check_type_well_formed_inner(inner, context, allow_passport);
            }
            Type::ImplTrait(traits) => {
                for trait_name in traits {
                    if !self.traits.contains_key(trait_name) {
                        self.emit_code(crate::diagnostic::codes::E0406, format!("unknown trait '{}' in impl Trait in {}", trait_name, context));
                    }
                }
            }
            Type::DynTrait(traits) => {
                for trait_name in traits {
                    if !self.traits.contains_key(trait_name) {
                        self.emit_code(crate::diagnostic::codes::E0406, format!("unknown trait '{}' in dyn Trait in {}", trait_name, context));
                    }
                }
            }
        }
    }

    /// Returns true if the type (or any type nested inside it) is one of the
    /// FFI boundary passport types.
    pub(crate) fn type_contains_passport(ty: &Type) -> bool {
        match ty {
            Type::RawPtr(_) | Type::RawPtrMut(_)
                | Type::CShared(_) | Type::CBorrow(_) | Type::CBorrowMut(_)
                | Type::RawString => true,
            Type::Name(_, args) => args.iter().any(Self::type_contains_passport),
            Type::Ref(_, inner) | Type::RefMut(_, inner) | Type::Option(inner)
                | Type::Shared(inner) | Type::LocalShared(inner) | Type::Weak(inner)
                | Type::WeakLocal(inner)
                | Type::Array(inner, _) | Type::Slice(inner) => Self::type_contains_passport(inner),
            Type::Result(ok, err) => Self::type_contains_passport(ok) || Self::type_contains_passport(err),
            Type::Tuple(elems) => elems.iter().any(Self::type_contains_passport),
            Type::Func(args, ret) => args.iter().any(Self::type_contains_passport) || Self::type_contains_passport(ret),
            Type::ExternFunc(args, ret) => args.iter().any(Self::type_contains_passport) || Self::type_contains_passport(ret),
            Type::CBuffer(inner) => Self::type_contains_passport(inner),
            Type::Newtype(_, inner) => Self::type_contains_passport(inner),
            Type::Cap(_) | Type::Nothing | Type::Allocator | Type::Infer => false,
            Type::ImplTrait(_) => false,
            Type::DynTrait(_) => false,
        }
    }

    /// Check if a type implements a trait (built-in or user-defined)
    pub(crate) fn type_implements_trait(&self, ty: &Type, trait_name: &str) -> bool {
        // Check built-in traits first
        if self.type_implements_builtin_trait(ty, trait_name) {
            return true;
        }
        // Check user-defined trait implementations
        match ty {
            Type::Name(type_name, _) => {
                self.impls.contains_key(&(trait_name.to_string(), type_name.clone()))
            }
            _ => false,
        }
    }

    /// Check if a type automatically implements a built-in trait (Clone/Default/Copy/Eq)
    fn type_implements_builtin_trait(&self, ty: &Type, trait_name: &str) -> bool {
        match trait_name {
            "Clone" => {
                // All types in Mimi are cloneable via assignment copy
                matches!(ty, Type::Name(_, _) | Type::Tuple(_) | Type::Option(_)
                    | Type::Result(_, _) | Type::Array(_, _) | Type::Slice(_))
            }
            "Copy" => {
                // Only primitive scalar types are Copy
                self.type_is_primitive_scalar(ty)
            }
            "Default" => {
                self.type_has_default(ty)
            }
            "Eq" => {
                // Types with == operator
                self.type_is_eq_comparable(ty)
            }
            _ => false,
        }
    }

    /// Check if a type is a primitive scalar (i32, i64, f64, bool, unit)
    fn type_is_primitive_scalar(&self, ty: &Type) -> bool {
        match ty {
            Type::Name(name, _) => matches!(name.as_str(), "i32" | "i64" | "f64" | "bool" | "unit"),
            _ => false,
        }
    }

    /// Check if a type has a default value
    fn type_has_default(&self, ty: &Type) -> bool {
        match ty {
            Type::Name(name, args) => {
                if matches!(name.as_str(), "i32" | "i64" | "f64" | "bool" | "string" | "unit") {
                    return true;
                }
                // Option types have default (None)
                if name == "Option" && args.len() == 1 {
                    return true;
                }
                // List<T> has default (empty list)
                if name == "List" && args.len() == 1 {
                    return true;
                }
                false
            }
            Type::Tuple(elems) => elems.iter().all(|e| self.type_has_default(e)),
            _ => false,
        }
    }

    /// Check if a type supports equality comparison
    fn type_is_eq_comparable(&self, ty: &Type) -> bool {
        match ty {
            Type::Name(name, _) => matches!(name.as_str(), "i32" | "i64" | "f64" | "bool" | "string" | "unit"),
            Type::Tuple(elems) => elems.iter().all(|e| self.type_is_eq_comparable(e)),
            _ => false,
        }
    }
}
