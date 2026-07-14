use crate::ast::*;
use crate::core::helpers::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

use super::Checker;

impl<'a> Checker<'a> {
    pub(crate) fn lookup_var(&mut self, name: &str, scopes: &mut [HashMap<String, Type>]) -> Type {
        for scope in scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                // Arch-4: resolve TypeVars before returning so downstream unify calls
                // get concrete types rather than unresolved inference variables.
                // CO-C1: instantiate ForAll so each use of a polymorphic let gets fresh vars.
                let resolved = self.unification.resolve(t);
                return self.instantiate(&resolved);
            }
        }
        // Check if it's a module-qualified name via use imports
        for module in &self.use_imports.clone() {
            let qualified = format!("{}::{}", module, name);
            if let Some((params, ret)) = self.funcs.get(&qualified) {
                // Arch-4: resolve TypeVars in function signature
                let ret = self.unification.resolve(ret);
                let func_ty = Type::Func(params.clone(), Box::new(ret));
                return self.func_value_type(name, &qualified, func_ty);
            }
        }
        // Check if it's a function name (used as first-class value)
        if let Some((params, ret)) = self.funcs.get(name) {
            if params.is_empty() {
                // Zero-argument constructor (enum variant without payload)
                return self.unification.resolve(ret);
            } else {
                // Function reference: return func(T) -> U type
                let resolved_params: Vec<Type> =
                    params.iter().map(|p| self.unification.resolve(p)).collect();
                let resolved_ret = self.unification.resolve(ret);
                let func_ty = Type::Func(resolved_params, Box::new(resolved_ret));
                return self.func_value_type(name, name, func_ty);
            }
        }

        // Check if it's a type name (actor/record or enum)
        if let Some(tdef) = self.types.get(name) {
            if matches!(
                tdef.kind,
                TypeDefKind::Record(_) | TypeDefKind::Enum(_) | TypeDefKind::Union(_)
            ) {
                // This is a type name - return it as a type
                return Type::Name(name.into(), vec![]);
            }
        }
        // Check if it's a top-level constant
        if let Some(const_ty) = self.const_types.get(name) {
            return self.unification.resolve(const_ty);
        }
        // Built-in bare None constructor (only if no user-defined None variant exists)
        if name == "None" {
            let has_user_none = self.types.values().any(|t| {
                matches!(&t.kind, TypeDefKind::Enum(variants) if variants.iter().any(|v| v.name == "None"))
            });
            if !has_user_none {
                return Type::Option(Box::new(Type::Infer));
            }
        }
        // Collect all known names for "did you mean?" suggestions
        let mut candidates: Vec<String> = Vec::new();
        for scope in scopes.iter().rev() {
            candidates.extend(scope.keys().cloned());
        }
        candidates.extend(self.funcs.keys().cloned());
        candidates.extend(self.types.keys().cloned());

        let suggestion = suggest_name(name, &candidates, 3);
        if let Some(suggested) = suggestion {
            self.errors.push(
                Diagnostic::error_code(
                    crate::diagnostic::codes::E0400,
                    format!("undefined variable '{}'", name),
                    Span::single(self.current_line, self.current_col),
                )
                .with_help(format!("did you mean '{}'?", suggested)),
            );
        } else {
            self.emit_code(
                crate::diagnostic::codes::E0400,
                format!("undefined variable '{}'", name),
            );
        }
        Type::Name("unknown".into(), vec![])
    }

    /// First-class function value type. Generic top-level functions become
    /// `ForAll` so let-bound uses can re-instantiate per call site (CO-C1).
    fn func_value_type(&mut self, _display: &str, key: &str, func_ty: Type) -> Type {
        let generics = self.func_generics.get(key).cloned().unwrap_or_default();
        if generics.is_empty() {
            return func_ty;
        }
        // Map each generic name T → TypeVar(i) and wrap ForAll([T0..], body).
        let mut remap_names: HashMap<String, u32> = HashMap::new();
        for (i, g) in generics.iter().enumerate() {
            remap_names.insert(g.name.clone(), i as u32);
        }
        let body = Self::replace_generic_names_with_typevars(&func_ty, &remap_names);
        let param_names: Vec<String> = generics.iter().map(|g| g.name.clone()).collect();
        let forall = Type::ForAll(param_names, Box::new(body));
        self.instantiate(&forall)
    }

    fn replace_generic_names_with_typevars(ty: &Type, names: &HashMap<String, u32>) -> Type {
        match ty {
            Type::Name(n, args) if args.is_empty() => {
                if let Some(&id) = names.get(n) {
                    Type::TypeVar(id)
                } else {
                    ty.clone()
                }
            }
            Type::Name(n, args) => Type::Name(
                n.clone(),
                args.iter()
                    .map(|a| Self::replace_generic_names_with_typevars(a, names))
                    .collect(),
            ),
            Type::Option(inner) => Type::Option(Box::new(
                Self::replace_generic_names_with_typevars(inner, names),
            )),
            Type::Result(ok, err) => Type::Result(
                Box::new(Self::replace_generic_names_with_typevars(ok, names)),
                Box::new(Self::replace_generic_names_with_typevars(err, names)),
            ),
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|e| Self::replace_generic_names_with_typevars(e, names))
                    .collect(),
            ),
            Type::Func(args, ret) | Type::ExternFunc(args, ret) => Type::Func(
                args.iter()
                    .map(|a| Self::replace_generic_names_with_typevars(a, names))
                    .collect(),
                Box::new(Self::replace_generic_names_with_typevars(ret, names)),
            ),
            Type::Array(inner, size) => Type::Array(
                Box::new(Self::replace_generic_names_with_typevars(inner, names)),
                *size,
            ),
            Type::Slice(inner) => Type::Slice(Box::new(Self::replace_generic_names_with_typevars(
                inner, names,
            ))),
            Type::Ref(lt, inner) => Type::Ref(
                lt.clone(),
                Box::new(Self::replace_generic_names_with_typevars(inner, names)),
            ),
            Type::RefMut(lt, inner) => Type::RefMut(
                lt.clone(),
                Box::new(Self::replace_generic_names_with_typevars(inner, names)),
            ),
            Type::ForAll(params, body) => Type::ForAll(
                params.clone(),
                Box::new(Self::replace_generic_names_with_typevars(body, names)),
            ),
            other => other.clone(),
        }
    }

    /// Check if an effect is available in the current scope
    pub(crate) fn has_effect(&self, effect: &str) -> bool {
        for scope in self.available_effects.iter().rev() {
            if scope.contains_key(effect) {
                return true;
            }
        }
        false
    }

    /// Get all variant names for an enum type
    pub(crate) fn get_enum_variants(&self, ty: &Type) -> Vec<String> {
        match ty {
            Type::Result(_, _) => {
                vec!["Ok".into(), "Err".into()]
            }
            Type::Option(_) => {
                vec!["Some".into(), "None".into()]
            }
            Type::Name(name, _) => {
                if name == "bool" {
                    // Built-in bool: pretend it has true/false variants
                    vec!["true".into(), "false".into()]
                } else if let Some(tdef) = self.types.get(name) {
                    match &tdef.kind {
                        TypeDefKind::Enum(variants) => {
                            variants.iter().map(|v| v.name.clone()).collect()
                        }
                        _ => Vec::new(),
                    }
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }
}
