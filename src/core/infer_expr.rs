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
        match expr {
            // C3: None in context of Option<T> → infer Option<T>
            Expr::Literal(Lit::Unit) if matches!(expected, Type::Option(_)) => expected.clone(),
            // C3: bare None (parsed as Ident "None") in Option context
            Expr::Ident(name) if name == "None" => {
                if let Type::Option(_) = expected {
                    expected.clone()
                } else {
                    self.infer_expr(expr, scopes)
                }
            }
            // Empty list literal in List<T> context → infer List<T>
            Expr::List(elems) if elems.is_empty() => {
                if let Type::Name(name, _inner) = expected {
                    if name == "List" {
                        return expected.clone();
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
        match expr {
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
                let base_ty = match &inner_ty {
                    Type::Option(t) => t.as_ref().clone(),
                    Type::Result(ok, _) => ok.as_ref().clone(),
                    _ => inner_ty.clone(),
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
        }
    }
}
