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
                return t.clone();
            }
        }
        // Check if it's a module-qualified name via use imports
        for module in &self.use_imports.clone() {
            let qualified = format!("{}::{}", module, name);
            if let Some((params, ret)) = self.funcs.get(&qualified) {
                return Type::Func(params.clone(), Box::new(ret.clone()));
            }
        }
        // Check if it's a zero-argument constructor (enum variant without payload)
        if let Some((params, ret)) = self.funcs.get(name) {
            if params.is_empty() {
                return ret.clone();
            }
        }
        // Check if it's a type name (actor/record or enum)
        if let Some(tdef) = self.types.get(name) {
            if matches!(tdef.kind, TypeDefKind::Record(_) | TypeDefKind::Enum(_) | TypeDefKind::Union(_)) {
                // This is a type name - return it as a type
                return Type::Name(name.into(), vec![]);
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
                ).with_help(format!("did you mean '{}'?", suggested))
            );
        } else {
            self.emit_code(crate::diagnostic::codes::E0400, format!("undefined variable '{}'", name));
        }
        Type::Name("unknown".into(), vec![])
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
