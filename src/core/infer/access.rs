use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{fmt_type, suggest_name};
use crate::diagnostic::Diagnostic;
use std::collections::HashMap;

/// Replace type parameters in `ty` according to `subst`.
/// audit (MEDIUM): depth guard prevents infinite recursion on self-referencing
/// types (e.g. T = Option<T>). MAX_SUBST_DEPTH=32 is well above any realistic
/// nesting depth. The same pattern is used in record.rs.
const MAX_SUBST_DEPTH: u32 = 32;

fn substitute_type_params(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    subst_with_depth(ty, subst, 0)
}

fn subst_with_depth(ty: &Type, subst: &HashMap<String, Type>, depth: u32) -> Type {
    if depth >= MAX_SUBST_DEPTH {
        return ty.clone();
    }
    let next = depth + 1;
    match ty {
        Type::Located { meta, ty } => {
            subst_with_depth(ty, subst, next).with_meta(*meta)
        }
        Type::Name(name, args) if args.is_empty() && subst.contains_key(name) => {
            subst[name].clone()
        }
        Type::Name(name, args) => Type::Name(
            name.clone(),
            args.iter()
                .map(|a| subst_with_depth(a, subst, next))
                .collect(),
        ),
        Type::Option(inner) => Type::Option(Box::new(subst_with_depth(inner, subst, next))),
        Type::Result(ok, err) => Type::Result(
            Box::new(subst_with_depth(ok, subst, next)),
            Box::new(subst_with_depth(err, subst, next)),
        ),
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| subst_with_depth(e, subst, next))
                .collect(),
        ),
        Type::Func(args, ret) => Type::Func(
            args.iter()
                .map(|a| subst_with_depth(a, subst, next))
                .collect(),
            Box::new(subst_with_depth(ret, subst, next)),
        ),
        Type::ExternFunc(args, ret) => Type::ExternFunc(
            args.iter()
                .map(|a| subst_with_depth(a, subst, next))
                .collect(),
            Box::new(subst_with_depth(ret, subst, next)),
        ),
        Type::Ref(lt, inner) => {
            Type::Ref(lt.clone(), Box::new(subst_with_depth(inner, subst, next)))
        }
        Type::RefMut(lt, inner) => {
            Type::RefMut(lt.clone(), Box::new(subst_with_depth(inner, subst, next)))
        }
        Type::Shared(inner) => Type::Shared(Box::new(subst_with_depth(inner, subst, next))),
        Type::LocalShared(inner) => {
            Type::LocalShared(Box::new(subst_with_depth(inner, subst, next)))
        }
        Type::Weak(inner) => Type::Weak(Box::new(subst_with_depth(inner, subst, next))),
        Type::WeakLocal(inner) => Type::WeakLocal(Box::new(subst_with_depth(inner, subst, next))),
        Type::RawPtr(inner) => Type::RawPtr(Box::new(subst_with_depth(inner, subst, next))),
        Type::RawPtrMut(inner) => Type::RawPtrMut(Box::new(subst_with_depth(inner, subst, next))),
        Type::CShared(inner) => Type::CShared(Box::new(subst_with_depth(inner, subst, next))),
        Type::CBorrow(inner) => Type::CBorrow(Box::new(subst_with_depth(inner, subst, next))),
        Type::CBorrowMut(inner) => Type::CBorrowMut(Box::new(subst_with_depth(inner, subst, next))),
        Type::CBuffer(inner) => Type::CBuffer(Box::new(subst_with_depth(inner, subst, next))),
        Type::Array(inner, n) => Type::Array(Box::new(subst_with_depth(inner, subst, next)), *n),
        Type::Slice(inner) => Type::Slice(Box::new(subst_with_depth(inner, subst, next))),
        Type::Newtype(name, inner) => {
            Type::Newtype(name.clone(), Box::new(subst_with_depth(inner, subst, next)))
        }
        Type::ForAll(params, body) => Type::ForAll(
            params.clone(),
            Box::new(subst_with_depth(body, subst, next)),
        ),
        Type::TypeVar(id) => Type::TypeVar(*id),
        Type::Infer
        | Type::Nothing
        | Type::Allocator
        | Type::RawString
        | Type::Cap(_)
        | Type::ImplTrait(_)
        | Type::DynTrait(_) => ty.clone(),
    }
}

impl<'a> Checker<'a> {
    pub(in crate::core) fn infer_field_access(
        &mut self,
        obj: &Expr,
        field: &str,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        // v0.29.49: reject direct field access on multi-target transition results.
        if let Expr::Ident(name) = obj.unlocated() {
            if self.multi_target_vars.contains_key(name) {
                self.errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0420,
                        format!(
                            "multi-target transition result '{}' must be exhaustively matched before accessing field '{}'",
                            name, field
                        ),
                        self.diagnostic_span(),
                    )
                    .with_help("use `match` to handle all possible return states"),
                );
            }
        }
        let obj_ty = self.infer_expr(obj, scopes);
        self.infer_field_access_on_type(&obj_ty, field, scopes)
    }

    pub(in crate::core) fn infer_field_access_on_type(
        &mut self,
        obj_ty: &Type,
        field: &str,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        match obj_ty.unlocated() {
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
                            self.diagnostic_span(),
                        )
                        .with_help(&help),
                    );
                    return Type::Name("unknown".into(), vec![]);
                }
                if let Some(tdef) = self.types.get(name) {
                    match &tdef.kind {
                        TypeDefKind::Record(fields) => {
                            if let Some(f) = fields.iter().find(|f| f.name == field) {
                                let resolved = self.resolve_type(&f.ty);
                                // If the object type carries concrete type arguments,
                                // instantiate the field type by substituting the type
                                // parameters with those arguments.
                                if let Type::Name(_, args) = obj_ty.unlocated() {
                                    if !args.is_empty() && tdef.generics.len() == args.len() {
                                        let subst: HashMap<String, Type> = tdef
                                            .generics
                                            .iter()
                                            .zip(args.iter())
                                            .map(|(gp, arg)| (gp.name.clone(), arg.clone()))
                                            .collect();
                                        return substitute_type_params(&resolved, &subst);
                                    }
                                }
                                return resolved;
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
                                    self.diagnostic_span(),
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
                                        self.diagnostic_span(),
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
                        self.diagnostic_span(),
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
        match obj_ty.unlocated() {
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
            // Newtype registered as Type::Name (e.g. in impl method self parameter)
            Type::Name(name, _) if idx == 0 && self.newtypes.contains_key(name) => {
                self.newtypes[name].clone()
            }
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
        match obj_ty.unlocated() {
            Type::Name(n, args) if n == "List" && args.len() == 1 => args[0].clone(),
            Type::Name(n, _) if n == "string" => Type::Name("string".into(), vec![]),
            // Support indexing through references: &List<T>, &mut List<T>, &string, &mut string
            Type::Ref(_, inner) | Type::RefMut(_, inner) => match inner.unlocated() {
                Type::Name(n, args) if n == "List" && args.len() == 1 => args[0].clone(),
                Type::Name(n, _) if n == "string" => Type::Name("string".into(), vec![]),
                _ => {
                    self.emit_code(
                        crate::diagnostic::codes::E0218,
                        format!("cannot index {}", fmt_type(&obj_ty)),
                    );
                    Type::Name("unknown".into(), vec![])
                }
            },
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
