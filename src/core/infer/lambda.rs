use crate::ast::*;
use crate::core::checker::Checker;
use std::collections::HashMap;

impl<'a> Checker<'a> {
    pub(in crate::core) fn infer_lambda(
        &mut self,
        params: &[Param],
        ret: Option<&Type>,
        body: &Block,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        // CO-C1 / H16: unannotated (`_`) params become fresh TypeVars so that
        // `let id = fn(x: _) { x }` can be generalized to ∀T. T → T.
        let param_types: Vec<Type> = params
            .iter()
            .map(|p| {
                let ty = self.resolve_type(&p.ty);
                if matches!(ty.unlocated(), Type::Infer) {
                    self.fresh_var()
                } else {
                    ty
                }
            })
            .collect();
        scopes.push(HashMap::new());
        for (p, ty) in params.iter().zip(param_types.iter()) {
            if let Some(s) = scopes.last_mut() {
                s.insert(p.name.clone(), ty.clone());
            }
        }
        let mut body_type = Type::Name("unit".into(), vec![]);
        for stmt in body {
            match stmt.unlocated() {
                Stmt::Expr(e) => body_type = self.infer_expr(e, scopes),
                Stmt::Return(Some(e)) => {
                    body_type = self.infer_expr(e, scopes);
                    break;
                }
                _ => {
                    // Process let/if/while/for/match etc. for their side effects
                    // on scope bindings. Only the last expression determines the
                    // lambda's return type; these statements return unit.
                    let unit = Type::Name("unit".into(), vec![]);
                    self.check_stmt(stmt, &unit, scopes);
                    body_type = Type::Name("unit".into(), vec![]);
                }
            }
        }
        scopes.pop();
        let return_type = match ret {
            Some(r) => {
                let rty = self.resolve_type(r);
                if matches!(rty.unlocated(), Type::Infer) {
                    body_type
                } else {
                    let body_type = self.unification.resolve(&body_type);
                    self.unify_types(&rty, &body_type);
                    rty
                }
            }
            None => body_type,
        };
        Type::Func(param_types, Box::new(return_type))
    }
}
