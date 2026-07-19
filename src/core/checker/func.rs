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
        self.current_ownership_owner = Some(owner.clone());
        self.ownership_ledgers
            .entry(owner.clone())
            .or_insert_with(|| crate::core::OwnershipLedger::new(owner));
        // C2: reset unification table for each function
        self.unification.reset();
        // v0.29.19: session residual tracking is per-function.
        self.session_residuals.clear();
        // v0.29.23: view/mutate param borrow sets.
        self.view_params.clear();
        self.mutate_params.clear();
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
        let ret = func
            .ret
            .as_ref()
            .map(|t| self.resolve_type(t))
            .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
        self.current_ret = Some(ret.clone());
        let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
        // Push function-level variable scope for shadowing detection
        self.var_scopes.push(HashMap::new());
        // Push cap scope for function body
        self.cap_vars.push(HashMap::new());
        for p in &func.params {
            let ty = self.resolve_type(&p.ty);
            // If param is a cap type, track it
            if matches!(ty.unlocated(), Type::Cap(_)) {
                if let Some(s) = self.cap_vars.last_mut() {
                    s.insert(
                        p.name.clone(),
                        super::CapVarInfo {
                            consumed: false,
                            maybe_consumed: false,
                        },
                    );
                }
                self.record_resource_action(crate::core::ResourceActionKind::Introduce, &p.name);
            }
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
        if let Some(stmt) = func.body.last() {
            if let Stmt::Expr(expr) = stmt.unlocated() {
                self.consume_capabilities_in_expr(expr, crate::core::ResourceActionKind::Return);
            }
        }
        // Check for unconsumed caps before popping
        self.check_unconsumed_caps();
        self.available_effects.pop();
        self.var_scopes.pop();
        self.cap_vars.pop();
        self.current_ret = None;
        self.current_ownership_owner = None;
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
                Stmt::Return(_) => return true,
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

    /// Conservative: body has no `continue` and every path ends in `break`/`return`.
    /// Such bodies cannot form a capability-carrying back-edge.
    pub(crate) fn block_exits_loop_without_backedge(&self, block: &Block) -> bool {
        if block.iter().any(|stmt| self.stmt_contains_continue(stmt)) {
            return false;
        }
        block
            .last()
            .map(|stmt| self.stmt_always_exits_loop(stmt))
            .unwrap_or(false)
    }

    fn stmt_contains_continue(&self, stmt: &Stmt) -> bool {
        match stmt.unlocated() {
            Stmt::Continue => true,
            Stmt::If { then_, else_, .. } => {
                then_.iter().any(|s| self.stmt_contains_continue(s))
                    || else_
                        .as_ref()
                        .is_some_and(|b| b.iter().any(|s| self.stmt_contains_continue(s)))
            }
            Stmt::Block(b)
            | Stmt::Do(b)
            | Stmt::Arena(b)
            | Stmt::Loop(b)
            | Stmt::While { body: b, .. }
            | Stmt::WhileLet { body: b, .. }
            | Stmt::For { body: b, .. }
            | Stmt::Alloc { body: b, .. } => b.iter().any(|s| self.stmt_contains_continue(s)),
            _ => false,
        }
    }

    fn stmt_always_exits_loop(&self, stmt: &Stmt) -> bool {
        match stmt.unlocated() {
            Stmt::Break(_) | Stmt::Return(_) => true,
            Stmt::Continue => false,
            Stmt::If { then_, else_, .. } => {
                self.block_exits_loop_without_backedge(then_)
                    && else_
                        .as_ref()
                        .is_some_and(|b| self.block_exits_loop_without_backedge(b))
            }
            Stmt::Block(b) | Stmt::Do(b) | Stmt::Arena(b) | Stmt::Alloc { body: b, .. } => {
                self.block_exits_loop_without_backedge(b)
            }
            _ => false,
        }
    }

    pub(crate) fn check_unconsumed_caps(&mut self) {
        if let Some(scope) = self.cap_vars.last() {
            // v0.29.50: fast path — if all consumed, return immediately.
            let total = scope.len();
            let consumed_count = scope
                .values()
                .filter(|info| info.consumed && !info.maybe_consumed)
                .count();
            if consumed_count == total {
                return; // O(1) fast path via count comparison
            }
            // Slow path: find unconsumed vars
            let unconsumed: Vec<String> = scope
                .iter()
                .filter(|(_, info)| !info.consumed || info.maybe_consumed)
                .map(|(name, _)| name.clone())
                .collect();
            for name in unconsumed {
                self.emit_code(
                    crate::diagnostic::codes::E0256,
                    format!(
                        "linear capability '{}' must be consumed (via drop) before end of scope",
                        name
                    ),
                );
            }
        }
    }
}
