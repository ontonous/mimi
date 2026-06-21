use crate::ast::*;
use crate::core::helpers::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

use super::Checker;

impl<'a> Checker<'a> {
    pub(crate) fn check_pattern(
        &mut self,
        pat: &Pattern,
        subject: &Type,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) {
        match pat {
            Pattern::Wildcard => {}
            Pattern::Variable(name) => {
                // If the name matches an enum variant of the subject type,
                // treat it as a constructor match (no variable binding).
                let is_constructor = match subject {
                    Type::Result(_, _) => name == "Ok" || name == "Err",
                    Type::Option(_) => name == "Some" || name == "None",
                    Type::Name(tn, _) => self.types.get(tn)
                        .and_then(|t| match &t.kind { TypeDefKind::Enum(vs) => Some(vs), _ => None })
                        .map(|vs| vs.iter().any(|v| v.name == *name))
                        .unwrap_or(false),
                    _ => false,
                };
                if !is_constructor {
                    if let Some(s) = scopes.last_mut() {
                        s.insert(name.clone(), subject.clone());
                    }
                }
            }
            Pattern::Literal(l) => {
                let lit_ty = match l {
                    Lit::Int(_) => Type::Name("i32".into(), vec![]),
                    Lit::Float(_) => Type::Name("f64".into(), vec![]),
                    Lit::Bool(_) => Type::Name("bool".into(), vec![]),
                    Lit::String(_) => Type::Name("string".into(), vec![]),
                    Lit::FString(_) => Type::Name("string".into(), vec![]),
                    Lit::Unit => Type::Name("unit".into(), vec![]),
                };
                if !same_type(subject, &lit_ty) {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0225,
                            format!(
                                "pattern literal type {} does not match subject {}",
                                fmt_type(&lit_ty),
                                fmt_type(subject)
                            ),
                            Span::single(self.current_line, self.current_col),
                        ).with_help(format!("change the pattern to match type {}", fmt_type(subject)))
                    );
                }
            }
            Pattern::Constructor(name, pats) => {
                // Handle built-in Result<T,E> constructors Ok/Err (only for Type::Result subjects)
                if (name == "Ok" || name == "Err") && matches!(subject, Type::Result(_, _)) {
                    if let Type::Result(ok_ty, err_ty) = subject {
                        let expected_ty = if name == "Ok" { ok_ty } else { err_ty };
                        if pats.len() != 1 {
                            self.emit_code(crate::diagnostic::codes::E0228, format!("'{}' expects 1 argument, got {}", name, pats.len()));
                        } else {
                            self.check_pattern(&pats[0], expected_ty, scopes);
                        }
                    }
                    return;
                }
                // Handle built-in Option<T> constructors (only for Type::Option subjects)
                if name == "Some" && matches!(subject, Type::Option(_)) {
                    if let Type::Option(inner) = subject {
                        if pats.len() != 1 {
                            self.emit_code(crate::diagnostic::codes::E0228, format!("'Some' expects 1 argument, got {}", pats.len()));
                        } else {
                            self.check_pattern(&pats[0], inner, scopes);
                        }
                    }
                    return;
                }
                if name == "None" && matches!(subject, Type::Option(_)) {
                    if !pats.is_empty() {
                        self.emit_code(crate::diagnostic::codes::E0227, "'None' expects no arguments".to_string());
                    }
                    return;
                }
                let def = self.types.values().find(|t| {
                    match &t.kind {
                        TypeDefKind::Enum(variants) => variants.iter().any(|v| v.name == *name),
                        TypeDefKind::Newtype(_) => t.name == *name,
                        _ => false,
                    }
                });
                match def {
                    Some(tdef) => {
                        match &tdef.kind {
                            TypeDefKind::Enum(variants) => {
                                if let Some(variant) = variants.iter().find(|v| v.name == *name) {
                                    match &variant.payload {
                                        None => {
                                            if !pats.is_empty() {
                                                self.emit_code(crate::diagnostic::codes::E0227, format!(
                                                    "variant '{}' takes no arguments",
                                                    name
                                                ));
                                            }
                                        }
                                        Some(VariantPayload::Tuple(types)) => {
                                            let types: Vec<Type> = types.clone();
                                            if pats.len() != types.len() {
                                                self.emit_code(crate::diagnostic::codes::E0228, format!(
                                                    "variant '{}' expects {} arguments, got {}",
                                                    name,
                                                    types.len(),
                                                    pats.len()
                                                ));
                                            } else {
                                                for (p, t) in pats.iter().zip(types.iter()) {
                                                    self.check_pattern(p, &self.resolve_type(t), scopes);
                                                }
                                            }
                                        }
                                        Some(VariantPayload::Record(fields)) => {
                                            if pats.len() != fields.len() {
                                                self.emit_code(crate::diagnostic::codes::E0228, format!(
                                                    "variant '{}' record expects {} fields, got {}",
                                                    name,
                                                    fields.len(),
                                                    pats.len()
                                                ));
                                            } else {
                                                let resolved: Vec<Type> = fields.iter().map(|f| self.resolve_type(&f.ty)).collect();
                                                for (p, t) in pats.iter().zip(resolved.iter()) {
                                                    self.check_pattern(p, t, scopes);
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    self.emit_code(crate::diagnostic::codes::E0226, format!("variant '{}' not found in type '{}'", name, tdef.name));
                                }
                            }
                            TypeDefKind::Newtype(inner) => {
                                if pats.len() != 1 {
                                    self.emit_code(crate::diagnostic::codes::E0228, format!(
                                        "newtype '{}' pattern expects exactly one argument",
                                        name
                                    ));
                                } else {
                                    self.check_pattern(&pats[0], &self.resolve_type(inner), scopes);
                                }
                            }
                            _ => {
                                self.emit_code(crate::diagnostic::codes::E0226, format!("'{}' is not an enum variant", name));
                            }
                        }
                    }
                    None => {
                        let mut constructors: Vec<String> = Vec::new();
                        for tdef in self.types.values() {
                            match &tdef.kind {
                                TypeDefKind::Enum(variants) => {
                                    constructors.extend(variants.iter().map(|v| v.name.clone()));
                                }
                                TypeDefKind::Newtype(_) => {
                                    constructors.push(tdef.name.clone());
                                }
                                _ => {}
                            }
                        }
                        let suggestion = suggest_name(name, &constructors, 3);
                        let msg = if let Some(s) = suggestion {
                            format!("undefined constructor '{}' — did you mean '{}'?", name, s)
                        } else {
                            format!("undefined constructor '{}'", name)
                        };
                        self.emit_code(crate::diagnostic::codes::E0226, msg);
                    }
                }
            }
            Pattern::Tuple(pats) => {
                match subject {
                    Type::Tuple(types) => {
                        if pats.len() != types.len() {
                            self.emit_code(crate::diagnostic::codes::E0251, format!(
                                "tuple pattern expects {} elements, found {}",
                                types.len(),
                                pats.len()
                            ));
                        } else {
                            for (p, t) in pats.iter().zip(types.iter()) {
                                self.check_pattern(p, t, scopes);
                            }
                        }
                    }
                    _ => {
                        self.emit_code(crate::diagnostic::codes::E0251, format!(
                            "cannot match tuple pattern against non-tuple type {}",
                            fmt_type(subject)
                        ));
                    }
                }
            }
            Pattern::Array(pats) => {
                match subject {
                    Type::Array(inner, size) => {
                        if pats.len() != *size {
                            self.emit_code(crate::diagnostic::codes::E0251, format!(
                                "array pattern expects {} elements, found {}",
                                size,
                                pats.len()
                            ));
                        } else {
                            for p in pats {
                                self.check_pattern(p, inner, scopes);
                            }
                        }
                    }
                    Type::Name(n, _) if n == "List" => {
                        // List pattern: check each element against the element type
                        // For now, just check against the inner type if available
                    }
                    _ => {
                        self.emit_code(crate::diagnostic::codes::E0251, format!(
                            "cannot match array pattern against non-array type {}",
                            fmt_type(subject)
                        ));
                    }
                }
            }
            Pattern::Slice(pats, rest) => {
                match subject {
                    Type::Array(inner, _) | Type::Slice(inner) => {
                        if !pats.is_empty() {
                            for p in pats {
                                self.check_pattern(p, inner, scopes);
                            }
                        }
                        if let Some(rest_pat) = rest {
                            // Rest pattern binds to a List of the element type
                            let list_ty = Type::Name("List".into(), vec![inner.as_ref().clone()]);
                            self.check_pattern(rest_pat, &list_ty, scopes);
                        }
                    }
                    _ => {
                        self.emit_code(crate::diagnostic::codes::E0251, format!(
                            "cannot match slice pattern against non-slice type {}",
                            fmt_type(subject)
                        ));
                    }
                }
            }
        }
    }
}
