use super::*;

impl<'a> Checker<'a> {
    /// C3: Bidirectional type checking — check an expression against an expected type.
    ///
    /// When the expected type is known, this propagates context downward, enabling
    /// inference of `None`/`Ok`/`Err` from context. Falls back to `infer_expr` when
    /// the expression cannot benefit from the expected type.
    pub(crate) fn check_expr(
        &mut self,
        expected: &Type,
        expr: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let previous_span = self.replace_span(expr.meta().map(|meta| meta.span));
        let result = self.check_expr_inner(expected, expr, scopes);
        self.set_span(previous_span);
        self.record_expression_type(expr, &result);
        result
    }

    fn check_expr_inner(
        &mut self,
        expected: &Type,
        expr: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        match expr.unlocated() {
            // C3: None in context of Option<T> → infer Option<T>
            Expr::Literal(Lit::Unit) if matches!(expected.unlocated(), Type::Option(_)) => {
                expected.clone()
            }
            // C3: bare None (parsed as Ident "None") in Option context
            Expr::Ident(name) if name == "None" => {
                let is_option = match expected.unlocated() {
                    Type::Option(_) => true,
                    Type::Name(name, arguments) => name == "Option" && arguments.len() == 1,
                    _ => false,
                };
                if is_option {
                    expected.clone()
                } else {
                    self.infer_expr(expr, scopes)
                }
            }
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.unlocated() {
                    let option_inner = match expected.unlocated() {
                        Type::Option(inner) => Some(inner.as_ref()),
                        Type::Name(name, arguments) if name == "Option" && arguments.len() == 1 => {
                            arguments.first()
                        }
                        _ => None,
                    };
                    let result_parts = match expected.unlocated() {
                        Type::Result(ok, error) => Some((ok.as_ref(), error.as_ref())),
                        Type::Name(name, arguments) if name == "Result" && arguments.len() == 2 => {
                            Some((&arguments[0], &arguments[1]))
                        }
                        _ => None,
                    };
                    if name == "Some" && args.len() == 1 {
                        if let Some(inner) = option_inner {
                            self.check_expr(inner, &args[0], scopes);
                            return expected.clone();
                        }
                    } else if name == "None" && args.is_empty() && option_inner.is_some() {
                        return expected.clone();
                    } else if name == "Ok" && args.len() == 1 {
                        if let Some((ok, _)) = result_parts {
                            self.check_expr(ok, &args[0], scopes);
                            return expected.clone();
                        }
                    } else if name == "Err" && args.len() == 1 {
                        if let Some((_, error)) = result_parts {
                            self.check_expr(error, &args[0], scopes);
                            return expected.clone();
                        }
                    }
                }
                self.infer_expr(expr, scopes)
            }
            // List literal in List / List<T> context:
            // empty → expected; non-empty List<T> → check each elem against T.
            Expr::List(elems) => {
                if let Type::Name(name, inner) = expected.unlocated() {
                    if name == "List" {
                        if elems.is_empty() {
                            return expected.clone();
                        }
                        if !inner.is_empty() {
                            let elem_expected = &inner[0];
                            for (i, e) in elems.iter().enumerate() {
                                let actual = self.check_expr(elem_expected, e, scopes);
                                let ok = self.unification.unify(&actual, elem_expected).is_ok()
                                    || crate::core::helpers::is_numeric_coercion(
                                        elem_expected,
                                        &actual,
                                    );
                                if !ok {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0242,
                                        format!(
                                            "list element {} type {} does not match expected {}",
                                            i + 1,
                                            crate::core::helpers::fmt_type(&actual),
                                            crate::core::helpers::fmt_type(elem_expected)
                                        ),
                                    );
                                }
                            }
                            return expected.clone();
                        }
                    }
                }
                self.infer_expr(expr, scopes)
            }
            // C3: block — check last expression against expected type and
            // ensure every intermediate statement is type-checked.
            Expr::Block(block) => self.check_block_expr(block, expected, scopes),
            // C3: if — check both branches against expected type
            Expr::If { then_, else_, .. } => {
                // Use check_block_expr to propagate expected type to branches
                let then_ty = self.check_block_expr(then_, expected, scopes);
                if let Some(else_block) = else_ {
                    let else_ty = self.check_block_expr(else_block, expected, scopes);
                    // Unify both branches
                    if self.unification.unify(&then_ty, &else_ty).is_err() {
                        self.emit_code(
                            crate::diagnostic::codes::E0214,
                            format!(
                                "if/else branches have different types: {} vs {}",
                                crate::core::helpers::fmt_type(&then_ty),
                                crate::core::helpers::fmt_type(&else_ty)
                            ),
                        );
                    }
                    self.unification.resolve(&then_ty)
                } else {
                    then_ty
                }
            }
            // For all other expressions, fall back to inference
            _ => self.infer_expr(expr, scopes),
        }
    }

    pub(crate) fn infer_expr(
        &mut self,
        expr: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let previous_span = self.replace_span(expr.meta().map(|meta| meta.span));
        let result = self.infer_expr_inner(expr, scopes);
        self.set_span(previous_span);
        // The AstNodeMeta migration wraps types in `Type::Located { meta, ty }`.
        // Inference results are consumed by `matches!` and `unify` which expect
        // kind variants; strip the wrapper so downstream type discrimination
        // (len/sort/is_empty/...) sees the underlying Type::Name directly.
        let result = result.into_unlocated();
        self.record_expression_type(expr, &result);
        result
    }

    fn infer_expr_inner(&mut self, expr: &Expr, scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        match expr.unlocated() {
            Expr::Literal(Lit::FString(parts)) => {
                for part in parts {
                    if let FStringPart::Interp(expression) = part {
                        self.infer_expr(expression, scopes);
                    }
                }
                Type::Name("string".into(), Vec::new())
            }
            Expr::Literal(l) => self.infer_literal(l),
            Expr::Ident(name) => self.lookup_var(name, scopes),
            Expr::Call(callee, args) => self.infer_call_expr(callee, args, scopes),
            Expr::Field(obj, field) => self.infer_field_access(obj, field, scopes),
            Expr::Record { ty, fields } => self.infer_record_expr(ty, fields, scopes),
            Expr::Match(target, arms) => self.infer_match_expr(target, arms, scopes),
            Expr::Unary(op, e) => self.infer_unary(*op, e, scopes),
            Expr::Binary(op, l, r) => self.infer_binary(*op, l, r, scopes),
            Expr::Tuple(elems) => self.infer_tuple_expr(elems, scopes),
            Expr::TupleIndex(obj, idx) => self.infer_tuple_index(obj, *idx, scopes),
            Expr::List(elems) => self.infer_list_expr(elems, scopes),
            Expr::Comprehension {
                expr,
                var,
                iter,
                guard,
            } => self.infer_comprehension(expr, var, iter, guard.as_deref(), scopes),
            Expr::Arena(block) => self.infer_block_expr(block, scopes),
            Expr::Block(block) => self.infer_block_expr(block, scopes),
            Expr::If { cond, then_, else_ } => {
                let else_ref = else_.as_ref().map(|b| {
                    let v: &Block = b;
                    v
                });
                self.infer_if_expr(cond, then_, else_ref, scopes)
            }
            Expr::Index(obj, idx) => self.infer_index(obj, idx, scopes),
            Expr::Try(expr) => self.infer_try_expr(expr, scopes),
            // PA-H3 (audit): optional chain `x?.y` — type is `Option<field_type>`.
            // H1 (core audit): actually resolve the field type instead of
            // always returning Option<unknown> (which silently accepted
            // x?.non_existent_field).
            Expr::OptionalChain(inner, field) => {
                let inner_ty = self.infer_expr(inner, scopes);
                // Normalize both Type::Option/Result and Type::Name("Option"/"Result", …).
                let base_ty = match inner_ty.unlocated() {
                    Type::Option(t) => t.as_ref().clone(),
                    Type::Result(ok, _) => ok.as_ref().clone(),
                    Type::Name(n, args) if n == "Option" && args.len() == 1 => args[0].clone(),
                    Type::Name(n, args) if n == "Result" && !args.is_empty() => args[0].clone(),
                    _ => {
                        // May still be a TypeVar unified to Option later — try resolve.
                        let resolved = self.unification.resolve(&inner_ty);
                        match resolved.unlocated() {
                            Type::Option(t) => t.as_ref().clone(),
                            Type::Result(ok, _) => ok.as_ref().clone(),
                            Type::Name(n, args) if n == "Option" && args.len() == 1 => {
                                args[0].clone()
                            }
                            Type::Name(n, args) if n == "Result" && !args.is_empty() => {
                                args[0].clone()
                            }
                            _ => inner_ty.clone(),
                        }
                    }
                };
                let field_ty = self.infer_field_access_on_type(&base_ty, field, scopes);
                Type::Option(Box::new(field_ty))
            }
            Expr::Spawn(inner) => {
                let inner_ty = self.infer_expr(inner, scopes);
                Type::Name("Future".into(), vec![inner_ty])
            }
            Expr::Await(inner) => self.infer_await(inner, scopes),
            Expr::Quote(_) => Type::Name("AST".into(), vec![]),
            Expr::QuoteInterpolate(inner) => self.infer_expr(inner, scopes),
            Expr::Comptime(block) => self.infer_comptime(block, scopes),
            Expr::TypeOf(_) => Type::Name("Type".into(), vec![]),
            Expr::SliceExpr { target, start, end } => {
                self.infer_slice(target, start.as_deref(), end.as_deref(), scopes)
            }
            Expr::Range { start, end } => self.infer_range(start, end, scopes),
            Expr::TypeInfo(_) => Type::Name("TypeInfo".into(), vec![]),
            Expr::Old(expr) => self.infer_expr(expr, scopes),
            Expr::Lambda { params, ret, body } => {
                self.infer_lambda(params, ret.as_ref(), body, scopes)
            }
            Expr::Turbofish(name, type_args, args) => {
                self.infer_turbofish(name, type_args, args, scopes)
            }
            Expr::MapLiteral { entries } => self.infer_map_literal(entries, scopes),
            Expr::SetLiteral(elems) => self.infer_set_literal(elems, scopes),
            Expr::NamedArg(_, value) => self.infer_expr(value, scopes),
            Expr::Cast(inner, target_type) => {
                let _ = self.infer_expr(inner, scopes);
                self.resolve_type(target_type)
            }
            Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
        }
    }
}
