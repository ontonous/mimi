use crate::ast::*;
use crate::core::checker::Checker;
use std::collections::HashMap;

mod helpers;
mod method;
mod simple;

impl<'a> Checker<'a> {
    pub(in crate::core) fn infer_call_expr(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        match callee {
            Expr::Ident(name) => self.check_call(name, args, scopes),
            Expr::Field(obj, method_name) => {
                self.infer_method_call(obj, method_name, args, scopes)
            }
            _ => {
                self.emit_code(
                    crate::diagnostic::codes::E0223,
                    "callee must be a function name",
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }
}
