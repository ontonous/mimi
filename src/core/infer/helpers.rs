use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::fmt_type;
use std::collections::HashMap;

impl<'a> Checker<'a> {
    pub(in crate::core) fn infer_block_expr(
        &mut self,
        block: &Block,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        scopes.push(HashMap::new());
        let mut result_type = Type::Name("unit".into(), vec![]);
        for stmt in block {
            match stmt {
                Stmt::Expr(e) => result_type = self.infer_expr(e, scopes),
                Stmt::Return(Some(e)) => {
                    result_type = self.infer_expr(e, scopes);
                    break;
                }
                Stmt::Let { pat, init: Some(e), .. } => {
                    let ty = self.infer_expr(e, scopes);
                    // Bind let variable to scope so subsequent statements can reference it
                    Self::bind_pattern_to_scope(pat, &ty, scopes);
                    result_type = ty;
                }
                _ => {}
            }
        }
        scopes.pop();
        result_type
    }

    /// C3: Bidirectional block checking — check the last expression against expected type.
    pub(in crate::core) fn check_block_expr(
        &mut self,
        block: &Block,
        expected: &Type,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        scopes.push(HashMap::new());
        let mut result_type = Type::Name("unit".into(), vec![]);
        for stmt in block {
            match stmt {
                Stmt::Expr(e) => result_type = self.check_expr(expected, e, scopes),
                Stmt::Return(Some(e)) => {
                    result_type = self.check_expr(expected, e, scopes);
                    break;
                }
                Stmt::Let { pat, init: Some(e), .. } => {
                    let ty = self.infer_expr(e, scopes);
                    Self::bind_pattern_to_scope(pat, &ty, scopes);
                    result_type = ty;
                }
                _ => {}
            }
        }
        scopes.pop();
        result_type
    }

    pub(in crate::core) fn infer_if_expr(
        &mut self,
        cond: &Expr,
        then_: &Block,
        else_: Option<&Block>,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        self.infer_expr(cond, scopes);
        let then_ty = self.infer_block_expr(then_, scopes);
        if let Some(eb) = else_ {
            let else_ty = self.infer_block_expr(eb, scopes);
            // Bug-3: use unify instead of same_type to enable bidirectional type inference.
            // This allows the expected type to propagate into both branches, so
            // `Some(1)` in an `Option<i64>` context can infer i64 from the expected type.
            if self.unification.unify(&then_ty, &else_ty).is_ok() {
                self.unification.resolve(&then_ty)
            } else {
                self.emit_code(
                    crate::diagnostic::codes::E0214,
                    format!(
                        "if/else branches have different types: {} vs {}",
                        fmt_type(&then_ty),
                        fmt_type(&else_ty)
                    ),
                );
                Type::Name("unknown".into(), vec![])
            }
        } else {
            then_ty
        }
    }

    pub(in crate::core) fn infer_comprehension(
        &mut self,
        expr: &Expr,
        var: &str,
        iter: &Expr,
        guard: Option<&Expr>,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let iter_ty = self.infer_expr(iter, scopes);
        // Check iter is a list
        if let Type::Name(n, args) = &iter_ty {
            if n != "List" || args.len() != 1 {
                self.emit_code(
                    crate::diagnostic::codes::E0250,
                    format!(
                        "comprehension requires a list, found {}",
                        fmt_type(&iter_ty)
                    ),
                );
            }
        }
        // Infer element type from iter
        let elem_ty = if let Type::Name(_, args) = &iter_ty {
            if args.len() == 1 {
                args[0].clone()
            } else {
                Type::Name("unknown".into(), vec![])
            }
        } else {
            Type::Name("unknown".into(), vec![])
        };
        // Add var to scope
        if let Some(s) = scopes.last_mut() {
            s.insert(var.to_owned(), elem_ty);
        }
        // Infer expression type
        let expr_ty = self.infer_expr(expr, scopes);
        // Check guard if present
        if let Some(g) = guard {
            let guard_ty = self.infer_expr(g, scopes);
            if !matches!(&guard_ty, Type::Name(n, _) if n == "bool") {
                self.emit_code(
                    crate::diagnostic::codes::E0230,
                    format!(
                        "comprehension guard must be bool, found {}",
                        fmt_type(&guard_ty)
                    ),
                );
            }
        }
        Type::Name("List".into(), vec![expr_ty])
    }

    pub(in crate::core) fn infer_slice(
        &mut self,
        target: &Expr,
        start: Option<&Expr>,
        end: Option<&Expr>,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let target_ty = self.infer_expr(target, scopes);
        if let Some(s) = start {
            let _ = self.infer_expr(s, scopes);
        }
        if let Some(e) = end {
            let _ = self.infer_expr(e, scopes);
        }
        Type::Slice(Box::new(target_ty))
    }

    pub(in crate::core) fn infer_range(
        &mut self,
        start: &Expr,
        end: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let _ = self.infer_expr(start, scopes);
        let _ = self.infer_expr(end, scopes);
        Type::Name("Range".into(), vec![])
    }

    pub(in crate::core) fn infer_await(
        &mut self,
        inner: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let inner_ty = self.infer_expr(inner, scopes);
        match inner_ty {
            Type::Name(n, args) if n == "Future" && !args.is_empty() => args[0].clone(),
            other => {
                self.emit_code(
                    crate::diagnostic::codes::E0245,
                    format!("await requires Future type, found {}", fmt_type(&other)),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    pub(in crate::core) fn infer_try_expr(
        &mut self,
        expr: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let inner_ty = self.infer_expr(expr, scopes);
        match inner_ty {
            // Built-in Result<T, E> -> ? extracts T
            Type::Name(n, args) if n == "Result" && args.len() == 2 => args[0].clone(),
            // Built-in Option<T> -> ? extracts T
            Type::Name(n, args) if n == "Option" && args.len() == 1 => args[0].clone(),
            // T? syntactic sugar for Option<T>
            Type::Option(inner) => (*inner).clone(),
            // For unparameterized enum types like `Res`, look up the type definition
            Type::Name(name, ref args) if args.is_empty() => {
                if let Some(tdef) = self.types.get(&name) {
                    match &tdef.kind {
                        TypeDefKind::Enum(variants) if variants.len() == 2 => {
                            // Try to find Ok/Err or Some/None pattern
                            let first_variant = &variants[0];
                            match &first_variant.payload {
                                Some(VariantPayload::Tuple(types)) if !types.is_empty() => {
                                    types[0].clone()
                                }
                                _ => {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0224,
                                        format!(
                                            "? operator: cannot determine success type from enum '{}'",
                                            name
                                        ),
                                    );
                                    Type::Name("unknown".into(), vec![])
                                }
                            }
                        }
                        _ => {
                            self.emit_code(
                                crate::diagnostic::codes::E0224,
                                format!(
                                    "? operator requires Result or Option type, found '{}'",
                                    name
                                ),
                            );
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                } else {
                    self.emit_code(
                        crate::diagnostic::codes::E0224,
                        format!(
                            "? operator requires Result or Option type, found '{}'",
                            name
                        ),
                    );
                    Type::Name("unknown".into(), vec![])
                }
            }
            Type::Infer => {
                // _ type in let binding: infer from init expression
                Type::Name("unknown".into(), vec![])
            }
            _ => {
                self.emit_code(
                    crate::diagnostic::codes::E0224,
                    format!(
                        "? operator requires Result or Option type, found {}",
                        fmt_type(&inner_ty)
                    ),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    /// Bind a pattern's variables to the current type-checking scope.
    /// Supports Variable, Tuple, and Wildcard patterns.
    fn bind_pattern_to_scope(
        pat: &Pattern,
        ty: &Type,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) {
        match pat {
            Pattern::Variable(name) => {
                if let Some(s) = scopes.last_mut() {
                    s.insert(name.clone(), ty.clone());
                }
            }
            Pattern::Tuple(pats) => {
                let types = match ty {
                    Type::Tuple(ts) => Some(ts.as_slice()),
                    Type::Name(n, args) if n == "Tuple" => Some(args.as_slice()),
                    _ => None,
                };
                if let Some(types) = types {
                    for (p, t) in pats.iter().zip(types.iter()) {
                        Self::bind_pattern_to_scope(p, t, scopes);
                    }
                }
            }
            Pattern::Constructor(_, subpatterns) => {
                for sp in subpatterns {
                    Self::bind_pattern_to_scope(sp, ty, scopes);
                }
            }
            Pattern::Array(pats) => {
                for p in pats {
                    Self::bind_pattern_to_scope(p, ty, scopes);
                }
            }
            // Wildcard, Literal, Slice — no variables to bind
            _ => {}
        }
    }

    pub(in crate::core) fn infer_comptime(
        &mut self,
        block: &Block,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        // Comptime block: infer type from last expression
        let mut result_type = Type::Name("unit".into(), vec![]);
        for stmt in block {
            match stmt {
                Stmt::Expr(e) => result_type = self.infer_expr(e, scopes),
                Stmt::Return(Some(e)) => {
                    result_type = self.infer_expr(e, scopes);
                    break;
                }
                _ => {}
            }
        }
        result_type
    }
}
