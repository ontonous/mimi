use super::*;

impl<'a> Checker<'a> {
    pub(crate) fn infer_expr(&mut self, expr: &Expr, scopes: &mut Vec<HashMap<String, Type>>) -> Type {
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
            Expr::Comprehension { expr, var, iter, guard } => {
                self.infer_comprehension(expr, var, iter, guard.as_deref(), scopes)
            }
            Expr::Arena(block) => self.infer_block_expr(block, scopes),
            Expr::Block(block) => self.infer_block_expr(block, scopes),
            Expr::If { cond, then_, else_ } => {
                let else_ref = else_.as_ref().map(|b| { let v: &Block = b; v });
                self.infer_if_expr(cond, then_, else_ref, scopes)
            }
            Expr::Index(obj, idx) => self.infer_index(obj, idx, scopes),
            Expr::Try(expr) => self.infer_try_expr(expr, scopes),
            Expr::Spawn(_) => Type::Name("Future".into(), vec![]),
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
            Expr::Lambda { params, ret, body } => self.infer_lambda(params, ret.as_ref(), body, scopes),
            Expr::Turbofish(name, type_args, args) => {
                self.infer_turbofish(name, type_args, args, scopes)
            }
            Expr::MapLiteral { entries } => self.infer_map_literal(entries, scopes),
            Expr::SetLiteral(elems) => self.infer_set_literal(elems, scopes),
            Expr::NamedArg(_, value) => self.infer_expr(value, scopes),
        }
    }
}
