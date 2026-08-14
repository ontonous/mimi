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
        match callee.unlocated() {
            Expr::Ident(name) => self.check_call(name, args, scopes),
            Expr::Field(obj, method_name) => self.infer_method_call(obj, method_name, args, scopes),
            _ => {
                // 0.36.28: infer the callee expression itself first so its
                // own diagnostics surface instead of being masked by the
                // call-shape error — e.g. `x?.to_string()` on a plain i32
                // must report E0224 (`?.` requires Option/Result receiver)
                // as well as the final "callee must be a function name".
                // The callee's type is discarded: a non-Ident/Field callee
                // is never a valid callable, so the verdict stands.
                let _ = self.infer_expr(callee, scopes);
                self.emit_code(
                    crate::diagnostic::codes::E0223,
                    "callee must be a function name",
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }
}
