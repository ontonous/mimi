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
        // P2-3 fix: session alias consumption is per-function (same as flow vars).
        // Without this, function A's alias marking leaks into function B.
        self.consumed_session_vars.clear();
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
        // 0.36.7 (裁决 3, DoD #3): Fault 是状态不是值 — 禁止作为函数返回值。
        // bare `Fault` 或 flow-qualified `flow::X::Fault` 都是同一系统 sink；
        // 进入 Fault 后必须经 recover/reset 显式离开，预期失败走 Result 值。
        if func.ret.is_some() && Self::is_fault_sink_type(&ret) {
            self.emit_code(
                crate::diagnostic::codes::E0441,
                format!(
                    "function '{}' returns the Fault sink: Fault 是状态不是值，禁止作为函数返回值；进入 Fault 后必须经 recover/reset 显式离开（预期失败用 Result<T, E> 值传播）",
                    func.name
                ),
            );
        }
        self.current_ret = Some(ret.clone());
        let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
        // Push function-level variable scope for shadowing detection
        self.var_scopes.push(HashMap::new());
        for p in &func.params {
            let ty = self.resolve_type(&p.ty);
            // SessionChan<S> / SessionChan<dual S> params: seed residual from
            // the declared session body (dual expressions resolve + dualize;
            // 0.36.38).
            if let Some(resolved) =
                crate::session::residual_from_chan_type(&ty, &self.session_types)
            {
                self.session_residuals.insert(p.name.clone(), resolved);
            }
            scopes[0].insert(p.name.clone(), ty);
            // Track mutable parameters for assignment checking
            if let Some(s) = self.mut_vars.last_mut() {
                s.insert(p.name.clone(), p.mut_);
            }
        }

        // 0.1.9 Phase A (slice 0.39.9): `linear T` 参数在**定义时**做 transfer-only
        // 体校验。`linear T` 是显式线性种类：调体必须把 T 整体转移（直通/整容器），
        // 禁止投影 / 弃置 / drop(T)（T 可能实例化为 Session，drop = E0425 弃置）。
        // 校验通过后，调用点对该位置的线性实参直接放行（kind 兼容），不再依赖
        // 调用点 blackbox（Free `T` 仍走迁移 blackbox，P 合同不变）。
        self.check_linear_kind_param_bodies(&func.name, &func.params, &func.body);

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
        let has_shared_param = func
            .params
            .iter()
            .any(|p| matches!(p.ty.unlocated(), Type::Shared(_)));
        if has_shared_param {
            let has_contract = func.body.iter().any(|s| {
                matches!(
                    s.unlocated(),
                    Stmt::Requires(..) | Stmt::Ensures(..) | Stmt::Invariant(..) | Stmt::Math(_)
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
        // 0.31.24: Comptime purity enforcement — comptime functions must be pure
        let was_in_comptime = self.in_comptime;
        if func.is_comptime {
            self.in_comptime = true;
        }
        // v0.34.18c (§4.2): the `with` effect clause is abolished — no effects
        // scope is pushed for the function body.
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
            let last_ty = self.unification.zonk_or_unknown(&last_ty);
            // Unwrap shared/aliasing wrappers for return type compatibility
            let last_ty_clean = match last_ty.unlocated() {
                Type::Shared(i) => (**i).clone(),
                _ => last_ty.clone(),
            };
            let coerced = is_numeric_coercion(&ret, &last_ty_clean);
            let type_ok = coerced || self.unification.unify(&ret, &last_ty_clean).is_ok();
            if !type_ok {
                // 0.35.23 deep-eval: the old `!unit` exemption silently
                // discarded a non-unit tail expression in a unit function
                // (e.g. `func send_help() { send(...) }` — send returns i64).
                // The resolved layer then hard-rejected the body as
                // TOOL-RESOLUTION-001 ("i64 and () have no admitted implicit
                // conversion"), leaking an internal error to the user. Report
                // E0207 up front so the mismatch is a clear diagnostic.
                self.errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0207,
                        format!("implicit return: expected {}, found {}", fmt_type(&ret), fmt_type(&last_ty)),
                        self.diagnostic_span(),
                    ).with_help("the last expression in a function body is implicitly returned; make sure its type matches the declared return type — or discard it with `let _ = ...`")
                );
            }
        }
        // v0.31.12: session scope exit check — non-end residuals must not
        // silently leave scope. Every tracked endpoint must reach `end` before
        // the function returns (or be explicitly returned/transferred).
        let unfinished: Vec<(String, String)> = self
            .session_residuals
            .iter()
            .filter(|(_, r)| !matches!(r.unlocated(), crate::ast::SessionType::End))
            .map(|(v, r)| (v.clone(), crate::session::fmt_session(r)))
            .collect();
        for (var, residual_str) in unfinished {
            self.emit_code(
                crate::diagnostic::codes::E0425,
                format!(
                    "session endpoint '{}' leaves scope with unfinished protocol residual `{}`; \
                     complete the protocol (send/recv/close) or return the endpoint",
                    var, residual_str
                ),
            );
        }
        self.var_scopes.pop();
        self.finish_expression_type_capture();
        self.current_ret = None;
        self.current_callable_owner = None;
        // Audit 2026-08-05 (wave-1 central): nested-func directory entries
        // registered during this body must not leak to later items.
        self.flush_pending_nested_restores();
        self.generic_scope.truncate(generic_scope_len);
        // 0.31.24: Restore comptime context
        self.in_comptime = was_in_comptime;
    }

    /// 0.1.9 Phase A + 0.39.61: 定义时 `linear T` / `linear drop T` 参数体校验
    /// （E0841）。顶层函数与 impl/actor 方法共用（方法此前完全绕过本检查——
    /// 泄漏 `linear T` 方法体静默弃值，0.39.61 实证修复）。
    pub(crate) fn check_linear_kind_param_bodies(
        &mut self,
        name: &str,
        params: &[crate::ast::Param],
        body: &[crate::ast::Stmt],
    ) {
        let linear_names = self.linear_kind_generic_names(name);
        if linear_names.is_empty() {
            return;
        }
        // 0.39.61: 方法隐式 self 偏移——funcs 签名把 self 放 index 0，而
        // `params`（AST，无 self）从 0 起。funcs 长 - AST 长 = 偏移
        // （顶层函数 0，隐式 self 方法 1，显式 self 0）。
        let offset = self
            .funcs
            .get(name)
            .map(|(ps, _)| ps.len().saturating_sub(params.len()))
            .unwrap_or(0);
        for (index, p) in params.iter().enumerate() {
            let funcs_index = index + offset;
            if !self.param_uses_linear_kind(name, funcs_index) {
                continue;
            }
            // `linear T` → transfer-only（allow_drop=false，T 可能 Session）；
            // `linear drop T` → drop-tolerant（allow_drop=true，T 可 drop 但
            // 每路径必须消费；实例化由调用点排除 Session）。
            let drop_tolerant = self.param_uses_linear_drop_kind(name, funcs_index);
            // 0.39.61: 直接对给定 AST 参数 + 体做 blackbox 分析（方法无
            // find_func_def_ast 命中；顶层/方法同路径）。
            let ok = self.linear_kind_body_sound(&params[index], body, drop_tolerant);
            if !ok {
                let (kind, reason) = if drop_tolerant {
                    (
                        "`linear drop T`",
                        "the body must consume T on every path (transfer or drop), \
                         never project / discard / silently abandon it",
                    )
                } else {
                    (
                        "`linear T`",
                        "the body must pass T through (return / move into a linear-safe receiver), \
                         never project / discard / drop it (T may instantiate to Session)",
                    )
                };
                self.emit_code(
                    codes::E0841,
                    format!(
                        "{kind} parameter '{}' of function '{}' is not whole-transfer: {reason}",
                        p.name, name
                    ),
                );
            }
        }
    }

    /// Check if a block returns on all paths
    pub(crate) fn block_returns_on_all_paths(&self, block: &Block) -> bool {
        if block.is_empty() {
            return false;
        }
        // P1-16: If any statement before the last is a bare break/continue,
        // subsequent statements (including the last) are unreachable.
        // `loop { break; return 1 }` must NOT count as returning.
        // LIMITATION: Only detects bare break/continue at the top level
        // of the block. Conditional breaks (`if cond { break }`) require
        // CFG analysis and are NOT detected — the loop body may still
        // incorrectly report as returning on all paths.
        for stmt in block.iter().rev().skip(1) {
            if matches!(stmt.unlocated(), Stmt::Break(_) | Stmt::Continue) {
                return false;
            }
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
                Stmt::IfLet { then_, else_, .. } => {
                    let then_returns = self.block_returns_on_all_paths(then_);
                    let else_returns = else_
                        .as_ref()
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
                // H2/M1 fix: `ieee_float { }` and `unsafe { }` are transparent
                // wrapper blocks — a `return` inside them returns from the
                // enclosing function. Without these arms, `ieee_float { return X }`
                // as the last statement fell through to `_ => {}` and triggered a
                // spurious E0255 ("not all paths return"), which masked the H2
                // ieee_depth leak by forbidding the early-return syntax outright.
                Stmt::IeeeFloat(inner) | Stmt::Unsafe(inner) => {
                    if self.block_returns_on_all_paths(inner) {
                        return true;
                    }
                }
                Stmt::Arena(inner) => {
                    if self.block_returns_on_all_paths(inner) {
                        return true;
                    }
                }
                Stmt::Loop(body) => {
                    // T-3 (audit 2026-08-05): an infinite loop whose body
                    // returns on all paths only guarantees a function return
                    // if the loop can never exit via `break`. The top-level
                    // bare break/continue scan above misses CONDITIONAL breaks
                    // (`if cond { break }`), so the body was wrongly judged
                    // all-paths-returning and E0255 was missed. Require
                    // break-unreachability as well (conservative over-
                    // approximation of break reachability — fail-closed).
                    if self.block_returns_on_all_paths(body) && !self.loop_body_can_break(body) {
                        return true;
                    }
                }
                // P0-3: While/WhileLet/For loops do NOT guarantee a return
                // even if the body returns on all paths — the loop condition
                // may be false / the iterable may be empty, so execution can
                // skip the body entirely. Only Loop (infinite) guarantees
                // the body executes. `while false { return 1 }` must NOT
                // count as returning on all paths.
                Stmt::While { .. } | Stmt::WhileLet { .. } | Stmt::For { .. } => {}
                _ => {}
            }
        }
        false
    }

    /// T-3 (audit 2026-08-05): conservative over-approximation of "`some`
    /// path through this loop body reaches a `break` of THIS loop". Breaks
    /// nested inside an inner Loop/While/For target that inner loop and do
    /// not exit the analyzed loop, so nested loop bodies are not descended.
    /// Any conditional break (`if cond { break }`) counts: fail-closed for
    /// E0255 is accepting an extra warning, not missing a real missing
    /// return. Local walk — deliberately not a full CFG.
    fn loop_body_can_break(&self, block: &Block) -> bool {
        block.iter().any(|stmt| self.stmt_can_break(stmt))
    }

    fn stmt_can_break(&self, stmt: &Stmt) -> bool {
        match stmt.unlocated() {
            Stmt::Break(_) => true,
            // continue re-iterates; return exits the function — neither
            // exits the loop via break.
            Stmt::Continue | Stmt::Return(_) => false,
            Stmt::If { then_, else_, .. } | Stmt::IfLet { then_, else_, .. } => {
                self.loop_body_can_break(then_)
                    || else_
                        .as_ref()
                        .map(|e| self.loop_body_can_break(e))
                        .unwrap_or(false)
            }
            Stmt::Block(inner)
            | Stmt::Arena(inner)
            | Stmt::Unsafe(inner)
            | Stmt::IeeeFloat(inner)
            | Stmt::Defer(inner) => self.loop_body_can_break(inner),
            Stmt::Expr(expr) => self.expr_can_break(expr),
            // Breaks inside nested loops target the INNER loop; a nested
            // loop statement cannot by itself exit the analyzed loop.
            Stmt::Loop(_) | Stmt::While { .. } | Stmt::WhileLet { .. } | Stmt::For { .. } => false,
            _ => false,
        }
    }

    fn expr_can_break(&self, expr: &Expr) -> bool {
        match expr.unlocated() {
            Expr::Block(block) | Expr::Arena(block) | Expr::Comptime(block) => {
                self.loop_body_can_break(block)
            }
            Expr::If { then_, else_, .. } => {
                self.loop_body_can_break(then_)
                    || else_
                        .as_ref()
                        .map(|e| self.loop_body_can_break(e))
                        .unwrap_or(false)
            }
            Expr::Match(_subject, arms) => arms.iter().any(|arm| self.expr_can_break(&arm.body)),
            _ => false,
        }
    }
}
