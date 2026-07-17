use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::fmt_type;
use std::collections::HashMap;

impl<'a> Checker<'a> {
    /// Common scaffolding for block expressions: push/pop all needed scope
    /// stacks and type-check every statement. The caller decides how to obtain
    /// the result type from the last statement.
    fn process_block_expr<F>(
        &mut self,
        block: &Block,
        scopes: &mut Vec<HashMap<String, Type>>,
        process_last: F,
    ) -> Type
    where
        F: FnOnce(&mut Self, &Stmt, &mut Vec<HashMap<String, Type>>, &Type) -> Type,
    {
        if block.is_empty() {
            return Type::Name("unit".into(), vec![]);
        }
        let ret = self
            .current_ret
            .clone()
            .unwrap_or_else(|| Type::Name("unit".into(), vec![]));

        // Mirror the scope setup used by check_block_with_implicit_return so
        // that borrow tracking, cap tracking and shadowing detection are all
        // active inside a block expression.
        self.var_scopes.push(HashMap::new());
        self.mut_vars.push(HashMap::new());
        scopes.push(HashMap::new());
        self.cap_vars.push(HashMap::new());
        self.push_borrow_scope();

        let last_idx = block.len() - 1;
        for (i, stmt) in block.iter().enumerate() {
            if i == last_idx {
                break;
            }
            self.check_stmt(stmt, &ret, scopes);
        }

        let last = &block[last_idx];
        let result_type = process_last(self, last, scopes, &ret);

        self.check_unconsumed_caps();
        self.pop_borrow_scope();
        self.cap_vars.pop();
        scopes.pop();
        self.mut_vars.pop();
        self.var_scopes.pop();
        result_type
    }

    pub(in crate::core) fn infer_block_expr(
        &mut self,
        block: &Block,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        self.process_block_expr(block, scopes, |this, last, scopes, ret| match last {
            Stmt::Expr(e) => this.infer_expr(e, scopes),
            Stmt::Return(Some(e)) => {
                let t = this.infer_expr(e, scopes);
                this.check_stmt(last, ret, scopes);
                t
            }
            Stmt::Return(None) => {
                this.check_stmt(last, ret, scopes);
                Type::Name("unit".into(), vec![])
            }
            Stmt::If { cond, then_, else_ } => {
                this.infer_if_expr(cond, then_, else_.as_ref(), scopes)
            }
            Stmt::Block(inner) => this.infer_block_expr(inner, scopes),
            _ => {
                this.check_stmt(last, ret, scopes);
                Type::Name("unit".into(), vec![])
            }
        })
    }

    /// C3: Bidirectional block checking — check the last expression against expected type.
    pub(in crate::core) fn check_block_expr(
        &mut self,
        block: &Block,
        expected: &Type,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        self.process_block_expr(block, scopes, |this, last, scopes, ret| match last {
            Stmt::Expr(e) => this.check_expr(expected, e, scopes),
            Stmt::Return(Some(e)) => {
                let t = this.check_expr(expected, e, scopes);
                this.check_stmt(last, ret, scopes);
                t
            }
            Stmt::Return(None) => {
                this.check_stmt(last, ret, scopes);
                Type::Name("unit".into(), vec![])
            }
            Stmt::If { cond, then_, else_ } => {
                let if_expr = Expr::If {
                    cond: Box::new(cond.clone()),
                    then_: then_.clone(),
                    else_: else_.clone(),
                };
                this.check_expr(expected, &if_expr, scopes)
            }
            Stmt::Block(inner) => this.check_block_expr(inner, expected, scopes),
            _ => {
                this.check_stmt(last, ret, scopes);
                Type::Name("unit".into(), vec![])
            }
        })
    }

    pub(in crate::core) fn infer_if_expr(
        &mut self,
        cond: &Expr,
        then_: &Block,
        else_: Option<&Block>,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        self.infer_expr(cond, scopes);
        let entry_caps = self.cap_vars.clone();
        self.ownership_control_path.push("expr-then".to_string());
        let then_ty = self.infer_block_expr(then_, scopes);
        self.ownership_control_path.pop();
        let then_caps = self.cap_vars.clone();
        self.cap_vars = entry_caps.clone();
        if let Some(eb) = else_ {
            self.ownership_control_path.push("expr-else".to_string());
            let else_ty = self.infer_block_expr(eb, scopes);
            self.ownership_control_path.pop();
            let else_caps = self.cap_vars.clone();
            self.cap_vars = entry_caps;
            self.merge_capability_branches(&then_caps, &else_caps);
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
            self.cap_vars = entry_caps;
            self.merge_capability_branches(&then_caps, &self.cap_vars.clone());
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
        // Add var to scope, then remove it after inference to prevent
        // audit (MEDIUM): comprehension variable leak into outer scope.
        // The loop variable `var` must not be visible after the comprehension
        // expression completes.
        let old_var = scopes.last_mut().and_then(|s| s.remove(var));
        if let Some(s) = scopes.last_mut() {
            s.insert(var.to_owned(), elem_ty);
        }
        // Infer expression type
        let expr_ty = self.infer_expr(expr, scopes);
        // Check guard if present — MUST be done while var is still in scope,
        // because the guard references the loop variable.
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
        // NOW restore old binding (or remove if it didn't exist before)
        if let Some(s) = scopes.last_mut() {
            s.remove(var);
            if let Some(old) = old_var {
                s.insert(var.to_owned(), old);
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
