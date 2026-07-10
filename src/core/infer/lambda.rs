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
        let param_types: Vec<Type> = params.iter().map(|p| self.resolve_type(&p.ty)).collect();
        scopes.push(HashMap::new());
        for p in params {
            if let Some(s) = scopes.last_mut() {
                s.insert(p.name.clone(), self.resolve_type(&p.ty));
            }
        }
        let mut body_type = Type::Name("unit".into(), vec![]);
        for stmt in body {
            match stmt {
                Stmt::Expr(e) => body_type = self.infer_expr(e, scopes),
                Stmt::Return(Some(e)) => {
                    body_type = self.infer_expr(e, scopes);
                    break;
                }
                other => {
                    // Process let/if/while/for/match etc. for their side effects
                    // on scope bindings. Only the last expression determines the
                    // lambda's return type; these statements return unit.
                    let unit = Type::Name("unit".into(), vec![]);
                    self.check_stmt(other, &unit, scopes);
                    body_type = Type::Name("unit".into(), vec![]);
                }
            }
        }
        scopes.pop();
        let return_type = ret.cloned().unwrap_or(body_type);
        Type::Func(param_types, Box::new(return_type))
    }
}
