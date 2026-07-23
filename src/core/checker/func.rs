use crate::ast::*;
use crate::core::helpers::{fmt_type, is_numeric_coercion};
use crate::diagnostic::codes;
use crate::diagnostic::Diagnostic;
use std::collections::HashMap;

use super::Checker;

impl<'a> Checker<'a> {
    pub(crate) fn check_func(&mut self, func: &FuncDef) {
        self.set_span(func.meta.span);
        // Function generic binders remain in scope while checking local type
        // annotations in the body (`let xs: List<T> = ...`). Declaration
        // collection already establishes this scope for the signature; body
        // checking must mirror it instead of treating `T` as an unknown type.
        let generic_scope_len = self.generic_scope.len();
        self.generic_scope
            .extend(func.generics.iter().map(|generic| generic.name.clone()));
        let owner_name = if self.module_path.is_empty() {
            func.name.clone()
        } else {
            format!("{}::{}", self.module_path.join("::"), func.name)
        };
        let owner = crate::core::NodeId(format!("function:{}", owner_name));
        self.current_callable_owner = Some(owner.clone());
        self.begin_expression_type_capture(owner.clone());
        // C2: reset unification table for each function
        self.unification.reset();
        // v0.29.19: session residual tracking is per-function.
        self.session_residuals.clear();
        // v0.29.23: view/mutate param borrow sets.
        self.view_params.clear();
        self.mutate_params.clear();
        // FLOW-IDENTITY-001: linear generation — per-function consumption tracking.
        self.consumed_flow_vars.clear();
        for p in &func.params {
            match p.borrow {
                Some(crate::ast::ParamBorrow::View) => {
                    self.view_params.insert(p.name.clone());
                }
                Some(crate::ast::ParamBorrow::Mutate) => {
                    self.mutate_params.insert(p.name.clone());
                }
                None => {}
            }
        }
        // E0402: duplicate parameter names are a user-facing checker diagnostic
        // with a precise span. The IR-level `ResolvedSignature::validate`
        // uniqueness check remains a fail-closed safety net, but it surfaces as
        // a code-less TOOL-RESOLUTION-001 error; the canonical error code must
        // originate here.
        let mut seen_param_names: Vec<&str> = Vec::new();
        for p in &func.params {
            if seen_param_names.contains(&p.name.as_str()) {
                self.errors.push(Diagnostic::error_code(
                    codes::E0402,
                    format!(
                        "duplicate parameter name '{}' in function '{}'",
                        p.name, func.name
                    ),
                    p.meta.span,
                ));
            } else {
                seen_param_names.push(p.name.as_str());
            }
        }
        let ret = func
            .ret
            .as_ref()
            .map(|t| self.resolve_type(t))
            .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
        self.current_ret = Some(ret.clone());
        let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
        // Push function-level variable scope for shadowing detection
        self.var_scopes.push(HashMap::new());
        for p in &func.params {
            let ty = self.resolve_type(&p.ty);
            // SessionChan<S> params: seed residual from declared session body.
            if let Type::Name(n, args) = ty.unlocated() {
                if (n == "SessionChan" || n == "session_chan") && !args.is_empty() {
                    if let Type::Name(sname, _) = args[0].unlocated() {
                        if let Some(body) = self.session_types.get(sname).cloned() {
                            let resolved =
                                crate::session::resolve(&body, &self.session_types).unwrap_or(body);
                            self.session_residuals.insert(p.name.clone(), resolved);
                        }
                    }
                }
            }
            scopes[0].insert(p.name.clone(), ty);
            // Track mutable parameters for assignment checking
            if let Some(s) = self.mut_vars.last_mut() {
                s.insert(p.name.clone(), p.mut_);
            }
        }

        // Default expressions are declaration-owned typed artifacts. Capture
        // them under the callee while its parameters and generic binders are
        // in scope, rather than re-checking cloned syntax at each call site.
        for parameter in &func.params {
            if let Some(default) = &parameter.default_value {
                let expected = self.resolve_type(&parameter.ty);
                let actual = self.check_expr(&expected, default, &mut scopes);
                if self.unification.unify(&actual, &expected).is_err()
                    && !is_numeric_coercion(&expected, &actual)
                {
                    self.errors.push(Diagnostic::error_code(
                        codes::E0211,
                        format!(
                            "default for parameter '{}' expected {}, found {}",
                            parameter.name,
                            fmt_type(&expected),
                            fmt_type(&actual)
                        ),
                        parameter.meta.span,
                    ));
                }
            }
        }

        // Check for contracts on shared-param functions (E0502)
        let has_shared_param = func.params.iter().any(|p| {
            matches!(
                p.ty.unlocated(),
                Type::Shared(_) | Type::LocalShared(_) | Type::CShared(_)
            )
        });
        if has_shared_param {
            let has_contract = func.body.iter().any(|s| {
                matches!(
                    s.unlocated(),
                    Stmt::Requires(..)
                        | Stmt::Ensures(..)
                        | Stmt::Invariant(..)
                        | Stmt::Math(_)
                        | Stmt::MmsBlock { .. }
                )
            });
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
        // Make function's own effects available in its body
        let mut effects_scope = HashMap::new();
        for effect in &func.effects {
            effects_scope.insert(effect.clone(), true);
        }
        self.available_effects.push(effects_scope);
        // Check all-return-paths requirement
        if !matches!(ret.unlocated(), Type::Name(n, _) if n == "unit")
            && !self.block_returns_on_all_paths(&func.body)
        {
            self.errors.push(
                Diagnostic::error_code(
                    crate::diagnostic::codes::E0255,
                    format!("function '{}' does not return on all paths (missing return in some branches)", func.name),
                    self.diagnostic_span(),
                ).with_help("add a return statement or make the last expression return the appropriate type")
            );
        }
        // check_block_with_implicit_return returns the type of the last expression
        // to avoid redundant re-checking (refactoring: eliminate double traversal)
        let implicit_return_ty =
            self.check_block_with_implicit_return(&func.body, &ret, &mut scopes);
        // Implicit return type check: last expression must match declared return type
        if let Some(last_ty) = implicit_return_ty {
            // Resolve through unification table before further comparison
            let last_ty = self.unification.resolve(&last_ty);
            // Unwrap shared/aliasing wrappers for return type compatibility
            let last_ty_clean = match last_ty.unlocated() {
                Type::Shared(i) | Type::LocalShared(i) | Type::CShared(i) => (**i).clone(),
                _ => last_ty.clone(),
            };
            let coerced = is_numeric_coercion(&ret, &last_ty_clean);
            let type_ok = coerced || self.unification.unify(&ret, &last_ty_clean).is_ok();
            if !type_ok && !matches!(ret.unlocated(), Type::Name(n, _) if n == "unit") {
                self.errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0207,
                        format!("implicit return: expected {}, found {}", fmt_type(&ret), fmt_type(&last_ty)),
                        self.diagnostic_span(),
                    ).with_help("the last expression in a function body is implicitly returned; make sure its type matches the declared return type")
                );
            }
        }
        self.available_effects.pop();
        self.var_scopes.pop();
        self.finish_expression_type_capture();
        self.current_ret = None;
        self.current_callable_owner = None;
        self.generic_scope.truncate(generic_scope_len);
    }

    /// Check if a block returns on all paths
    pub(crate) fn block_returns_on_all_paths(&self, block: &Block) -> bool {
        if block.is_empty() {
            return false;
        }
        // Check if the last statement is an implicit return (expression statement)
        if let Some(last) = block.last() {
            match last.unlocated() {
                Stmt::Return(_) | Stmt::Become(_) | Stmt::Stay => return true,
                Stmt::Expr(expr) => {
                    if let Expr::Match(_, arms) = expr.unlocated() {
                        return arms.iter().all(|arm| {
                            let meta = arm
                                .body
                                .meta()
                                .map(|meta| {
                                    AstNodeMeta::new(
                                        meta.span,
                                        AstOrigin::Desugared("checker.match_arm.return_analysis"),
                                    )
                                })
                                .unwrap_or_else(|| {
                                    AstNodeMeta::synthetic(AstOrigin::Desugared(
                                        "checker.match_arm.return_analysis",
                                    ))
                                });
                            let block = vec![Stmt::Expr(arm.body.clone()).with_meta(meta)];
                            self.block_returns_on_all_paths(&block)
                        });
                    }
                    return true; // implicit return via last expression
                }
                Stmt::If { then_, else_, .. } => {
                    let then_returns = self.block_returns_on_all_paths(then_);
                    let else_returns = else_
                        .as_ref()
                        .map(|e| self.block_returns_on_all_paths(e))
                        .unwrap_or(false);
                    if then_returns && else_returns {
                        return true;
                    }
                }
                Stmt::Block(inner) | Stmt::Do(inner) => {
                    if self.block_returns_on_all_paths(inner) {
                        return true;
                    }
                }
                Stmt::Arena(inner) => {
                    if self.block_returns_on_all_paths(inner) {
                        return true;
                    }
                }
                Stmt::Alloc { kind: _, body } if self.block_returns_on_all_paths(body) => {
                    return true;
                }
                Stmt::Loop(body) => {
                    if self.block_returns_on_all_paths(body) {
                        return true;
                    }
                }
                Stmt::While { body, .. } => {
                    if self.block_returns_on_all_paths(body) {
                        return true;
                    }
                }
                Stmt::WhileLet { body, .. } => {
                    if self.block_returns_on_all_paths(body) {
                        return true;
                    }
                }
                Stmt::For { body, .. } if self.block_returns_on_all_paths(body) => {
                    return true;
                }
                _ => {}
            }
        }
        false
    }
}
