use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{fmt_type, suggest_name};
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

impl<'a> Checker<'a> {
    pub(in crate::core) fn infer_field_access(
        &mut self,
        obj: &Expr,
        field: &str,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let obj_ty = self.infer_expr(obj, scopes);
        self.infer_field_access_on_type(&obj_ty, field, scopes)
    }

    pub(in crate::core) fn infer_field_access_on_type(
        &mut self,
        obj_ty: &Type,
        field: &str,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        match obj_ty {
            Type::Name(name, _) => {
                if let Some(actor_def) = self.file.items.iter().find_map(|item| {
                    if let Item::Actor(a) = item {
                        if a.name == *name {
                            Some(a)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }) {
                    if let Some(f) = actor_def.fields.iter().find(|f| f.name == field) {
                        return self.resolve_type(&f.ty);
                    }
                    let field_names: Vec<String> =
                        actor_def.fields.iter().map(|f| f.name.clone()).collect();
                    let suggestion = suggest_name(field, &field_names, 3);
                    let help = if let Some(s) = suggestion {
                        format!("did you mean '{}'?", s)
                    } else {
                        format!("available fields: {}", field_names.join(", "))
                    };
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0220,
                            format!("actor '{}' has no field '{}'", name, field),
                            Span::single(self.current_line, self.current_col),
                        )
                        .with_help(&help),
                    );
                    return Type::Name("unknown".into(), vec![]);
                }
                if let Some(tdef) = self.types.get(name) {
                    match &tdef.kind {
                        TypeDefKind::Record(fields) => {
                            if let Some(f) = fields.iter().find(|f| f.name == field) {
                                return self.resolve_type(&f.ty);
                            }
                            if let Some(methods) = self.type_methods.get(name) {
                                if let Some((trait_name, _)) =
                                    methods.iter().find(|(_, m)| m == field)
                                {
                                    let tn = trait_name.clone();
                                    if let Some((params, ret)) = self
                                        .trait_method_sigs
                                        .get(&(tn, field.to_string()))
                                        .cloned()
                                    {
                                        return Type::Func(params, Box::new(ret));
                                    }
                                }
                            }
                            let field_names: Vec<String> =
                                fields.iter().map(|f| f.name.clone()).collect();
                            let suggestion = suggest_name(field, &field_names, 3);
                            self.errors.push(
                                Diagnostic::error_code(
                                    crate::diagnostic::codes::E0220,
                                    format!("type '{}' has no field '{}'", name, field),
                                    Span::single(self.current_line, self.current_col),
                                )
                                .with_help(
                                    suggestion
                                        .map(|s| format!("did you mean '{}'?", s))
                                        .unwrap_or_default(),
                                ),
                            );
                            Type::Name("unknown".into(), vec![])
                        }
                        TypeDefKind::Enum(variants) => {
                            if variants.iter().any(|v| v.name == field) {
                                let variant_func = format!("{}::{}", name, field);
                                if let Some((params, ret)) = self.funcs.get(&variant_func) {
                                    Type::Func(params.clone(), Box::new(ret.clone()))
                                } else {
                                    Type::Name(name.into(), vec![])
                                }
                            } else {
                                let variant_names: Vec<String> =
                                    variants.iter().map(|v| v.name.clone()).collect();
                                let suggestion = suggest_name(field, &variant_names, 3);
                                self.errors.push(
                                    Diagnostic::error_code(
                                        crate::diagnostic::codes::E0246,
                                        if let Some(s) = suggestion {
                                            format!(
                                                "type '{}' has no variant '{}' — did you mean '{}'?",
                                                name, field, s
                                            )
                                        } else {
                                            format!(
                                                "type '{}' has no variant '{}' — available variants: {}",
                                                name,
                                                field,
                                                variant_names.join(", ")
                                            )
                                        },
                                        Span::single(self.current_line, self.current_col),
                                    )
                                    .with_help("check the variant name spelling"),
                                );
                                Type::Name("unknown".into(), vec![])
                            }
                        }
                        _ => {
                            if let Some(methods) = self.type_methods.get(name) {
                                if let Some((trait_name, _)) =
                                    methods.iter().find(|(_, m)| m == field)
                                {
                                    let tn = trait_name.clone();
                                    if let Some((params, ret)) = self
                                        .trait_method_sigs
                                        .get(&(tn, field.to_string()))
                                        .cloned()
                                    {
                                        return Type::Func(params, Box::new(ret));
                                    }
                                }
                            }
                            self.emit_code(
                                crate::diagnostic::codes::E0249,
                                format!("'{}' is not a record type", name),
                            );
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                } else {
                    self.emit_code(
                        crate::diagnostic::codes::E0220,
                        format!("field access on unknown type '{}'", name),
                    );
                    Type::Name("unknown".into(), vec![])
                }
            }
            Type::Tuple(elems) => match field.parse::<usize>() {
                Ok(idx) if idx < elems.len() => elems[idx].clone(),
                _ => {
                    self.emit_code(
                        crate::diagnostic::codes::E0223,
                        format!("tuple of {} elements has no field '{}'", elems.len(), field),
                    );
                    Type::Name("unknown".into(), vec![])
                }
            },
            Type::Ref(_, inner) | Type::RefMut(_, inner) => {
                self.infer_field_deref(inner, field, scopes)
            }
            Type::Shared(inner) | Type::LocalShared(inner) => {
                self.infer_field_deref(inner, field, scopes)
            }
            Type::Newtype(_, inner) => self.infer_field_deref(inner, field, scopes),
            Type::Infer => Type::Infer,
            _ => {
                self.errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0219,
                        format!(
                            "field access requires record type, found {}",
                            fmt_type(obj_ty)
                        ),
                        Span::single(self.current_line, self.current_col),
                    )
                    .with_help("only record types support field access with '.'"),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    pub(in crate::core) fn infer_field_deref(
        &mut self,
        inner: &Type,
        field: &str,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        self.infer_field_access_on_type(inner, field, scopes)
    }

    pub(in crate::core) fn infer_tuple_index(
        &mut self,
        obj: &Expr,
        idx: usize,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let obj_ty = self.infer_expr(obj, scopes);
        match &obj_ty {
            Type::Tuple(elems) => {
                if idx < elems.len() {
                    elems[idx].clone()
                } else {
                    self.emit_code(
                        crate::diagnostic::codes::E0243,
                        format!("tuple index {} out of bounds (len {})", idx, elems.len()),
                    );
                    Type::Name("unknown".into(), vec![])
                }
            }
            Type::Newtype(_, inner) if idx == 0 => inner.as_ref().clone(),
            _ => {
                self.emit_code(
                    crate::diagnostic::codes::E0244,
                    format!(
                        "cannot index non-tuple type {} with .{}",
                        fmt_type(&obj_ty),
                        idx
                    ),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    pub(in crate::core) fn infer_index(
        &mut self,
        obj: &Expr,
        idx: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let obj_ty = self.infer_expr(obj, scopes);
        let idx_ty = self.infer_expr(idx, scopes);
        if !is_int(&idx_ty) {
            self.emit_code(
                crate::diagnostic::codes::E0217,
                format!("index must be integer, found {}", fmt_type(&idx_ty)),
            );
        }
        match obj_ty {
            Type::Name(n, args) if n == "List" && args.len() == 1 => args[0].clone(),
            Type::Name(n, _) if n == "string" => Type::Name("string".into(), vec![]),
            _ => {
                self.emit_code(
                    crate::diagnostic::codes::E0218,
                    format!("cannot index {}", fmt_type(&obj_ty)),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }
}

fn is_int(t: &Type) -> bool {
    crate::core::helpers::is_int(t)
}
