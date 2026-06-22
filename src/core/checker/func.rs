use crate::ast::*;
use crate::diagnostic::codes;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

use super::Checker;

impl<'a> Checker<'a> {
    pub(crate) fn check_func(&mut self, func: &FuncDef) {
        let ret = func
            .ret
            .as_ref()
            .map(|t| self.resolve_type(t))
            .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
        let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
        // Push function-level variable scope for shadowing detection
        self.var_scopes.push(HashMap::new());
        // Push cap scope for function body
        self.cap_vars.push(HashMap::new());
        for p in &func.params {
            let ty = self.resolve_type(&p.ty);
            // If param is a cap type, track it
            if matches!(&ty, Type::Cap(_)) {
                if let Some(s) = self.cap_vars.last_mut() {
                    s.insert(p.name.clone(), false);
                }
            }
            scopes[0].insert(p.name.clone(), ty);
        }

        // Check for contracts on shared-param functions (E0502)
        let has_shared_param = func.params.iter().any(|p| matches!(&p.ty,
            Type::Shared(_) | Type::LocalShared(_) | Type::CShared(_)
        ));
        if has_shared_param {
            let has_contract = func.body.iter().any(|s| matches!(s,
                Stmt::Requires(..) | Stmt::Ensures(..) | Stmt::Math(_) | Stmt::MmsBlock { .. }
            ));
            if has_contract {
                self.emit_code(codes::E0502, format!(
                    "function '{}' has contracts but takes a shared parameter — Z3 cannot verify heap state",
                    func.name
                ));
            }
        }
        // Comptime functions: type-check body but mark as compile-time evaluable
        if func.is_comptime {
            // Comptime functions can only use pure expressions (no side effects)
            // For now, just type-check the body normally
        }
        // Check all-return-paths requirement
        if !matches!(&ret, Type::Name(n, _) if n == "unit") && !self.block_returns_on_all_paths(&func.body) {
            self.errors.push(
                Diagnostic::error_code(
                    crate::diagnostic::codes::E0255,
                    format!("function '{}' does not return on all paths (missing return in some branches)", func.name),
                    Span::single(self.current_line, self.current_col),
                ).with_help("add a return statement or make the last expression return the appropriate type")
            );
        }
        self.check_block(&func.body, &ret, &mut scopes);
        // Check for unconsumed caps before popping
        self.check_unconsumed_caps();
        self.var_scopes.pop();
        self.cap_vars.pop();
    }

    /// Check if a block returns on all paths
    pub(crate) fn block_returns_on_all_paths(&self, block: &Block) -> bool {
        if block.is_empty() {
            return false;
        }
        // Check if the last statement is an implicit return (expression statement)
        if let Some(last) = block.last() {
            match last {
                Stmt::Return(_) => return true,
                Stmt::Expr(Expr::Match(_, arms)) => {
                    return arms.iter().all(|arm| {
                        let block = vec![Stmt::Expr(arm.body.clone())];
                        self.block_returns_on_all_paths(&block)
                    });
                }
                Stmt::Expr(_) => return true, // implicit return via last expression
                Stmt::If { then_, else_, .. } => {
                    let then_returns = self.block_returns_on_all_paths(then_);
                    let else_returns = else_.as_ref()
                        .map(|e| self.block_returns_on_all_paths(e))
                        .unwrap_or(false);
                    if then_returns && else_returns {
                        return true;
                    }
                }
                Stmt::Block(inner) => {
                    if self.block_returns_on_all_paths(inner) {
                        return true;
                    }
                }
                Stmt::Arena(inner) => {
                    if self.block_returns_on_all_paths(inner) {
                        return true;
                    }
                }
                Stmt::Alloc { kind: _, body } => {
                    if self.block_returns_on_all_paths(body) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    pub(crate) fn check_unconsumed_caps(&mut self) {
        if let Some(scope) = self.cap_vars.last() {
            let unconsumed: Vec<String> = scope.iter()
                .filter(|(_, consumed)| !*consumed)
                .map(|(name, _)| name.clone())
                .collect();
            for name in unconsumed {
                self.emit_code(crate::diagnostic::codes::E0256, format!(
                    "linear capability '{}' must be consumed (via drop) before end of scope",
                    name
                ));
            }
        }
    }
}
