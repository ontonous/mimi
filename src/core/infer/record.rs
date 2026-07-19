use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::fmt_type;
use std::collections::HashMap;

/// Replace type parameters in `ty` according to `subst`.
/// audit (MEDIUM): guard against infinite recursion on self-referencing types
/// (e.g. `T = List<T>`). Returns original type unchanged past depth limit.
const MAX_SUBST_DEPTH: usize = 32;

fn substitute_type_params(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    subst_with_depth(ty, subst, 0)
}

fn subst_with_depth(ty: &Type, subst: &HashMap<String, Type>, depth: usize) -> Type {
    if depth > MAX_SUBST_DEPTH {
        mimi_debug_assert!(
            false,
            "substitute_type_params: exceeded max depth ({}), \
             possible self-referencing type parameter",
            MAX_SUBST_DEPTH
        );
        return ty.clone();
    }
    let next = depth + 1;
    match ty {
        Type::Located { meta, ty } => subst_with_depth(ty, subst, next).with_meta(*meta),
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
        Type::TypeVar(id) => {
            // TypeVars are not substituted by name parameters.
            Type::TypeVar(*id)
        }
        // Leaf / inference placeholders — no parameters inside.
        Type::Infer
        | Type::Nothing
        | Type::Allocator
        | Type::RawString
        | Type::Cap(_)
        | Type::ImplTrait(_)
        | Type::DynTrait(_) => {
            mimi_debug_assert!(
                !matches!(ty.unlocated(), Type::Infer),
                "substitute_type_params: unexpected Infer leaf type"
            );
            ty.clone()
        }
    }
}

impl<'a> Checker<'a> {
    pub(in crate::core) fn infer_record_expr(
        &mut self,
        ty: &Option<String>,
        fields: &[RecordFieldExpr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let tdef = ty.as_ref().and_then(|n| self.types.get(n)).cloned();
        match tdef {
            Some(tdef) => match &tdef.kind {
                TypeDefKind::Record(expected_fields) => {
                    // Build a substitution for generic parameters when constructing
                    // a generic ADT. Each parameter is mapped to a fresh unification
                    // variable so that field values can infer the concrete types.
                    let mut subst: HashMap<String, Type> = HashMap::new();
                    let mut type_args: Vec<Type> = Vec::new();
                    for gp in &tdef.generics {
                        let var = Type::TypeVar(self.unification.fresh_var());
                        subst.insert(gp.name.clone(), var.clone());
                        type_args.push(var);
                    }

                    let expected: HashMap<String, Type> = expected_fields
                        .iter()
                        .map(|f| {
                            let resolved = self.resolve_type(&f.ty);
                            let instantiated = substitute_type_params(&resolved, &subst);
                            (f.name.clone(), instantiated)
                        })
                        .collect();

                    for (name, value) in fields.iter().map(|f| (&f.name, &f.value)) {
                        if let Some(expected_ty) = expected.get(name) {
                            // Use check_expr to propagate expected type (enables empty list inference)
                            let actual_ty = self.check_expr(expected_ty, value, scopes);
                            // For concrete (non-generic) fields, retain the original
                            // same_type check to keep diagnostics unchanged. For fields
                            // that involve type parameters, use unification so the
                            // parameter can be inferred from the value.
                            let uses_param = subst.values().any(|v| match v.unlocated() {
                                Type::TypeVar(id) => {
                                    crate::core::unification::UnificationTable::occurs_in(
                                        *id,
                                        expected_ty,
                                    )
                                }
                                _ => false,
                            });
                            if uses_param {
                                if self.unification.unify(expected_ty, &actual_ty).is_err() {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0247,
                                        format!(
                                            "field '{}' expected {}, found {}",
                                            name,
                                            fmt_type(expected_ty),
                                            fmt_type(&actual_ty)
                                        ),
                                    );
                                }
                            } else if self.unification.unify(expected_ty, &actual_ty).is_err() {
                                // IF-H2: unify concrete record fields too (TypeVar resolution).
                                self.emit_code(
                                    crate::diagnostic::codes::E0247,
                                    format!(
                                        "field '{}' expected {}, found {}",
                                        name,
                                        fmt_type(expected_ty),
                                        fmt_type(&actual_ty)
                                    ),
                                );
                            }
                        } else {
                            self.emit_code(
                                crate::diagnostic::codes::E0247,
                                format!("type '{}' has no field '{}'", tdef.name, name),
                            );
                        }
                    }
                    for name in expected.keys() {
                        if !fields.iter().any(|f| &f.name == name) {
                            self.emit_code(
                                crate::diagnostic::codes::E0248,
                                format!("missing field '{}' in record literal", name),
                            );
                        }
                    }

                    let ret = Type::Name(tdef.name.clone(), type_args);
                    self.unification.resolve(&ret)
                }
                _ => {
                    self.emit_code(
                        crate::diagnostic::codes::E0249,
                        format!("'{}' is not a record type", tdef.name),
                    );
                    Type::Name("unknown".into(), vec![])
                }
            },
            None => {
                self.emit_code(
                    crate::diagnostic::codes::E0410,
                    "cannot infer record type without explicit type name",
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    pub(in crate::core) fn infer_tuple_expr(
        &mut self,
        elems: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        Type::Tuple(elems.iter().map(|e| self.infer_expr(e, scopes)).collect())
    }

    pub(in crate::core) fn infer_list_expr(
        &mut self,
        elems: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let mut elem_ty = Type::Name("unknown".into(), vec![]);
        for (i, e) in elems.iter().enumerate() {
            let t = self.infer_expr(e, scopes);
            if i == 0 {
                elem_ty = t;
            } else if self.unification.unify(&elem_ty, &t).is_err() {
                // IF-H3: unify list elements so TypeVars resolve.
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    format!(
                        "list element {} type {} does not match first element {}",
                        i + 1,
                        fmt_type(&t),
                        fmt_type(&elem_ty)
                    ),
                );
            } else {
                elem_ty = self.unification.resolve(&elem_ty);
            }
        }
        Type::Name("List".into(), vec![elem_ty])
    }

    pub(in crate::core) fn infer_map_literal(
        &mut self,
        entries: &[(Expr, Expr)],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        for (k, v) in entries {
            let key_ty = self.infer_expr(k, scopes);
            if !crate::core::helpers::is_string(&key_ty) {
                self.emit_code(
                    crate::diagnostic::codes::E0211,
                    format!(
                        "map literal key must be a string, found {}",
                        crate::core::helpers::fmt_type(&key_ty)
                    ),
                );
            }
            self.infer_expr(v, scopes);
        }
        Type::Name("Record".into(), vec![])
    }

    pub(in crate::core) fn infer_set_literal(
        &mut self,
        elems: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let mut elem_ty = Type::Name("unknown".into(), vec![]);
        for (i, e) in elems.iter().enumerate() {
            let t = self.infer_expr(e, scopes);
            if i == 0 {
                elem_ty = t;
            } else if self.unification.unify(&elem_ty, &t).is_err() {
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    format!(
                        "set element {} type {} does not match first element {}",
                        i + 1,
                        fmt_type(&t),
                        fmt_type(&elem_ty)
                    ),
                );
            } else {
                elem_ty = self.unification.resolve(&elem_ty);
            }
        }
        Type::Name("Set".into(), vec![elem_ty])
    }
}
