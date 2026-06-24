use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{fmt_type, same_type};
use std::collections::HashMap;

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
                    let expected: HashMap<String, Type> = expected_fields
                        .iter()
                        .map(|f| (f.name.clone(), self.resolve_type(&f.ty)))
                        .collect();
                    for (name, value) in fields.iter().map(|f| (&f.name, &f.value)) {
                        if let Some(expected_ty) = expected.get(name) {
                            let actual_ty = self.infer_expr(value, scopes);
                            if !same_type(expected_ty, &actual_ty) {
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
                    Type::Name(tdef.name.clone(), vec![])
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
            } else if !same_type(&elem_ty, &t) {
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    format!(
                        "list element {} type {} does not match first element {}",
                        i + 1,
                        fmt_type(&t),
                        fmt_type(&elem_ty)
                    ),
                );
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
            self.infer_expr(k, scopes);
            self.infer_expr(v, scopes);
        }
        Type::Name("Record".into(), vec![])
    }
}
