use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::verifier::ctx::{
    Counterexample, SolverSession, TrustedSubsetDomain, VerifStatus, VerificationResult,
    VerifierCtx, Z3VarMap,
};
use crate::verifier::expr;
use crate::verifier::helpers::{
    block_tail_expr, collect_idents_in_expr, collect_idents_in_stmt, extract_body_return,
    format_expr,
};
use std::collections::HashMap;
use std::str::FromStr;
use std::time::Instant;
use z3::ast::String as Z3String;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};
use z3::SatResult;

impl VerifierCtx {
    pub(crate) fn verify_items(
        &mut self,
        session: &mut SolverSession,
        items: &[Item],
        results: &mut Vec<VerificationResult>,
    ) {
        // Pre-populate func_defs so call-site verification can look up
        // callee ensures (cross-module contract propagation).
        self.collect_func_defs(items);
        // V-C4 source-order independence: verify leaves (no body calls to
        // known funcs with ensures) first via a simple multi-wave schedule.
        // Wave 0: all funcs (status may be conservative for callers-before-callees).
        // Wave 1: re-verify all and keep final status/results.
        // This is O(2n) and correct once callees are Verified from wave 0.
        let mut wave0 = Vec::new();
        self.verify_items_collect(session, items, &mut wave0);
        // Discard wave0 results; keep func_status. Re-verify for final results.
        results.clear();
        self.verify_items_collect(session, items, results);
    }

    fn verify_items_collect(
        &mut self,
        session: &mut SolverSession,
        items: &[Item],
        results: &mut Vec<VerificationResult>,
    ) {
        for item in items {
            match item {
                Item::Func(f) => {
                    if !f.body.is_empty() {
                        session.reset();
                        let result = self.verify_func(session, f);
                        self.func_status
                            .insert(f.name.clone(), result.status.clone());
                        results.push(result);
                    }
                }
                Item::Module(m) => self.verify_items_collect(session, &m.items, results),
                Item::ExternBlock(block) => {
                    for func in &block.funcs {
                        if func.requires.is_some() || func.ensures.is_some() {
                            session.reset();
                            let result = self.verify_extern_func(session, func);
                            self.func_status
                                .insert(func.name.clone(), result.status.clone());
                            results.push(result);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Pre-seed func_status for Flow Ready state (same two-wave idea).
    pub(crate) fn preseed_func_status(&mut self, session: &mut SolverSession, items: &[Item]) {
        let mut discard = Vec::new();
        self.verify_items_collect(session, items, &mut discard);
    }

    pub(crate) fn verify_extern_func(
        &mut self,
        session: &mut SolverSession,
        func: &ExternFunc,
    ) -> VerificationResult {
        let start = Instant::now();
        // 2.3: reset() clears all assertions. Z3's Params (incl. timeout) are NOT
        // affected by reset() — they persist across calls. The solver is clean
        // for each extern verification, preventing cross-contamination from
        // prior verify_func calls.

        let requires_expr = func.requires.as_ref();
        let ensures_expr = func.ensures.as_ref();

        let returns_real = func
            .ret
            .as_ref()
            .is_some_and(|t| matches!(t.unlocated(), Type::Name(n, _) if n == "f64"));

        let mut vars = Z3VarMap::new();

        for p in &func.params {
            if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "f64") {
                vars.insert_real(p.name.as_str(), Z3Real::new_const(p.name.as_str()));
            } else if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "string") {
                // V-H5: strings get dedicated string vars (plus length/nonempty).
                // §11-#37: dot separator — `_len`/`_ne` suffixes could alias a
                // parameter literally named `{p}_len`/`{p}_ne` (cross-object proof).
                vars.insert_string_var(p.name.as_str(), Z3String::new_const(p.name.as_str()));
                vars.insert_string_nonempty(
                    p.name.as_str(),
                    Z3Bool::new_const(format!("{}.ne", p.name)),
                );
                vars.insert_string_len(
                    p.name.as_str(),
                    Z3Int::new_const(format!("{}.len", p.name)),
                );
            } else if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "bool" || n == "Bool") {
                // V-H5: bools are Z3 Bool, not opaque Int.
                vars.insert_bool(p.name.as_str(), Z3Bool::new_const(p.name.as_str()));
            } else {
                vars.insert_int(p.name.as_str(), Z3Int::new_const(p.name.as_str()));
            }
        }
        if returns_real {
            vars.insert_real("result", Z3Real::new_const("result"));
        } else {
            vars.insert_int("result", Z3Int::new_const("result"));
        }

        let constraint_count =
            (requires_expr.is_some() as usize) + (ensures_expr.is_some() as usize);

        if let Some(req) = requires_expr {
            match expr::expr_to_z3_bool(req, &mut vars) {
                Some(z3_bool) => session.assert(&z3_bool),
                None => {
                    return VerificationResult {
                        func_name: format!("extern {}", func.name),
                        status: VerifStatus::NotInTrustedSubset,
                        message: "could not encode extern requires for Z3".into(),
                        diagnostic: None,
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count,
                        artifact: None,
                        trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
                    };
                }
            }
        }

        match session.check() {
            SatResult::Unsat => VerificationResult {
                func_name: format!("extern {}", func.name),
                status: VerifStatus::Failed,
                message: "preconditions are unsatisfiable".into(),
                diagnostic: Some(
                    Diagnostic::error(
                        format!("extern function '{}' has unsatisfiable requires", func.name),
                        func.meta.span,
                    )
                    .with_help("check that your requires conditions can actually be satisfied"),
                ),
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
                artifact: None,
                trusted_subset_domain: None,
            },
            SatResult::Unknown => {
                let elapsed = start.elapsed();
                let timed_out =
                    elapsed.as_millis() >= session.timeout_ms as u128 || session.timeout_observed;
                let msg = if timed_out {
                    format!(
                        "extern precondition check timed out after {}ms for '{}'",
                        elapsed.as_millis(),
                        func.name
                    )
                } else {
                    format!(
                        "extern precondition satisfiability unknown for '{}' ({:.1?})",
                        func.name, elapsed
                    )
                };
                VerificationResult {
                    func_name: format!("extern {}", func.name),
                    status: if timed_out {
                        VerifStatus::Timeout
                    } else {
                        session.unknown_status()
                    }, // §11-#50
                    message: msg,
                    diagnostic: None,
                    duration_us: elapsed.as_micros() as u64,
                    constraint_count,
                    artifact: None,
                    trusted_subset_domain: None,
                }
            }
            SatResult::Sat => {
                if let Some(ens) = ensures_expr {
                    match expr::expr_to_z3_bool(ens, &mut vars) {
                        Some(z3_bool) => {
                            let (result, _) = session.check_scope(z3_bool.not());
                            match result {
                                SatResult::Unsat => VerificationResult {
                                    func_name: format!("extern {}", func.name),
                                    status: VerifStatus::Verified,
                                    message: if returns_real {
                                            "postconditions always satisfied given preconditions (V-H3: floats modeled as exact reals; rounding not checked)"
                                                .into()
                                        } else {
                                            "postconditions always satisfied given preconditions"
                                                .into()
                                        },
                                    diagnostic: None,
                                    duration_us: start.elapsed().as_micros() as u64,
                                    constraint_count,
                                    artifact: None,
                                    trusted_subset_domain: None,
                                },
                                SatResult::Sat | SatResult::Unknown => VerificationResult {
                                    func_name: format!("extern {}", func.name),
                                    status: VerifStatus::SolverUnknown,
                                    message:
                                        "extern contracts are consistent (preconditions do not statically guarantee postconditions; runtime verification required)"
                                            .into(),
                                    diagnostic: None,
                                    duration_us: start.elapsed().as_micros() as u64,
                                    constraint_count,
                                    artifact: None,
                                    trusted_subset_domain: None,
                                },
                            }
                        }
                        None => VerificationResult {
                            func_name: format!("extern {}", func.name),
                            status: VerifStatus::SolverUnknown,
                            message: "could not encode ensures for Z3".into(),
                            diagnostic: None,
                            duration_us: start.elapsed().as_micros() as u64,
                            constraint_count,
                            artifact: None,
                            trusted_subset_domain: None,
                        },
                    }
                } else {
                    VerificationResult {
                        func_name: format!("extern {}", func.name),
                        status: VerifStatus::Verified,
                        message: "preconditions satisfiable".into(),
                        diagnostic: None,
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count,
                        artifact: None,
                        trusted_subset_domain: None,
                    }
                }
            }
        }
    }

    pub(crate) fn verify_func(
        &mut self,
        session: &mut SolverSession,
        func: &FuncDef,
    ) -> VerificationResult {
        // 0.31.27: VIR path is now wired in. For functions in the trusted
        // subset (no heap, no loops, no calls, no mutation), the VIR path
        // provides:
        // - Checked integer arithmetic (overflow/div-zero definedness VCs)
        // - Counterexample extraction with variable values
        // - Span-free semantic hashing for proof caching (VC artifact)
        // - old(param) == param equality constraints
        // - i32 parameter range constraints (sound overflow checking)
        //
        // Functions outside the trusted subset fall back to the AST path
        // (which handles calls, f64, invariants, etc.).
        //
        // DEFERRED → post-0.31.27: Extend trusted subset to allow calls
        // to verified functions (callee ensures propagation in VIR path).
        // Currently calls are rejected by the gate; the AST path handles
        // callee ensures via assert_callee_ensures_in_block.
        if let Some(vir_result) = self.verify_func_vir(session, func) {
            return vir_result;
        }

        let start = Instant::now();

        // Shared parameters use abstract heap encoding:
        // shared identity → opaque Int variable,
        // field accesses → fresh Z3 variables (handled by Expr::Field encoding).
        // This allows verifying scalar-field contracts on shared params.

        let mut requires_exprs: Vec<Expr> = Vec::new();
        let mut ensures_exprs: Vec<Expr> = Vec::new();
        let mut invariant_exprs: Vec<Expr> = Vec::new();
        let mut math_exprs: Vec<Expr> = Vec::new();
        let mut requires_spans: Vec<Span> = Vec::new();
        let mut ensures_spans: Vec<Span> = Vec::new();
        let mut invariant_spans: Vec<Span> = Vec::new();
        let mut parse_errors: Vec<String> = Vec::new();

        for stmt in &func.body {
            match stmt.unlocated() {
                Stmt::Requires(expr, span) => {
                    requires_exprs.push(expr.clone());
                    requires_spans.push(
                        expr.meta()
                            .map(|meta| meta.span)
                            .or_else(|| stmt.meta().map(|meta| meta.span))
                            .unwrap_or(*span),
                    );
                }
                Stmt::Ensures(expr, span) => {
                    ensures_exprs.push(expr.clone());
                    ensures_spans.push(
                        expr.meta()
                            .map(|meta| meta.span)
                            .or_else(|| stmt.meta().map(|meta| meta.span))
                            .unwrap_or(*span),
                    );
                }
                Stmt::Invariant(expr, span) => {
                    invariant_exprs.push(expr.clone());
                    invariant_spans.push(
                        expr.meta()
                            .map(|meta| meta.span)
                            .or_else(|| stmt.meta().map(|meta| meta.span))
                            .unwrap_or(*span),
                    );
                }
                Stmt::Math(exprs) => math_exprs.extend(exprs.clone()),
                // 0.35.13 trivia-ization: contracts must use top-level
                // requires:/ensures: statements for mimi verify; desc:/rule:/
                // mms{} are consumed by the parser as trivia.
                Stmt::Ellipsis => {}
                _ => {}
            }
        }

        if requires_exprs.is_empty() && ensures_exprs.is_empty() && math_exprs.is_empty() {
            // Even if this function has no contracts, it may call other
            // functions that have requires. Check call sites in a minimal
            // solver context.
            let mut vars = Z3VarMap::new();
            for p in &func.params {
                if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "f64") {
                    vars.insert_real(p.name.as_str(), z3::ast::Real::new_const(p.name.as_str()));
                } else {
                    vars.insert_int(p.name.as_str(), z3::ast::Int::new_const(p.name.as_str()));
                }
            }
            let let_subst = self.build_let_subst(&func.body);
            let expanded_body: Vec<Stmt> = func
                .body
                .iter()
                .map(|s| Self::expand_lets_in_stmt(s, &let_subst))
                .collect();
            let mut call_site_errors: Vec<(String, String, Span)> = Vec::new();
            self.check_callee_requires_in_block(
                session,
                &expanded_body,
                &mut vars,
                func.name.as_str(),
                &mut call_site_errors,
            );
            if !call_site_errors.is_empty() {
                let (_, msg, span) = &call_site_errors[0];
                return VerificationResult {
                    func_name: func.name.clone(),
                    status: VerifStatus::Failed,
                    message: msg.clone(),
                    diagnostic: Some(Diagnostic::error(msg.clone(), *span)),
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count: 0,
                    artifact: None,
                    trusted_subset_domain: None,
                };
            }
            let msg = if parse_errors.is_empty() {
                "no contracts to verify".into()
            } else {
                format!("contract parse errors: {}", parse_errors.join("; "))
            };
            return VerificationResult {
                func_name: func.name.clone(),
                status: VerifStatus::InfrastructureError,
                message: msg,
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count: 0,
                artifact: None,
                trusted_subset_domain: None,
            };
        }

        let returns_real = func
            .ret
            .as_ref()
            .is_some_and(|t| matches!(t.unlocated(), Type::Name(n, _) if n == "f64"));
        let returns_bool = func.ret.as_ref().is_some_and(
            |t| matches!(t.unlocated(), Type::Name(n, _) if n == "bool" || n == "Bool"),
        );
        let returns_i32 = func
            .ret
            .as_ref()
            .is_some_and(|t| matches!(t.unlocated(), Type::Name(n, _) if n == "i32" || n == "Int"));
        let returns_i64 = func
            .ret
            .as_ref()
            .is_some_and(|t| matches!(t.unlocated(), Type::Name(n, _) if n == "i64"));

        let mut vars = Z3VarMap::new();
        let mut old_names: Vec<String> = Vec::with_capacity(func.params.len());

        for p in &func.params {
            if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "f64") {
                vars.insert_real(p.name.as_str(), Z3Real::new_const(p.name.as_str()));
            } else if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "string") {
                // V-H5: strings use dedicated string vars (not opaque Int).
                // §11-#37: dot separator keeps derived constants out of the
                // user-identifier namespace (`s_len` params stay distinct).
                vars.insert_string_nonempty(
                    p.name.as_str(),
                    Z3Bool::new_const(format!("{}.ne", p.name)),
                );
                vars.insert_string_len(
                    p.name.as_str(),
                    Z3Int::new_const(format!("{}.len", p.name)),
                );
                vars.insert_string_var(p.name.as_str(), Z3String::new_const(p.name.as_str()));
            } else if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "bool" || n == "Bool") {
                // V-H5: bools are Z3 Bool.
                vars.insert_bool(p.name.as_str(), Z3Bool::new_const(p.name.as_str()));
            } else if matches!(p.ty.unlocated(), Type::Name(n, args) if n == "List" && !args.is_empty())
            {
                // List parameters get a length variable for modeling sort() etc.
                vars.insert_int(p.name.as_str(), Z3Int::new_const(p.name.as_str()));
                let len_var = Z3Int::new_const(format!("{}.len", p.name));
                // RT-H10 (audit): constrain list length to be >= 0 so the
                // solver does not produce unconstrained values that could
                // satisfy vacuously true postconditions.
                let zero = Z3Int::from_i64(0);
                session.solver.assert(len_var.ge(&zero));
                vars.insert_list_len(p.name.as_str(), len_var);
            } else {
                let iv = Z3Int::new_const(p.name.as_str());
                vars.insert_int(p.name.as_str(), iv.clone());
                // V-H4 (partial) + H2: constrain checked integer params to
                // their machine range so unbounded Z3 Int does not prove false
                // modular properties. H2 extends this from i32-only to i64 —
                // without the i64 range pin, `x == i64::MAX` is unreachable to
                // the solver and overflow obligations validate vacuously.
                let int_bounds = match p.ty.unlocated() {
                    Type::Name(n, _) if n == "i32" || n == "Int" => {
                        Some((i32::MIN as i64, i32::MAX as i64))
                    }
                    Type::Name(n, _) if n == "i64" => Some((i64::MIN, i64::MAX)),
                    _ => None,
                };
                if let Some((lo, hi)) = int_bounds {
                    let lo = Z3Int::from_i64(lo);
                    let hi = Z3Int::from_i64(hi);
                    session.solver.assert(iv.ge(&lo));
                    session.solver.assert(iv.le(&hi));
                }
            }
            // §11-#37 (audit 2026-08-05): dot separator prevents collision
            // between parameter `old_p` and `old(p)` expression.
            old_names.push(format!("old.{}", p.name));
        }

        if returns_real {
            let z3_result = Z3Real::new_const("result");
            vars.insert_real("result", z3_result.clone());
        } else if returns_bool {
            let z3_result = Z3Bool::new_const("result");
            vars.insert_bool("result", z3_result.clone());
        } else {
            let z3_result = Z3Int::new_const("result");
            vars.insert_int("result", z3_result.clone());
            if returns_i32 {
                let lo = Z3Int::from_i64(i32::MIN as i64);
                let hi = Z3Int::from_i64(i32::MAX as i64);
                session.assert(z3_result.ge(&lo));
                session.assert(z3_result.le(&hi));
            }
        }

        for (i, p) in func.params.iter().enumerate() {
            let old_name = old_names[i].as_str();
            if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "f64") {
                vars.insert_real(old_name, Z3Real::new_const(old_name));
            } else if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "string") {
                vars.insert_string_nonempty(
                    old_name,
                    Z3Bool::new_const(format!("{}.ne", old_name)),
                );
                vars.insert_string_len(old_name, Z3Int::new_const(format!("{}.len", old_name)));
                vars.insert_string_var(old_name, Z3String::new_const(old_name));
            } else if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "bool" || n == "Bool") {
                vars.insert_bool(old_name, Z3Bool::new_const(old_name));
            } else if matches!(p.ty.unlocated(), Type::Name(n, args) if n == "List" && !args.is_empty())
            {
                vars.insert_int(old_name, Z3Int::new_const(old_name));
                let old_len_var = Z3Int::new_const(format!("{}.len", old_name));
                let zero = Z3Int::from_i64(0);
                session.solver.assert(old_len_var.ge(&zero));
                vars.insert_list_len(old_name, old_len_var);
            } else {
                vars.insert_int(old_name, Z3Int::new_const(old_name));
            }
        }

        // Assert consistency between Z3 string theory variables and the
        // integer-encoded string_len/string_nonempty variables.
        // This ensures that s.length() == string_len[s] and (s != "") == string_nonempty[s].
        for p in &func.params {
            if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "string") {
                if let Some(z3_s) = vars.get_string_var(p.name.as_str()) {
                    if let Some(len_var) = vars.get_string_len(p.name.as_str()) {
                        session.assert(z3_s.length().eq(len_var));
                    }
                    let Ok(empty) = Z3String::from_str("") else {
                        continue;
                    };
                    let nonempty_check = z3_s.ne(&empty);
                    if let Some(ne_var) = vars.get_string_nonempty(p.name.as_str()) {
                        session.assert(ne_var.eq(&nonempty_check));
                    }
                }
            }
        }
        // Same for old_* snapshots
        for (i, p) in func.params.iter().enumerate() {
            if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "string") {
                let old_name = old_names[i].as_str();
                if let Some(z3_s) = vars.get_string_var(old_name) {
                    if let Some(len_var) = vars.get_string_len(old_name) {
                        session.assert(z3_s.length().eq(len_var));
                    }
                    let Ok(empty) = Z3String::from_str("") else {
                        continue;
                    };
                    let nonempty_check = z3_s.ne(&empty);
                    if let Some(ne_var) = vars.get_string_nonempty(old_name) {
                        session.assert(ne_var.eq(&nonempty_check));
                    }
                }
            }
        }

        let body_return = extract_body_return(&func.body);

        // Build let-substitution map so that `let y = double(x); y` resolves
        // `y` to `double(x)` for encoding purposes.
        let let_subst = self.build_let_subst(&func.body);

        // Expand let-variables in the body return expression to expose
        // function calls that would otherwise be hidden behind local names.
        let body_return = body_return.map(|expr| Self::expand_lets_in_expr(&expr, &let_subst));

        for req in &requires_exprs {
            if let Some(z3_bool) = expr::expr_to_z3_bool(req, &mut vars) {
                session.assert(z3_bool);
            } else {
                // V-2 (0.31.53): fail-closed — unencodable requires means the
                // precondition is incomplete. Returning Proven would be unsound.
                return VerificationResult {
                    func_name: func.name.clone(),
                    status: VerifStatus::NotInTrustedSubset,
                    message: format!(
                        "could not encode requires (fail-closed): {}",
                        format_expr(req)
                    ),
                    diagnostic: None,
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count: 0,
                    artifact: None,
                    trusted_subset_domain: None,
                };
            }
        }

        for math in &math_exprs {
            let Some(z3_bool) = expr::expr_to_z3_bool(math, &mut vars) else {
                return VerificationResult {
                    func_name: func.name.clone(),
                    status: VerifStatus::SolverUnknown,
                    message: format!("could not encode math obligation: {}", format_expr(math)),
                    diagnostic: None,
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count: requires_exprs.len() + math_exprs.len(),
                    artifact: None,
                    trusted_subset_domain: None,
                };
            };
            let (proof, _) = session.check_scope(z3_bool.not());
            match proof {
                SatResult::Unsat => session.assert(z3_bool),
                SatResult::Sat => {
                    return VerificationResult {
                        func_name: func.name.clone(),
                        status: VerifStatus::Failed,
                        message: format!(
                            "math obligation is not implied by preconditions: {}",
                            format_expr(math)
                        ),
                        diagnostic: Some(
                            Diagnostic::error(
                                format!("unproven math obligation in '{}'", func.name),
                                math
                                    .meta()
                                    .map(|meta| meta.span)
                                    .unwrap_or(func.meta.span),
                            )
                            .with_help(
                                "add the necessary requires condition or weaken the math obligation",
                            ),
                        ),
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count: requires_exprs.len() + math_exprs.len(),
                        artifact: None,
                        trusted_subset_domain: None,
                    };
                }
                SatResult::Unknown => {
                    return VerificationResult {
                        func_name: func.name.clone(),
                        status: session.unknown_status(), // §11-#50
                        message: format!(
                            "solver could not prove math obligation: {}",
                            format_expr(math)
                        ),
                        diagnostic: None,
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count: requires_exprs.len() + math_exprs.len(),
                        artifact: None,
                        trusted_subset_domain: None,
                    };
                }
            }
        }

        // V-H1 (establish): prove each invariant from requires before assuming it.
        // V-H1 (preserve, conservative): if any loop body assigns a free variable of
        // an invariant, we cannot claim Verified without a body⇒inv' proof — record
        // a parse_error so the final status degrades to Unknown rather than a false
        // Verified. Bodies that do not touch inv free vars auto-preserve.
        if !invariant_exprs.is_empty() {
            let mut inv_free: Vec<String> = Vec::new();
            for inv in &invariant_exprs {
                collect_idents_in_expr(inv, &mut inv_free);
            }
            let mut assigned: Vec<String> = Vec::new();
            Self::collect_loop_assigned_idents(&func.body, &mut assigned);
            let mut touched = false;
            for a in &assigned {
                if inv_free.iter().any(|f| f == a) {
                    touched = true;
                    break;
                }
            }
            if touched {
                parse_errors.push(
                    "loop invariant preserve not proven (loop body assigns invariant free vars; full body⇒inv' residual)"
                        .into(),
                );
            }
        }
        for inv in &invariant_exprs {
            let Some(z3_bool) = expr::expr_to_z3_bool(inv, &mut vars) else {
                parse_errors.push(format!("could not encode invariant: {}", format_expr(inv)));
                continue;
            };
            // Check requires ⇒ inv by unsat of !inv under current (requires) assumptions.
            let (proof, _) = session.check_scope(z3_bool.not());
            match proof {
                SatResult::Unsat => {
                    // Established: safe to assume for the rest of the function.
                    session.assert(z3_bool);
                }
                SatResult::Sat => {
                    return VerificationResult {
                        func_name: func.name.clone(),
                        status: VerifStatus::Failed,
                        message: format!(
                            "loop invariant not established by requires: {}",
                            format_expr(inv)
                        ),
                        diagnostic: Some(
                            Diagnostic::error(
                                format!(
                                    "invariant not established at entry in '{}'",
                                    func.name
                                ),
                                inv.meta()
                                    .map(|meta| meta.span)
                                    .unwrap_or(func.meta.span),
                            )
                            .with_help(
                                "strengthen requires so the invariant holds before the loop, or weaken the invariant",
                            ),
                        ),
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count: requires_exprs.len() + invariant_exprs.len(),
                        artifact: None,
                        trusted_subset_domain: None,
                    };
                }
                SatResult::Unknown => {
                    parse_errors.push(format!(
                        "could not prove invariant established: {}",
                        format_expr(inv)
                    ));
                    // Conservatively still assume for ensures checking, but status may be Unknown later.
                    session.assert(z3_bool);
                }
            }
        }

        for (i, p) in func.params.iter().enumerate() {
            let old_name = old_names[i].as_str();
            let param_z3 = vars.get_int(p.name.as_str()).cloned();
            let old_z3 = vars.get_int(old_name).cloned();
            if let (Some(pv), Some(ov)) = (param_z3, old_z3) {
                session.assert(ov.eq(&pv));
            }
        }

        for (i, p) in func.params.iter().enumerate() {
            let old_name = old_names[i].as_str();
            let param_z3 = vars.get_real(p.name.as_str()).cloned();
            let old_z3 = vars.get_real(old_name).cloned();
            if let (Some(pv), Some(ov)) = (param_z3, old_z3) {
                session.assert(ov.eq(&pv));
            }
        }

        // V-2 (full-audit-2026-08-05-0656 §3.8): bool and string params also
        // satisfy old == current on every contract path. Previously only
        // int_vars/real_vars were walked, so `ensures: old(s) == s` was a
        // fake Disproven (old_s unconstrained) while the Resolved engine
        // asserted it — the two engines disagreed. The trusted subset has no
        // mutation, so equality is exact; Z3 string theory then derives the
        // length/non-empty consistency for `old_*` snapshots from the axiom.
        for (i, p) in func.params.iter().enumerate() {
            let old_name = old_names[i].as_str();
            if let (Some(pv), Some(ov)) = (
                vars.get_bool(p.name.as_str()).cloned(),
                vars.get_bool(old_name).cloned(),
            ) {
                session.assert(ov.eq(&pv));
            }
            if let (Some(pv), Some(ov)) = (
                vars.get_string_var(p.name.as_str()).cloned(),
                vars.get_string_var(old_name).cloned(),
            ) {
                session.assert(ov.eq(&pv));
            }
        }

        // v0.31.6: assert callee ensures for the return expression BEFORE the
        // i32 definedness (overflow) check below. A return such as
        // `(await t1) + (await t2)` — expanded from `let t1 = spawn id(x)` —
        // needs the callee's ensures (e.g. id: result == a) in the solver
        // context so the awaited operands are bounded and the no-overflow
        // obligation can be discharged. The old order asserted ensures only
        // after this check, leaving await results in arithmetic position
        // unconstrained → spurious "integer overflow is not excluded".
        let mut call_site_errors: Vec<(String, String, Span)> = Vec::new();
        if let Some(ref return_expr) = body_return {
            self.assert_callee_ensures_in_expr(
                session,
                return_expr,
                &mut vars,
                func.name.as_str(),
                &mut call_site_errors,
            );
        }

        if let Some(ref return_expr) = body_return {
            if returns_real {
                if let Some(body_z3) = expr::expr_to_z3_real(return_expr, &mut vars) {
                    if let Some(r) = vars.get_real("result") {
                        session.assert(r.eq(&body_z3));
                    }
                } else {
                    parse_errors.push(
                        "could not encode return expression — result may be unconstrained".into(),
                    );
                }
            } else if !returns_i32 && !returns_i64 {
                if let Some(body_z3) = expr::expr_to_z3_int(return_expr, &mut vars) {
                    if let Some(i) = vars.get_int("result") {
                        session.assert(i.eq(&body_z3));
                    }
                } else {
                    parse_errors.push(
                        "could not encode return expression — result may be unconstrained".into(),
                    );
                }
            } else if let Some(obligations) = expr::int_definedness_obligations(
                return_expr,
                &mut vars,
                if returns_i64 {
                    i64::MIN
                } else {
                    i32::MIN as i64
                },
                if returns_i64 {
                    i64::MAX
                } else {
                    i32::MAX as i64
                },
            ) {
                for obligation in obligations {
                    let (proof, _) = session.check_scope(obligation.condition.not());
                    match proof {
                        SatResult::Unsat => session.assert(obligation.condition),
                        SatResult::Sat => {
                            return VerificationResult {
                                func_name: func.name.clone(),
                                status: VerifStatus::Failed,
                                message: obligation.failure.into(),
                                diagnostic: Some(
                                    Diagnostic::error(
                                        obligation.failure,
                                        return_expr
                                            .meta()
                                            .map(|meta| meta.span)
                                            .unwrap_or(func.meta.span),
                                    )
                                    .with_help("strengthen requires so the operation is defined"),
                                ),
                                duration_us: start.elapsed().as_micros() as u64,
                                constraint_count: requires_exprs.len() + 1,
                                artifact: None,
                                trusted_subset_domain: None,
                            };
                        }
                        SatResult::Unknown => {
                            return VerificationResult {
                                func_name: func.name.clone(),
                                status: session.unknown_status(), // §11-#50
                                message: format!(
                                    "solver could not prove integer definedness: {}",
                                    obligation.failure
                                ),
                                diagnostic: None,
                                duration_us: start.elapsed().as_micros() as u64,
                                constraint_count: requires_exprs.len() + 1,
                                artifact: None,
                                trusted_subset_domain: None,
                            };
                        }
                    }
                }
                let Some(body_z3) = expr::expr_to_z3_int(return_expr, &mut vars) else {
                    return VerificationResult {
                        func_name: func.name.clone(),
                        status: VerifStatus::SolverUnknown,
                        message: "could not encode return expression — result may be unconstrained"
                            .into(),
                        diagnostic: None,
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count: requires_exprs.len(),
                        artifact: None,
                        trusted_subset_domain: None,
                    };
                };
                if let Some(i) = vars.get_int("result") {
                    session.assert(i.eq(&body_z3));
                }
                // Link result length to body return length for sort/reverse.
                // This ensures len(result) == len(sort(xs)) == len(xs).
                if let Some(body_len) = expr::resolve_list_len(return_expr, &mut vars) {
                    let len_key = expr::call_var_key("len", &[Expr::Ident("result".to_string())]);
                    let result_len = vars.get_or_create_int(&len_key);
                    session.assert(result_len.eq(&body_len));
                }
            } else {
                parse_errors.push(
                    "could not encode return expression — result may be unconstrained".into(),
                );
            }
        } else if func.ret.is_some() {
            // #40 (full-audit-2026-08-05 §11): binding result to 0 here
            // FABRICATES a return value — `ensures: result == 0` then proves
            // against a constraint the real program never produces when the
            // tail expression is not extractable. A fake Proven. Fail closed
            // instead: no extractable return expression, no proof.
            return VerificationResult {
                func_name: func.name.clone(),
                status: VerifStatus::NotInTrustedSubset,
                message: "no extractable return expression for a function with a return type — cannot prove ensures (fail-closed)"
                    .into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count: requires_exprs.len(),
                artifact: None,
                trusted_subset_domain: None,
            };
        }

        // 1.2: Cross-module ensures propagation — for each function call in
        // the body, assert the callee's ensures as constraints on the call
        // variable. This allows the verifier to reason across function calls.
        // Scans the tail expression AND all body statements so that calls in
        // let/assign/if blocks are also propagated. Fixes P0.1: ensures from
        // calls in non-tail positions (e.g. `let y = double(x); y`) are now
        // propagated to the solver.
        // P1.2 fix: also expand let-bindings in body statements so that
        // `let y = double(x); y` expands to `double(x)` before ensures propagation,
        // ensuring callee ensures are propagated even when the call result is
        // stored in a let-bound variable.
        // v0.31.6: return-expression callee ensures are asserted earlier —
        // before the i32 definedness check — so overflow obligations see them.
        let expanded_body: Vec<Stmt> = func
            .body
            .iter()
            .map(|s| Self::expand_lets_in_stmt(s, &let_subst))
            .collect();
        self.assert_callee_ensures_in_block(
            session,
            &expanded_body,
            &mut vars,
            func.name.as_str(),
            &mut call_site_errors,
        );

        // Model length-preserving builtins (sort, reverse) so that
        // postconditions like len(result) == len(xs) can be verified.
        self.assert_builtin_length_preserving_in_block(session, &expanded_body, &mut vars);

        // P1-18: check call-site requires satisfaction. For each function
        // call in the body, verify that the callee's requires (preconditions)
        // are satisfiable given the current symbolic state.
        self.check_callee_requires_in_block(
            session,
            &expanded_body,
            &mut vars,
            func.name.as_str(),
            &mut call_site_errors,
        );

        if !call_site_errors.is_empty() {
            let (_, msg, span) = &call_site_errors[0];
            return VerificationResult {
                func_name: func.name.clone(),
                status: VerifStatus::Failed,
                message: msg.clone(),
                diagnostic: Some(Diagnostic::error(msg.clone(), *span)),
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count: 0,
                artifact: None,
                trusted_subset_domain: None,
            };
        }

        let num_real_params = func
            .params
            .iter()
            .filter(|p| matches!(p.ty.unlocated(), Type::Name(n, _) if n == "f64"))
            .count();
        let constraint_count = requires_exprs.len()
            + invariant_exprs.len()
            + math_exprs.len()
            + func.params.len() // old_* equality constraints (int)
            + num_real_params // old_* equality constraints (real)
            + if body_return.is_some() { 1 } else { 0 };

        let annotate_parse_errors =
            |diag: Option<Diagnostic>, errs: &[String]| -> Option<Diagnostic> {
                if !errs.is_empty() {
                    let mut d = diag.unwrap_or_else(|| {
                        Diagnostic::error(
                            format!("contract errors in '{}'", func.name),
                            func.meta.span,
                        )
                    });
                    d = d.with_note(
                        format!("contract errors: {}", errs.join("; ")),
                        func.meta.span,
                    );
                    Some(d)
                } else {
                    diag
                }
            };

        match session.check() {
            SatResult::Sat => {
                if !ensures_exprs.is_empty() {
                    // Bug fix (CRITICAL #1): The previous implementation used
                    // check_scope_multi which AND-joins all NOT(ensures_i) and
                    // checks once. If ensures_1 is a tautology (NOT(ens1) is
                    // UNSAT) but ensures_2 is violatable (NOT(ens2) is SAT),
                    // the conjunction is UNSAT → false "Verified" report.
                    //
                    // Correct logic: verify each ensures_i independently. A
                    // postcondition is violated if NOT(ensures_i) is SAT.
                    // Only if ALL NOT(ensures_i) are UNSAT do we report
                    // Verified. This is OR semantics: a single SAT means
                    // violation; all UNSAT means verified.
                    // Check each ensures independently. We need to determine
                    // if any NOT(ensures_i) is SAT (violation). Unknown is
                    // treated as inconclusive — reported but not a violation.
                    let mut found_violation = false;
                    let mut found_unknown = false;
                    let mut viol_model: Option<z3::Model> = None;
                    for e in ensures_exprs.iter() {
                        if let Some(b) = expr::expr_to_z3_bool(e, &mut vars) {
                            let (result, model) = session.check_scope(b.not());
                            match result {
                                SatResult::Sat => {
                                    found_violation = true;
                                    viol_model = model;
                                    break;
                                }
                                SatResult::Unknown => {
                                    found_unknown = true;
                                }
                                SatResult::Unsat => {
                                    // This ensures holds; continue checking.
                                }
                            }
                        } else {
                            // V-2 (0.31.53): fail-closed — unencodable ensures
                            // means we cannot verify the postcondition fully.
                            return VerificationResult {
                                func_name: func.name.clone(),
                                status: VerifStatus::NotInTrustedSubset,
                                message: format!(
                                    "could not encode ensures (fail-closed): {}",
                                    format_expr(e)
                                ),
                                diagnostic: None,
                                duration_us: start.elapsed().as_micros() as u64,
                                constraint_count,
                                artifact: None,
                                trusted_subset_domain: None,
                            };
                        }
                    }
                    if found_violation {
                        let counterexample =
                            self.extract_counterexample(&viol_model, &vars, &ensures_exprs);
                        let diagnostic = self.build_failure_narrative(
                            func,
                            &counterexample,
                            &requires_exprs,
                            &ensures_exprs,
                            &requires_spans,
                            &ensures_spans,
                        );
                        VerificationResult {
                            func_name: func.name.clone(),
                            status: VerifStatus::Failed,
                            message: diagnostic.message.clone(),
                            diagnostic: annotate_parse_errors(Some(diagnostic), &parse_errors),
                            duration_us: start.elapsed().as_micros() as u64,
                            constraint_count,
                            artifact: None,
                            trusted_subset_domain: None,
                        }
                    } else if found_unknown {
                        let elapsed = start.elapsed();
                        let timed_out = elapsed.as_millis() >= session.timeout_ms as u128
                            || session.timeout_observed;
                        let msg = if timed_out {
                            format!("verification timed out after {}ms for '{}' — try simplifying postconditions or reducing constraint count ({})",
                                    elapsed.as_millis(), func.name, constraint_count)
                        } else {
                            format!("verification inconclusive for '{}' — solver returned unknown ({} constraints, {:.1?})",
                                    func.name, constraint_count, elapsed)
                        };
                        VerificationResult {
                            func_name: func.name.clone(),
                            status: if timed_out {
                                VerifStatus::Timeout
                            } else {
                                session.unknown_status()
                            }, // §11-#50
                            message: msg,
                            diagnostic: annotate_parse_errors(None, &parse_errors),
                            duration_us: elapsed.as_micros() as u64,
                            constraint_count,
                            artifact: None,
                            trusted_subset_domain: None,
                        }
                    } else {
                        if parse_errors.is_empty() {
                            VerificationResult {
                                func_name: func.name.clone(),
                                status: VerifStatus::Verified,
                                message: "postconditions verified".into(),
                                diagnostic: None,
                                duration_us: start.elapsed().as_micros() as u64,
                                constraint_count,
                                artifact: None,
                                trusted_subset_domain: None,
                            }
                        } else {
                            VerificationResult {
                                func_name: func.name.clone(),
                                status: VerifStatus::SolverUnknown,
                                message: format!(
                                    "verification incomplete for '{}': {}",
                                    func.name,
                                    parse_errors.join("; ")
                                ),
                                diagnostic: annotate_parse_errors(None, &parse_errors),
                                duration_us: start.elapsed().as_micros() as u64,
                                constraint_count,
                                artifact: None,
                                trusted_subset_domain: None,
                            }
                        }
                    }
                } else {
                    if parse_errors.is_empty() {
                        VerificationResult {
                            func_name: func.name.clone(),
                            status: VerifStatus::Verified,
                            message: "preconditions satisfiable, no postconditions".into(),
                            diagnostic: None,
                            duration_us: start.elapsed().as_micros() as u64,
                            constraint_count,
                            artifact: None,
                            trusted_subset_domain: None,
                        }
                    } else {
                        VerificationResult {
                            func_name: func.name.clone(),
                            status: VerifStatus::SolverUnknown,
                            message: format!(
                                "verification incomplete for '{}': {}",
                                func.name,
                                parse_errors.join("; ")
                            ),
                            diagnostic: annotate_parse_errors(None, &parse_errors),
                            duration_us: start.elapsed().as_micros() as u64,
                            constraint_count,
                            artifact: None,
                            trusted_subset_domain: None,
                        }
                    }
                }
            }
            SatResult::Unsat => {
                let req_span = requires_spans.first().copied().unwrap_or(func.meta.span);
                let diagnostic = Diagnostic::error(
                    format!("preconditions are unsatisfiable for '{}'", func.name),
                    req_span,
                )
                .with_help("check that your requires conditions can actually be satisfied");
                VerificationResult {
                    func_name: func.name.clone(),
                    status: VerifStatus::Failed,
                    message: "preconditions are unsatisfiable".into(),
                    diagnostic: annotate_parse_errors(Some(diagnostic), &parse_errors),
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count,
                    artifact: None,
                    trusted_subset_domain: None,
                }
            }
            SatResult::Unknown => {
                let elapsed = start.elapsed();
                let timed_out =
                    elapsed.as_millis() >= session.timeout_ms as u128 || session.timeout_observed;
                let msg = if timed_out {
                    format!("precondition check timed out after {}ms for '{}' — try simplifying requires or reducing constraint count ({})",
                        elapsed.as_millis(), func.name, constraint_count)
                } else {
                    format!(
                        "precondition satisfiability unknown for '{}' ({} constraints, {:.1?})",
                        func.name, constraint_count, elapsed
                    )
                };
                VerificationResult {
                    func_name: func.name.clone(),
                    status: if timed_out {
                        VerifStatus::Timeout
                    } else {
                        session.unknown_status()
                    }, // §11-#50
                    message: msg,
                    diagnostic: annotate_parse_errors(None, &parse_errors),
                    duration_us: elapsed.as_micros() as u64,
                    constraint_count,
                    artifact: None,
                    trusted_subset_domain: None,
                }
            }
        }
    }

    /// 0.31.27+: Attempt to inline callee ensures for VIR path.
    ///
    /// If the function contains calls to verified functions, replace each call
    /// with a fresh variable and inject the callee's ensures as additional
    /// requires. This extends VIR coverage to functions that call verified
    /// pure functions.
    ///
    /// Returns `None` if:
    /// - The function has no calls
    /// - Any call is to an unverified function (V-C4: only admit verified callees)
    /// - Any call argument is not in the trusted subset
    ///
    /// V1 limitations:
    /// - Only handles calls where the callee has explicit ensures
    /// - Callee ensures are injected as requires (unconditional assumptions)
    /// - Nested calls (f(g(x))) are handled recursively (inner first)
    fn try_inline_callee_ensures(&self, func: &FuncDef) -> Option<FuncDef> {
        let mut counter = 0usize;
        let mut injected: Vec<Stmt> = Vec::new();

        // Walk the body and inline calls
        let new_body = self.inline_calls_in_stmts(&func.body, &mut counter, &mut injected)?;

        if injected.is_empty() {
            return None; // No calls were inlined
        }

        // Prepend injected requires to the body. The injected callee-ensures
        // assumptions may reference let-bound call arguments (`double(y)`
        // where `y` is itself a `let`-bound call result). The VIR lowerer
        // processes the injected Requires BEFORE the let bindings they
        // mention: an unresolved name resolves to a phantom VarId distinct
        // from the let's own binding, so the assumption silently references
        // an unconstrained variable and the proof degrades to a fake
        // Disproven (VERIFIED on
        // verifier::tests::verify_callee_ensures_propagation_vir:
        // counterexample result=0 / x=536870911). Expand the injected
        // assumptions through the INLINED body's let-substitution — the
        // inlined let inits carry the `__call_N` fresh vars, so the
        // assumptions become entry-visible and reference exactly the Z3
        // variables the lets will bind.
        let mut body = injected;
        let let_subst = self.build_let_subst(&new_body);
        for stmt in body.iter_mut() {
            if let Stmt::Requires(e, _span) = stmt.unlocated_mut() {
                *e = Self::expand_lets_in_expr(e, &let_subst);
            }
        }
        body.extend(new_body);

        Some(FuncDef {
            body,
            ..func.clone()
        })
    }

    /// Recursively walk statements, inlining calls to verified functions.
    fn inline_calls_in_stmts(
        &self,
        stmts: &[Stmt],
        counter: &mut usize,
        injected: &mut Vec<Stmt>,
    ) -> Option<Vec<Stmt>> {
        let mut result = Vec::with_capacity(stmts.len());
        for stmt in stmts {
            let new_stmt = self.inline_calls_in_stmt(stmt, counter, injected)?;
            result.push(new_stmt);
        }
        Some(result)
    }

    /// Inline calls in a single statement.
    fn inline_calls_in_stmt(
        &self,
        stmt: &Stmt,
        counter: &mut usize,
        injected: &mut Vec<Stmt>,
    ) -> Option<Stmt> {
        match stmt.unlocated() {
            Stmt::Let {
                pat,
                ty,
                init,
                mut_,
                ref_,
            } => {
                let new_init = match init {
                    Some(expr) => Some(self.inline_calls_in_expr(expr, counter, injected)?),
                    None => None,
                };
                Some(Stmt::Let {
                    pat: pat.clone(),
                    ty: ty.clone(),
                    init: new_init,
                    mut_: *mut_,
                    ref_: *ref_,
                })
            }
            Stmt::Return(expr) => {
                let new_expr = match expr {
                    Some(e) => Some(self.inline_calls_in_expr(e, counter, injected)?),
                    None => None,
                };
                Some(Stmt::Return(new_expr))
            }
            Stmt::Expr(expr) => {
                let new_expr = self.inline_calls_in_expr(expr, counter, injected)?;
                Some(Stmt::Expr(new_expr))
            }
            // Contracts and other statements pass through unchanged
            _ => Some(stmt.clone()),
        }
    }

    /// Recursively walk an expression, replacing calls to verified functions
    /// with fresh variables and injecting callee ensures.
    ///
    /// V1: only handles Call, Binary, Unary, Old, Located. Complex expressions
    /// (If, Match, Block, etc.) return None → fall back to AST path.
    fn inline_calls_in_expr(
        &self,
        expr: &Expr,
        counter: &mut usize,
        injected: &mut Vec<Stmt>,
    ) -> Option<Expr> {
        match expr.unlocated() {
            Expr::Call(callee, args) => {
                // Only handle identifier callees (not method calls or complex expressions)
                let callee_name = match callee.unlocated() {
                    Expr::Ident(name) => name.clone(),
                    _ => return None, // Not a simple call; can't inline
                };

                // V-C4: only admit ensures from callees that already verified
                let callee_ok = self
                    .func_status
                    .get(&callee_name)
                    .is_some_and(|s| *s == VerifStatus::Verified);
                if !callee_ok {
                    return None; // Callee not verified; can't inline
                }

                // Get callee's ensures
                let callee_func = self.func_defs.get(&callee_name)?;
                let callee_ensures: Vec<Expr> = callee_func
                    .body
                    .iter()
                    .filter_map(|s| {
                        if let Stmt::Ensures(e, _) = s.unlocated() {
                            Some(e.clone())
                        } else {
                            None
                        }
                    })
                    .collect();

                if callee_ensures.is_empty() {
                    return None; // No ensures to inline
                }

                // Recursively inline calls in arguments first
                let new_args: Vec<Expr> = args
                    .iter()
                    .map(|a| self.inline_calls_in_expr(a, counter, injected))
                    .collect::<Option<Vec<_>>>()?;

                // Generate fresh variable for the call result
                let fresh_name = format!("__call_{}", counter);
                *counter += 1;

                // Collect callee requires: at the call site they become proof
                // obligations, not free assumptions (#41, full-audit-2026-08-05 §11).
                let callee_requires: Vec<Expr> = callee_func
                    .body
                    .iter()
                    .filter_map(|s| {
                        if let Stmt::Requires(e, _) = s.unlocated() {
                            Some(e.clone())
                        } else {
                            None
                        }
                    })
                    .collect();

                // Substitute callee ensures: params → args, result → fresh_var
                let callee_params = callee_func.params.clone();
                let span = expr.meta().map(|m| m.span).unwrap_or(Span::UNKNOWN);
                if callee_requires.is_empty() {
                    for ens_expr in &callee_ensures {
                        let substituted =
                            self.substitute_call(ens_expr, &callee_params, &new_args, &fresh_name);
                        // Inject as a requires statement (assumed precondition, not
                        // a postcondition to prove). The VIR lowering maps
                        // Stmt::Requires → VStmt::Assume.
                        injected.push(Stmt::Requires(substituted, span));
                    }
                } else {
                    // #41: gate the injected ensures on the callee's requires,
                    // substituted at the call site. Injecting ensures
                    // unconditionally let a caller prove its postcondition
                    // from a callee contract whose precondition it never
                    // satisfies (e.g. `double(x)` with x outside the safe
                    // range) — a fake Verified. With `requires ⇒ ensures`
                    // injected as the assumption, the call-site precondition
                    // becomes a proof obligation: only callers that can
                    // derive it (their requires imply the callee's) get the
                    // full callee contract.
                    let req_conj = callee_requires
                        .iter()
                        .map(|r| self.substitute_call(r, &callee_params, &new_args, &fresh_name))
                        .reduce(|acc, r| Expr::Binary(BinOp::And, Box::new(acc), Box::new(r)))
                        .expect("callee_requires is non-empty here");
                    let ens_conj = callee_ensures
                        .iter()
                        .map(|e| self.substitute_call(e, &callee_params, &new_args, &fresh_name))
                        .reduce(|acc, e| Expr::Binary(BinOp::And, Box::new(acc), Box::new(e)))
                        .expect("callee_ensures is non-empty here");
                    let implication = Expr::Binary(
                        BinOp::Or,
                        Box::new(Expr::Unary(UnOp::Not, Box::new(req_conj))),
                        Box::new(ens_conj),
                    );
                    injected.push(Stmt::Requires(implication, span));
                }

                // Replace the call with the fresh variable
                Some(Expr::Ident(fresh_name))
            }
            Expr::Binary(op, lhs, rhs) => {
                let new_lhs = self.inline_calls_in_expr(lhs, counter, injected)?;
                let new_rhs = self.inline_calls_in_expr(rhs, counter, injected)?;
                Some(Expr::Binary(*op, Box::new(new_lhs), Box::new(new_rhs)))
            }
            Expr::Unary(op, inner) => {
                let new_inner = self.inline_calls_in_expr(inner, counter, injected)?;
                Some(Expr::Unary(*op, Box::new(new_inner)))
            }
            Expr::Old(inner) => {
                let new_inner = self.inline_calls_in_expr(inner, counter, injected)?;
                Some(Expr::Old(Box::new(new_inner)))
            }
            // Leaf expressions: no calls to inline
            Expr::Literal(_) | Expr::Ident(_) => Some(expr.clone()),
            // Complex expressions (If, Match, Block, etc.): can't inline in v1
            _ => None,
        }
    }

    /// VIR-based verification path (0.31.26).
    ///
    /// Attempts to verify a function using the Verification IR:
    /// 1. Check trusted-subset gate
    /// 2. Lower FuncDef → VFunction
    /// 3. Encode VFunction → Z3
    /// 4. Check verification conditions
    ///
    /// Returns `None` if the function is not in the trusted subset
    /// (caller should fall back to the AST-based path).
    /// Returns `Some(result)` if verification was attempted via VIR.
    ///
    /// 0.31.27+: Callee ensures propagation. If the gate rejects due to
    /// calls, we attempt to inline callee ensures: replace each call to a
    /// verified function with a fresh variable and inject the callee's
    /// ensures as additional requires. This extends VIR coverage to
    /// functions that call verified pure functions.
    pub(crate) fn verify_func_vir(
        &mut self,
        session: &mut SolverSession,
        func: &FuncDef,
    ) -> Option<VerificationResult> {
        use crate::verifier::vir::{self, VStmt, VirZ3Ctx};

        let start = Instant::now();

        // 1. Trusted-subset gate
        // 0.31.27+: If the gate rejects due to calls, try inlining callee
        // ensures from verified functions. This extends VIR coverage to
        // functions that call verified pure functions.
        let effective_func: FuncDef;
        let func_ref: &FuncDef = if vir::check_trusted_subset(func).is_err() {
            // Gate rejected. Try callee ensures inlining.
            match self.try_inline_callee_ensures(func) {
                Some(inlined) => {
                    // Re-check the gate on the inlined function.
                    if vir::check_trusted_subset(&inlined).is_err() {
                        return None; // Still not in trusted subset; fall back
                    }
                    effective_func = inlined;
                    &effective_func
                }
                None => return None, // No inlinable calls; fall back to AST
            }
        } else {
            func
        };

        // 1b. Fall back to AST path for constructs VIR doesn't handle yet:
        // - invariant statements (VIR doesn't encode loop invariants)
        //
        // 0.31.28: f64 parameters/return are now handled by the VIR path:
        // - f64 arithmetic → NotInTrustedSubset (lowering returns None)
        // - f64 comparison → F64Compare (uninterpreted predicate)
        let has_invariant = func_ref
            .body
            .iter()
            .any(|s| matches!(s.unlocated(), Stmt::Invariant(..)));
        if has_invariant {
            return None; // Fall back to AST-based path
        }

        // 2. Lower to VIR
        let (vfunc, _span_table) = match vir::lower_func_to_vir(func_ref) {
            Ok(result) => result,
            Err(reason) => {
                // 0.31.28: Lowering failed. If the function has f64 parameters
                // or return type, this is likely because of f64 arithmetic
                // (which is NOT in the trusted subset). Return NotInTrustedSubset
                // instead of falling back to the AST path.
                let has_f64 = func_ref
                    .params
                    .iter()
                    .any(|p| matches!(p.ty.unlocated(), Type::Name(n, _) if n == "f64"))
                    || func_ref
                        .ret
                        .as_ref()
                        .is_some_and(|t| matches!(t.unlocated(), Type::Name(n, _) if n == "f64"));
                if has_f64 {
                    return Some(VerificationResult {
                        func_name: func.name.clone(),
                        status: VerifStatus::NotInTrustedSubset,
                        message: format!(
                            "f64 arithmetic is not in the trusted subset (IEEE 754 rounding not modeled): {}",
                            reason
                        ),
                        diagnostic: None,
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count: 0,
                        artifact: None,
                        trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
                    });
                }
                return None; // Lowering failed for other reasons; fall back
            }
        };

        // 2b. Compute VC artifact (semantics hash for proof caching)
        let vir_hash = crate::verifier::ctx::compute_semantic_hash(&vfunc.normalized_repr());
        let artifact = Some(crate::verifier::ctx::ProofArtifact {
            semantics_version: crate::verifier::ctx::ProofArtifact::SEMANTICS_VERSION,
            // P0-9: i32 checked, i64 unbounded (no definedness). See ctx.rs.
            integer_model: "checked_i32".to_string(),
            float_model: "opaque".to_string(),
            solver_version: format!("z3 {}", z3::full_version()),
            // P1-24: hashes plumbed from verify_source / verify_checked entry.
            source_hash: self.source_hash.clone(),
            resolved_ir_hash: self.resolved_ir_hash.clone(),
            vir_hash,
            // 0.34.44 (ADR-008 §2): this is the flow/VIR engine (demoted to
            // the math: channel; retirement registered on the 0.2 track).
            engine: crate::verifier::ctx::ProofArtifact::ENGINE_FLOW_AST.to_string(),
        });

        // Check if there are any contracts to verify
        let has_requires = vfunc.body.iter().any(|s| matches!(s, VStmt::Assume(_)));
        let has_ensures = !vfunc.postconditions.is_empty();
        let has_math = vfunc.body.iter().any(|s| matches!(s, VStmt::Assert(_)));

        if !has_requires && !has_ensures && !has_math {
            // No contracts at all — fall back to AST path (which checks call sites)
            return None;
        }

        // 3. Set up Z3 encoding context
        let returns_f64 = func_ref
            .ret
            .as_ref()
            .is_some_and(|t| matches!(t.unlocated(), Type::Name(n, _) if n == "f64"));
        let returns_bool = func_ref.ret.as_ref().is_some_and(
            |t| matches!(t.unlocated(), Type::Name(n, _) if n == "bool" || n == "Bool"),
        );

        let mut z3ctx = VirZ3Ctx::new(&vfunc);
        z3ctx.setup_result(returns_f64, returns_bool);

        // Assert old(param) == param for all integer parameters.
        // In the trusted subset (no mutation), parameters don't change,
        // so old(x) is always equal to x at function entry.
        // Also assert i32 range constraints for i32 parameters so that
        // overflow checks are sound (Z3 Int is unbounded by default).
        let mut constraint_count = 0usize;
        for &(var, vty, ref _name) in &vfunc.params {
            if let Some(param_z3) = z3ctx.int_vars.get(&var) {
                // old(param) == param
                if let Some(old_z3) = z3ctx.old_int_vars.get(&var) {
                    session.assert(param_z3.eq(old_z3));
                    constraint_count += 1;
                }
                // i32 range constraint: MIN <= x <= MAX
                if vty == crate::verifier::vir::VType::I32 {
                    let lo = z3::ast::Int::from_i64(i32::MIN as i64);
                    let hi = z3::ast::Int::from_i64(i32::MAX as i64);
                    session.assert(z3::ast::Bool::and(&[&param_z3.ge(&lo), &param_z3.le(&hi)]));
                    constraint_count += 1;
                }
                // V-6 (full-audit-2026-08-05-0656 §3.8): i64 params get the
                // machine-range axiom too. Runtime i64 values are by
                // construction inside [i64::MIN, i64::MAX]; the axiom makes
                // the new i64 div/mod/MIN÷-1 obligations discharge exactly as
                // they do for i32 (unbounded Z3 Int otherwise).
                if vty == crate::verifier::vir::VType::I64 {
                    let lo = z3::ast::Int::from_i64(i64::MIN);
                    let hi = z3::ast::Int::from_i64(i64::MAX);
                    session.assert(z3::ast::Bool::and(&[&param_z3.ge(&lo), &param_z3.le(&hi)]));
                    constraint_count += 1;
                }
            }
            // V-2 (full-audit-2026-08-05-0656 §3.8): bool params get
            // old(param) == param too (the trusted subset is immutable, so
            // old(b) is always b). Previously only int params were asserted;
            // `ensures: old(b) == b` was unencodable in the VIR path while
            // the Resolved engine completed it — engine inconsistency.
            if let (Some(param_z3), Some(old_z3)) =
                (z3ctx.bool_vars.get(&var), z3ctx.old_bool_vars.get(&var))
            {
                session.assert(param_z3.eq(old_z3));
                constraint_count += 1;
            }
        }

        // 4. Process body statements
        // 0.31.29 audit P1-3: all encoding failures are fail-closed (return
        // NotInTrustedSubset instead of silently skipping).

        // 0.31.27+: Pre-register all variables in the VIR body.
        // Callee ensures inlining introduces fresh variables (__call_N) in
        // Assume statements that precede their Let bindings. Without
        // pre-registration, encode_bool/encode_int would fail to find them.
        // Scan all VStmts for VarIds and register any not yet known.
        {
            use crate::verifier::vir::VExpr;
            fn collect_var_ids(
                expr: &VExpr,
                out: &mut Vec<(crate::verifier::vir::VarId, crate::verifier::vir::VType)>,
            ) {
                match expr {
                    VExpr::Var(id) => {
                        // Infer type from context: default to I64
                        out.push((*id, crate::verifier::vir::VType::I64));
                    }
                    VExpr::Old(id) => {
                        out.push((*id, crate::verifier::vir::VType::I64));
                    }
                    VExpr::CheckedArith(_, l, r, ty) => {
                        collect_var_ids(l, out);
                        collect_var_ids(r, out);
                        // Also register the result type
                        let _ = ty;
                    }
                    VExpr::CheckedNeg(inner, _) => collect_var_ids(inner, out),
                    VExpr::Compare(_, l, r) | VExpr::F64Compare(_, l, r) => {
                        collect_var_ids(l, out);
                        collect_var_ids(r, out);
                    }
                    VExpr::Boolean(_, operands) => {
                        for op in operands {
                            collect_var_ids(op, out);
                        }
                    }
                    VExpr::Not(inner) => collect_var_ids(inner, out),
                    VExpr::Select(c, t, e) => {
                        collect_var_ids(c, out);
                        collect_var_ids(t, out);
                        collect_var_ids(e, out);
                    }
                    VExpr::OpaqueF64(id) => {
                        out.push((*id, crate::verifier::vir::VType::F64Opaque));
                    }
                    _ => {}
                }
            }
            let mut all_vars: Vec<(crate::verifier::vir::VarId, crate::verifier::vir::VType)> =
                Vec::new();
            for stmt in &vfunc.body {
                match stmt {
                    VStmt::Assume(expr) | VStmt::Assert(expr) => {
                        collect_var_ids(expr, &mut all_vars)
                    }
                    VStmt::Let(var, expr) => {
                        let vty = expr.ty().unwrap_or(crate::verifier::vir::VType::I64);
                        all_vars.push((*var, vty));
                        collect_var_ids(expr, &mut all_vars);
                    }
                    VStmt::Return(expr) => collect_var_ids(expr, &mut all_vars),
                }
            }
            // Also scan postconditions
            for pc in &vfunc.postconditions {
                collect_var_ids(pc, &mut all_vars);
            }
            for (var, vty) in all_vars {
                if !z3ctx.var_types.contains_key(&var) {
                    z3ctx.register_let(var, vty);
                }
            }
        }

        for stmt in &vfunc.body {
            match stmt {
                VStmt::Assume(expr) => {
                    // Precondition / invariant assumption
                    match z3ctx.encode_bool(expr) {
                        Some(z3_bool) => {
                            session.assert(&z3_bool);
                            constraint_count += 1;
                        }
                        None => {
                            return Some(VerificationResult {
                                func_name: func.name.clone(),
                                status: VerifStatus::NotInTrustedSubset,
                                message: "cannot encode precondition (assumption) in VIR".into(),
                                diagnostic: None,
                                duration_us: start.elapsed().as_micros() as u64,
                                constraint_count,
                                artifact: artifact.clone(),
                                trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
                            });
                        }
                    }
                }
                VStmt::Assert(expr) => {
                    // Math obligation — prove from current assumptions
                    match z3ctx.encode_bool(expr) {
                        Some(z3_bool) => {
                            let (proof, _) = session.check_scope(z3_bool.not());
                            match proof {
                                SatResult::Unsat => {
                                    session.assert(&z3_bool);
                                    constraint_count += 1;
                                }
                                SatResult::Sat => {
                                    return Some(VerificationResult {
                                        func_name: func.name.clone(),
                                        status: VerifStatus::Disproven,
                                        message:
                                            "math obligation is not implied by preconditions (VIR)"
                                                .into(),
                                        diagnostic: Some(Diagnostic::error(
                                            format!("unproven math obligation in '{}'", func.name),
                                            func.meta.span,
                                        )),
                                        duration_us: start.elapsed().as_micros() as u64,
                                        constraint_count,
                                        artifact: artifact.clone(),
                                        trusted_subset_domain: None,
                                    });
                                }
                                SatResult::Unknown => {
                                    return Some(VerificationResult {
                                        func_name: func.name.clone(),
                                        status: session.unknown_status(), // §11-#50
                                        message: "solver could not prove math obligation (VIR)"
                                            .into(),
                                        diagnostic: None,
                                        duration_us: start.elapsed().as_micros() as u64,
                                        constraint_count,
                                        artifact: artifact.clone(),
                                        trusted_subset_domain: None,
                                    });
                                }
                            }
                        }
                        None => {
                            return Some(VerificationResult {
                                func_name: func.name.clone(),
                                status: VerifStatus::NotInTrustedSubset,
                                message: "cannot encode math obligation in VIR".into(),
                                diagnostic: None,
                                duration_us: start.elapsed().as_micros() as u64,
                                constraint_count,
                                artifact: artifact.clone(),
                                trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
                            });
                        }
                    }
                }
                VStmt::Let(var, expr) => {
                    // C-5 (full-audit-2026-08-05-0656 §1): the init
                    // expression's definedness obligations MUST be checked
                    // here. Previously obligations were collected ONLY in the
                    // Return arm — `let y = x / z` with a possible z == 0
                    // traps E0801 at runtime yet verified Proven because the
                    // division hides in a Let, and `VStmt::Return(Var(y))`
                    // carries no CheckedArith node. P0-8 plugged non-tail
                    // `Stmt::Expr` and missed Let. Trap ≠ Fault.
                    if let Some(result) = self.vir_check_definedness(
                        session,
                        &z3ctx,
                        expr,
                        &func.name,
                        func.meta.span,
                        start,
                        constraint_count,
                        &artifact,
                    ) {
                        return Some(result);
                    }
                    // Register the let-bound variable and assert its value
                    let vty = z3ctx
                        .var_types
                        .get(var)
                        .copied()
                        .unwrap_or(crate::verifier::vir::VType::I64);
                    z3ctx.register_let(*var, vty);
                    // Assert the let binding: var == expr
                    match vty {
                        crate::verifier::vir::VType::Bool => match z3ctx.encode_bool(expr) {
                            Some(body_z3) => {
                                if let Some(v) = z3ctx.bool_vars.get(var) {
                                    session.assert(v.eq(&body_z3));
                                    constraint_count += 1;
                                }
                            }
                            None => {
                                return Some(VerificationResult {
                                    func_name: func.name.clone(),
                                    status: VerifStatus::NotInTrustedSubset,
                                    message: "cannot encode let binding (bool) in VIR".into(),
                                    diagnostic: None,
                                    duration_us: start.elapsed().as_micros() as u64,
                                    constraint_count,
                                    artifact: artifact.clone(),
                                    trusted_subset_domain: Some(TrustedSubsetDomain::Body),
                                });
                            }
                        },
                        crate::verifier::vir::VType::F64Opaque => {
                            // §11-#47 (audit 2026-08-05, closed 2026-08-07):
                            // f64 let 绑定此前静默跳过（变量不绑定到初始化
                            // 表达式，后续契约对它的约束凭空成立/失效）。
                            // 现经 encode_f64 断言恒等（opaque 无算术语义，
                            // 仅 equality/ordering）；f64 算术表达式不在受信
                            // 子集（lowering 返 None）→ 诚实 NotInTrustedSubset。
                            match z3ctx.encode_f64(expr) {
                                Some(body_z3) => {
                                    if let Some(v) = z3ctx.f64_vars.get(var) {
                                        session.assert(v.eq(&body_z3));
                                        constraint_count += 1;
                                    }
                                }
                                None => {
                                    return Some(VerificationResult {
                                        func_name: func.name.clone(),
                                        status: VerifStatus::NotInTrustedSubset,
                                        message: "cannot encode let binding (f64) in VIR".into(),
                                        diagnostic: None,
                                        duration_us: start.elapsed().as_micros() as u64,
                                        constraint_count,
                                        artifact: artifact.clone(),
                                        trusted_subset_domain: Some(TrustedSubsetDomain::Body),
                                    });
                                }
                            }
                        }
                        _ => match z3ctx.encode_int(expr) {
                            Some(body_z3) => {
                                if let Some(v) = z3ctx.int_vars.get(var) {
                                    session.assert(v.eq(&body_z3));
                                    constraint_count += 1;
                                }
                            }
                            None => {
                                return Some(VerificationResult {
                                    func_name: func.name.clone(),
                                    status: VerifStatus::NotInTrustedSubset,
                                    message: "cannot encode let binding (int) in VIR".into(),
                                    diagnostic: None,
                                    duration_us: start.elapsed().as_micros() as u64,
                                    constraint_count,
                                    artifact: artifact.clone(),
                                    trusted_subset_domain: Some(TrustedSubsetDomain::Body),
                                });
                            }
                        },
                    }
                }
                VStmt::Return(expr) => {
                    // Bind result variable to return expression
                    if returns_bool {
                        match z3ctx.encode_bool(expr) {
                            Some(body_z3) => {
                                if let Some(r) = &z3ctx.result_bool {
                                    session.assert(r.eq(&body_z3));
                                    constraint_count += 1;
                                }
                            }
                            None => {
                                return Some(VerificationResult {
                                    func_name: func.name.clone(),
                                    status: VerifStatus::NotInTrustedSubset,
                                    message: "cannot encode return expression (bool) in VIR".into(),
                                    diagnostic: None,
                                    duration_us: start.elapsed().as_micros() as u64,
                                    constraint_count,
                                    artifact: artifact.clone(),
                                    trusted_subset_domain: Some(TrustedSubsetDomain::Body),
                                });
                            }
                        }
                    } else if !returns_f64 {
                        // Check definedness obligations first
                        if let Some(result) = self.vir_check_definedness(
                            session,
                            &z3ctx,
                            expr,
                            &func.name,
                            func.meta.span,
                            start,
                            constraint_count,
                            &artifact,
                        ) {
                            return Some(result);
                        }
                        // Bind result to return expression
                        match z3ctx.encode_int(expr) {
                            Some(body_z3) => {
                                if let Some(r) = &z3ctx.result_int {
                                    session.assert(r.eq(&body_z3));
                                    constraint_count += 1;
                                }
                            }
                            None => {
                                return Some(VerificationResult {
                                    func_name: func.name.clone(),
                                    status: VerifStatus::NotInTrustedSubset,
                                    message: "cannot encode return expression (int) in VIR".into(),
                                    diagnostic: None,
                                    duration_us: start.elapsed().as_micros() as u64,
                                    constraint_count,
                                    artifact: artifact.clone(),
                                    trusted_subset_domain: Some(TrustedSubsetDomain::Body),
                                });
                            }
                        }
                    }
                    // f64 return: bind result to opaque f64 variable
                    if returns_f64 {
                        match z3ctx.encode_f64(expr) {
                            Some(body_z3) => {
                                if let Some(r) = &z3ctx.result_f64 {
                                    session.assert(r.eq(&body_z3));
                                    constraint_count += 1;
                                }
                            }
                            None => {
                                return Some(VerificationResult {
                                    func_name: func.name.clone(),
                                    status: VerifStatus::NotInTrustedSubset,
                                    message: "cannot encode return expression (f64) in VIR".into(),
                                    diagnostic: None,
                                    duration_us: start.elapsed().as_micros() as u64,
                                    constraint_count,
                                    artifact: artifact.clone(),
                                    trusted_subset_domain: Some(TrustedSubsetDomain::Body),
                                });
                            }
                        }
                    }
                }
            }
        }

        // 5. Check preconditions satisfiability
        match session.check() {
            SatResult::Unsat => {
                return Some(VerificationResult {
                    func_name: func.name.clone(),
                    status: VerifStatus::Disproven,
                    message: "preconditions are unsatisfiable (VIR)".into(),
                    diagnostic: Some(
                        Diagnostic::error(
                            format!("preconditions are unsatisfiable for '{}'", func.name),
                            func.meta.span,
                        )
                        .with_help("check that your requires conditions can actually be satisfied"),
                    ),
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count,
                    artifact: artifact.clone(),
                    trusted_subset_domain: None,
                });
            }
            SatResult::Unknown => {
                return Some(VerificationResult {
                    func_name: func.name.clone(),
                    status: session.unknown_status(), // §11-#50
                    message: "precondition satisfiability unknown (VIR)".into(),
                    diagnostic: None,
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count,
                    artifact: artifact.clone(),
                    trusted_subset_domain: None,
                });
            }
            SatResult::Sat => {
                // Preconditions satisfiable; check postconditions
            }
        }

        // 6. Check postconditions (ensures)
        if vfunc.postconditions.is_empty() {
            return Some(VerificationResult {
                func_name: func.name.clone(),
                status: VerifStatus::Proven,
                message: "preconditions satisfiable, no postconditions (VIR)".into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
                artifact: artifact.clone(),
                trusted_subset_domain: None,
            });
        }

        let mut found_violation = false;
        let mut found_unknown = false;
        let mut viol_model: Option<z3::Model> = None;
        let mut viol_index: usize = 0;

        for (idx, post) in vfunc.postconditions.iter().enumerate() {
            match z3ctx.encode_bool(post) {
                Some(z3_bool) => {
                    let (result, model) = session.check_scope(z3_bool.not());
                    match result {
                        SatResult::Sat => {
                            found_violation = true;
                            viol_model = model;
                            viol_index = idx;
                            break;
                        }
                        SatResult::Unknown => {
                            found_unknown = true;
                        }
                        SatResult::Unsat => {
                            // This postcondition holds
                        }
                    }
                }
                None => {
                    // 0.31.29 audit P1-3: fail-closed. Cannot silently skip
                    // a postcondition — that would produce false Proven.
                    return Some(VerificationResult {
                        func_name: func.name.clone(),
                        status: VerifStatus::NotInTrustedSubset,
                        message: format!("cannot encode postcondition {} in VIR", idx),
                        diagnostic: None,
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count,
                        artifact: artifact.clone(),
                        trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
                    });
                }
            }
        }

        if found_violation {
            // Extract counterexample from the Z3 model
            let counterexample_msg =
                Self::format_vir_counterexample(&viol_model, &z3ctx, &vfunc, viol_index);
            let mut message = format!(
                "verification failed for '{}' (VIR): postcondition not satisfied",
                func.name
            );
            if !counterexample_msg.is_empty() {
                message.push_str(&format!("\n{}", counterexample_msg));
            }
            // Build diagnostic with counterexample details
            let mut diag_msg = format!("postcondition violated in '{}'", func.name);
            if !counterexample_msg.is_empty() {
                diag_msg.push_str(&format!("\n{}", counterexample_msg));
            }
            Some(VerificationResult {
                func_name: func.name.clone(),
                status: VerifStatus::Disproven,
                message,
                diagnostic: Some(Diagnostic::error(diag_msg, func.meta.span)),
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
                artifact: artifact.clone(),
                trusted_subset_domain: None,
            })
        } else if found_unknown {
            Some(VerificationResult {
                func_name: func.name.clone(),
                status: session.unknown_status(), // §11-#50
                message: "verification inconclusive (VIR)".into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
                artifact: artifact.clone(),
                trusted_subset_domain: None,
            })
        } else {
            // V-1 (0.31.55): check if any postcondition contains checked
            // arithmetic. With unbounded Z3 Int, overflow/div-by-zero are
            // not modeled — the proof assumes definedness. Report this
            // assumption transparently instead of claiming unconditional Proven.
            let has_arith = vfunc
                .postconditions
                .iter()
                .any(|post| post.contains_checked_arith());
            let (status, message) = if has_arith {
                (
                    VerifStatus::Proven,
                    // §11-#46 (2026-08-07): body arithmetic definedness
                    // (overflow/div-by-zero, i32 + i64) is obligation-checked;
                    // only POSTCONDITION arithmetic stays assumed-defined
                    // under the unbounded Int model.
                    "postconditions verified (VIR; postcondition arithmetic assumed defined — integers modeled as unbounded Int)"
                        .into(),
                )
            } else {
                (VerifStatus::Proven, "postconditions verified (VIR)".into())
            };
            Some(VerificationResult {
                func_name: func.name.clone(),
                status,
                message,
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
                artifact: artifact.clone(),
                trusted_subset_domain: None,
            })
        }
    }

    /// M5 (0.35.40): verify a Flow transition's typestate context.
    ///
    /// The transition's executable body (record construction, `self` field
    /// access) is out of the Z3 trusted subset and is verified by the checker.
    /// This path proves the contract-level obligation only:
    ///
    /// `(source_invariants ∧ transition_guards) ⊢ target_invariants`
    ///
    /// where the three fields are populated by `lower_transition_to_vir` from
    /// the transition body's own `invariant:`/`requires:`/`ensures:` clauses
    /// (all checker-verified before the verifier runs).
    ///
    /// Returns `None` when the transition is not VIR-eligible (params out of
    /// the trusted subset, no contracts, or contracts cannot be lowered) — the
    /// caller then falls back to the AST path.
    pub(crate) fn verify_transition_vir(
        &self,
        session: &mut SolverSession,
        flow_name: &str,
        transition: &TransitionDef,
    ) -> Option<VerificationResult> {
        use crate::verifier::vir::{self, VType, VirZ3Ctx};

        let start = Instant::now();
        let name = format!("{}::{}", flow_name, transition.name);

        // Gate: parameters must be in the trusted subset (the executable body
        // is not encoded, so only param/ret types are gated).
        let gate_func = vir::synthesize_transition_func(flow_name, transition, vec![]);
        if vir::check_trusted_subset(&gate_func).is_err() {
            return None;
        }

        // No contracts → nothing to prove at the typestate level; let the AST
        // path handle the (executable) body.
        let has_contracts = transition.body.as_ref().is_some_and(|b| {
            b.iter().any(|s| {
                matches!(
                    s.unlocated(),
                    Stmt::Requires(..) | Stmt::Ensures(..) | Stmt::Invariant(..)
                )
            })
        });
        if !has_contracts {
            return None;
        }

        // Lower to VIR with the typestate context populated.
        let (vfunc, _span_table) = match vir::lower_transition_to_vir(flow_name, transition) {
            Ok(result) => result,
            Err(_) => return None, // contract not lowerable → AST fallback
        };

        let vir_hash = crate::verifier::ctx::compute_semantic_hash(&vfunc.normalized_repr());
        let artifact = Some(crate::verifier::ctx::ProofArtifact {
            semantics_version: crate::verifier::ctx::ProofArtifact::SEMANTICS_VERSION,
            integer_model: "checked_i32".to_string(),
            float_model: "opaque".to_string(),
            solver_version: format!("z3 {}", z3::full_version()),
            source_hash: self.source_hash.clone(),
            resolved_ir_hash: self.resolved_ir_hash.clone(),
            vir_hash,
            engine: crate::verifier::ctx::ProofArtifact::ENGINE_FLOW_AST.to_string(),
        });

        let z3ctx = VirZ3Ctx::new(&vfunc);
        let mut constraint_count = 0usize;

        // Soundness axioms: old(param) == param and machine-range constraints
        // for integer params (mirrors verify_func_vir). Transitions do not
        // mutate parameters, so old(param) is always param.
        for &(var, vty, _) in &vfunc.params {
            if let Some(param_z3) = z3ctx.int_vars.get(&var) {
                if let Some(old_z3) = z3ctx.old_int_vars.get(&var) {
                    session.assert(param_z3.eq(old_z3));
                    constraint_count += 1;
                }
                let (lo, hi) = match vty {
                    VType::I32 => (i32::MIN as i64, i32::MAX as i64),
                    VType::I64 => (i64::MIN, i64::MAX),
                    _ => continue,
                };
                let lo = z3::ast::Int::from_i64(lo);
                let hi = z3::ast::Int::from_i64(hi);
                session.assert(z3::ast::Bool::and(&[&param_z3.ge(&lo), &param_z3.le(&hi)]));
                constraint_count += 1;
            }
            if let (Some(b), Some(ob)) = (z3ctx.bool_vars.get(&var), z3ctx.old_bool_vars.get(&var))
            {
                session.assert(b.eq(ob));
                constraint_count += 1;
            }
        }

        let ts = vfunc
            .typestate_context
            .as_ref()
            .expect("transition VIR carries typestate context");

        // Assert source invariants (axioms) and transition guards (preconditions).
        for inv in ts
            .source_invariants
            .iter()
            .chain(ts.transition_guards.iter())
        {
            match z3ctx.encode_bool(inv) {
                Some(z3_bool) => {
                    session.assert(&z3_bool);
                    constraint_count += 1;
                }
                None => {
                    return Some(VerificationResult {
                        func_name: name,
                        status: VerifStatus::NotInTrustedSubset,
                        message: "cannot encode transition invariant/guard in VIR".into(),
                        diagnostic: None,
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count,
                        artifact,
                        trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
                    });
                }
            }
        }

        // Prove target invariants.
        let mut found_unknown = false;
        for (idx, target) in ts.target_invariants.iter().enumerate() {
            match z3ctx.encode_bool(target) {
                Some(z3_bool) => {
                    let (result, _model) = session.check_scope(z3_bool.not());
                    match result {
                        SatResult::Sat => {
                            return Some(VerificationResult {
                                func_name: name,
                                status: VerifStatus::Disproven,
                                message: format!(
                                    "transition target invariant {} not implied by source invariants and guards (VIR)",
                                    idx
                                ),
                                diagnostic: Some(Diagnostic::error(
                                    format!(
                                        "target invariant violated in transition '{}'",
                                        transition.name
                                    ),
                                    transition.meta.span,
                                )),
                                duration_us: start.elapsed().as_micros() as u64,
                                constraint_count,
                                artifact,
                                trusted_subset_domain: None,
                            });
                        }
                        SatResult::Unknown => {
                            found_unknown = true;
                        }
                        SatResult::Unsat => {}
                    }
                }
                None => {
                    return Some(VerificationResult {
                        func_name: name,
                        status: VerifStatus::NotInTrustedSubset,
                        message: format!(
                            "cannot encode transition target invariant {} in VIR",
                            idx
                        ),
                        diagnostic: None,
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count,
                        artifact,
                        trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
                    });
                }
            }
        }

        if found_unknown {
            Some(VerificationResult {
                func_name: name,
                status: session.unknown_status(),
                message: "transition typestate verification inconclusive (VIR)".into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
                artifact,
                trusted_subset_domain: None,
            })
        } else {
            Some(VerificationResult {
                func_name: name,
                status: VerifStatus::Proven,
                message: "transition typestate context verified (VIR)".into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
                artifact,
                trusted_subset_domain: None,
            })
        }
    }

    /// Verify a Flow transition: typestate-context VIR proof first, falling
    /// back to the AST path (which verifies the executable body and its call
    /// sites) when the transition is not VIR-eligible.
    pub(crate) fn verify_transition(
        &mut self,
        session: &mut SolverSession,
        flow_name: &str,
        transition: &TransitionDef,
    ) -> VerificationResult {
        if let Some(result) = self.verify_transition_vir(session, flow_name, transition) {
            return result;
        }
        let func = crate::verifier::vir::synthesize_transition_func(
            flow_name,
            transition,
            transition.body.clone().unwrap_or_default(),
        );
        self.verify_func(session, &func)
    }

    /// C-5 (full-audit-2026-08-05-0656 §1): shared definedness checker for
    /// the VIR path. Every `VStmt` whose expression can trap at runtime
    /// (checked div/mod/overflow/neg on machine integers) must discharge its
    /// obligations against the assumptions established SO FAR — Let arms
    /// included. Trap ≠ Fault: a body that can E0801-trap must never be
    /// reported Proven.
    ///
    /// Returns `Some(result)` when an obligation is violated (Disproven) or
    /// the solver is undecided (SolverUnknown); `None` when every obligation
    /// was proved and asserted back into the session.
    fn vir_check_definedness(
        &self,
        session: &mut SolverSession,
        z3ctx: &crate::verifier::vir::VirZ3Ctx,
        expr: &crate::verifier::vir::VExpr,
        func_name: &str,
        func_span: Span,
        start: Instant,
        constraint_count: usize,
        artifact: &Option<crate::verifier::ctx::ProofArtifact>,
    ) -> Option<VerificationResult> {
        let obligations = z3ctx.definedness_obligations(expr);
        for (condition, failure) in obligations {
            let (proof, _) = session.check_scope(condition.not());
            match proof {
                SatResult::Unsat => {
                    session.assert(&condition);
                }
                SatResult::Sat => {
                    return Some(VerificationResult {
                        func_name: func_name.to_string(),
                        status: VerifStatus::Disproven,
                        message: failure.to_string(),
                        diagnostic: Some(Diagnostic::error(failure.to_string(), func_span)),
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count,
                        artifact: artifact.clone(),
                        trusted_subset_domain: None,
                    });
                }
                SatResult::Unknown => {
                    return Some(VerificationResult {
                        func_name: func_name.to_string(),
                        status: VerifStatus::SolverUnknown,
                        message: format!(
                            "solver could not prove integer definedness (VIR): {}",
                            failure
                        ),
                        diagnostic: None,
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count,
                        artifact: artifact.clone(),
                        trusted_subset_domain: None,
                    });
                }
            }
        }
        None
    }

    fn extract_counterexample(
        &self,
        model: &Option<z3::Model>,
        vars: &Z3VarMap,
        ensures_exprs: &[Expr],
    ) -> Counterexample {
        let mut assignments = Vec::new();
        let mut real_assignments = Vec::new();
        let mut string_assignments = Vec::new();

        if let Some(model) = model {
            for (name, z3_var) in &vars.int_vars {
                if name == "result" || name.starts_with("old.") || name.starts_with('_') {
                    continue;
                }
                if let Some(val) = model.eval(z3_var, true) {
                    if let Some(i) = val.as_i64() {
                        assignments.push((name.clone(), i));
                    }
                }
            }
            if let Some(z3_var) = vars.int_vars.get("result") {
                if let Some(val) = model.eval(z3_var, true) {
                    if let Some(i) = val.as_i64() {
                        assignments.push(("result".to_string(), i));
                    }
                }
            }
            for (name, z3_var) in &vars.real_vars {
                if name == "result" || name.starts_with("old.") {
                    continue;
                }
                if let Some(val) = model.eval(z3_var, true) {
                    // AU-C2: skip den==0 (would divide by zero).
                    if let Some((num, den)) = val.as_rational() {
                        if den != 0 {
                            let f = (num as f64) / (den as f64);
                            real_assignments.push((name.clone(), f));
                        }
                    }
                }
            }
            if let Some(z3_var) = vars.real_vars.get("result") {
                if let Some(val) = model.eval(z3_var, true) {
                    // AU-C2: skip den==0.
                    if let Some((num, den)) = val.as_rational() {
                        if den != 0 {
                            let f = (num as f64) / (den as f64);
                            real_assignments.push(("result".to_string(), f));
                        }
                    }
                }
            }
            // V5: Collect string variable values for counterexample display.
            for (name, z3_var) in &vars.string_vars {
                if name.starts_with("old.") {
                    continue;
                }
                if let Some(val) = model.eval(z3_var, true) {
                    if let Some(s) = val.as_string() {
                        string_assignments.push((name.clone(), s));
                    }
                }
            }
        }

        let mut violated_indices = Vec::new();
        if let Some(ref m) = model {
            for (idx, ens) in ensures_exprs.iter().enumerate() {
                if !Self::eval_expr_on_model(ens, m, vars) {
                    violated_indices.push(idx);
                }
            }
        }
        if violated_indices.is_empty() && model.is_none() {
            // No model available and no ensures evaluated as violated.
            // Conservatively mark all ensures as potentially violated.
            violated_indices = (0..ensures_exprs.len()).collect();
        }
        // If we have a model but no ensures were violated according to
        // model evaluation, the model may actually satisfy all ensures.
        // Keep violated_indices empty in that case to avoid false positives.

        let violated: Vec<String> = violated_indices
            .iter()
            .map(|&i| format_expr(&ensures_exprs[i]))
            .collect();

        Counterexample {
            assignments,
            real_assignments,
            string_assignments,
            violated_ensures: violated,
            violated_indices,
        }
    }

    /// Format a counterexample from the VIR verification path.
    ///
    /// Extracts variable values from the Z3 model using the VirZ3Ctx
    /// variable maps and the VFunction's parameter names.
    fn format_vir_counterexample(
        model: &Option<z3::Model>,
        z3ctx: &crate::verifier::vir::VirZ3Ctx,
        vfunc: &crate::verifier::vir::VFunction,
        violated_index: usize,
    ) -> String {
        let model = match model {
            Some(m) => m,
            None => return String::new(),
        };

        let mut lines: Vec<String> = Vec::new();
        lines.push("counterexample:".to_string());

        // Extract parameter values (using original names from VFunction)
        for &(var, _vty, ref name) in &vfunc.params {
            // Try int vars first
            if let Some(z3_var) = z3ctx.int_vars.get(&var) {
                if let Some(val) = model.eval(z3_var, true) {
                    if let Some(i) = val.as_i64() {
                        lines.push(format!("    {} = {}", name, i));
                        continue;
                    }
                }
            }
            // Try bool vars
            if let Some(z3_var) = z3ctx.bool_vars.get(&var) {
                if let Some(val) = model.eval(z3_var, true) {
                    if let Some(b) = val.as_bool() {
                        lines.push(format!("    {} = {}", name, b));
                        continue;
                    }
                }
            }
            // Try f64 opaque vars (encoded as Int, no arithmetic semantics)
            if let Some(z3_var) = z3ctx.f64_vars.get(&var) {
                if let Some(val) = model.eval(z3_var, true) {
                    if let Some(i) = val.as_i64() {
                        lines.push(format!("    {} = {} (opaque f64)", name, i));
                    }
                }
            }
        }

        // Extract result value
        if let Some(ref z3_var) = z3ctx.result_int {
            if let Some(val) = model.eval(z3_var, true) {
                if let Some(i) = val.as_i64() {
                    lines.push(format!("    result = {}", i));
                }
            }
        }
        if let Some(ref z3_var) = z3ctx.result_bool {
            if let Some(val) = model.eval(z3_var, true) {
                if let Some(b) = val.as_bool() {
                    lines.push(format!("    result = {}", b));
                }
            }
        }

        // Show which postcondition was violated
        if let Some(post) = vfunc.postconditions.get(violated_index) {
            lines.push(format!("violated ensures[{}]: {}", violated_index, post));
        }

        lines.join("\n")
    }

    /// Try to resolve an expression to a concrete i64 value from the model.
    /// Try to resolve an expression to a concrete string value from the model.
    fn resolve_to_string(expr: &Expr, model: &z3::Model, vars: &Z3VarMap) -> Option<String> {
        match expr.unlocated() {
            Expr::Literal(Lit::String(s)) => Some(s.clone()),
            Expr::Ident(name) => vars.get_string_var(name).and_then(|z3_var| {
                model
                    .eval(z3_var, true)
                    .and_then(|v| v.as_string().map(|s| s.to_string()))
            }),
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.unlocated() {
                    let old_name = format!("old.{}", name);
                    vars.get_string_var(&old_name).and_then(|z3_var| {
                        model
                            .eval(z3_var, true)
                            .and_then(|v| v.as_string().map(|s| s.to_string()))
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn resolve_to_i64(expr: &Expr, model: &z3::Model, vars: &Z3VarMap) -> Option<i64> {
        match expr.unlocated() {
            Expr::Literal(Lit::Int(n)) => Some(*n),
            Expr::Ident(name) => vars
                .get_int(name)
                .and_then(|z3_var| model.eval(z3_var, true).and_then(|v| v.as_i64())),
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.unlocated() {
                    let old_name = format!("old.{}", name);
                    vars.get_int(&old_name)
                        .and_then(|z3_var| model.eval(z3_var, true).and_then(|v| v.as_i64()))
                } else {
                    None
                }
            }
            Expr::Binary(op, lhs, rhs) => {
                let l = Self::resolve_to_i64(lhs, model, vars)?;
                let r = Self::resolve_to_i64(rhs, model, vars)?;
                // P1-22: Use checked arithmetic to avoid panic on
                // overflow/div-by-zero from unconstrained Z3 model values.
                match op {
                    BinOp::Add => l.checked_add(r),
                    BinOp::Sub => l.checked_sub(r),
                    BinOp::Mul => l.checked_mul(r),
                    BinOp::Div => l.checked_div(r),
                    BinOp::Mod => l.checked_rem(r),
                    _ => None,
                }
            }
            Expr::Unary(UnOp::Neg, inner) => {
                Self::resolve_to_i64(inner, model, vars).and_then(|v| v.checked_neg())
            }
            Expr::Spawn(inner) => Self::resolve_to_i64(inner, model, vars),
            Expr::Await(inner) => Self::resolve_to_i64(inner, model, vars),
            _ => None,
        }
    }

    /// Try to resolve an expression to a concrete f64 value from the model.
    fn resolve_to_f64(expr: &Expr, model: &z3::Model, vars: &Z3VarMap) -> Option<f64> {
        match expr.unlocated() {
            Expr::Literal(Lit::Int(n)) => Some(*n as f64),
            Expr::Literal(Lit::Float(f)) => Some(*f),
            Expr::Ident(name) => vars
                .get_real(name)
                .and_then(|z3_var| {
                    model
                        .eval(z3_var, true)
                        .and_then(|v| v.as_rational())
                        // AU-C2: den==0 would produce infinity/SIGFPE.
                        .and_then(|(num, den)| {
                            if den == 0 {
                                None
                            } else {
                                Some(num as f64 / den as f64)
                            }
                        })
                })
                .or_else(|| {
                    vars.get_int(name)
                        .and_then(|z3_var| model.eval(z3_var, true).and_then(|v| v.as_i64()))
                        .map(|v| v as f64)
                }),
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.unlocated() {
                    let old_name = format!("old.{}", name);
                    vars.get_real(&old_name)
                        .and_then(|z3_var| {
                            model
                                .eval(z3_var, true)
                                .and_then(|v| v.as_rational())
                                // AU-C2: den==0 would produce infinity/SIGFPE.
                                .and_then(|(num, den)| {
                                    if den == 0 {
                                        None
                                    } else {
                                        Some(num as f64 / den as f64)
                                    }
                                })
                        })
                        .or_else(|| {
                            vars.get_int(&old_name)
                                .and_then(|z3_var| {
                                    model.eval(z3_var, true).and_then(|v| v.as_i64())
                                })
                                .map(|v| v as f64)
                        })
                } else {
                    None
                }
            }
            Expr::Binary(op, lhs, rhs) => {
                let l = Self::resolve_to_f64(lhs, model, vars)?;
                let r = Self::resolve_to_f64(rhs, model, vars)?;
                match op {
                    BinOp::Add => Some(l + r),
                    BinOp::Sub => Some(l - r),
                    BinOp::Mul => Some(l * r),
                    BinOp::Div => Some(l / r),
                    _ => None,
                }
            }
            Expr::Unary(UnOp::Neg, inner) => Self::resolve_to_f64(inner, model, vars).map(|v| -v),
            Expr::Spawn(inner) => Self::resolve_to_f64(inner, model, vars),
            Expr::Await(inner) => Self::resolve_to_f64(inner, model, vars),
            _ => None,
        }
    }

    fn eval_expr_on_model(expr: &Expr, model: &z3::Model, vars: &Z3VarMap) -> bool {
        match expr.unlocated() {
            Expr::Literal(Lit::Bool(b)) => *b,
            Expr::Ident(name) => {
                if let Some(z3_var) = vars.get_int(name) {
                    match model.eval(z3_var, true) {
                        Some(val) => val.as_i64().map(|i| i != 0).unwrap_or(false),
                        None => false,
                    }
                } else if let Some(z3_var) = vars.get_real(name) {
                    model
                        .eval(z3_var, true)
                        .and_then(|v| v.as_rational())
                        .map(|(num, den)| den != 0 && num != 0)
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.unlocated() {
                    let old_name = format!("old.{}", name);
                    if let Some(z3_var) = vars.get_int(&old_name) {
                        match model.eval(z3_var, true) {
                            Some(val) => val.as_i64().map(|i| i != 0).unwrap_or(false),
                            None => false,
                        }
                    } else if let Some(z3_var) = vars.get_real(&old_name) {
                        // AU-C2: den==0 is not a valid rational truth value.
                        model
                            .eval(z3_var, true)
                            .and_then(|v| v.as_rational())
                            .map(|(num, den)| den != 0 && num != 0)
                            .unwrap_or(false)
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            Expr::Binary(op, lhs, rhs) => match op {
                BinOp::EqCmp => {
                    match (
                        Self::resolve_to_i64(lhs, model, vars),
                        Self::resolve_to_i64(rhs, model, vars),
                    ) {
                        (Some(l), Some(r)) => l == r,
                        _ => match (
                            Self::resolve_to_f64(lhs, model, vars),
                            Self::resolve_to_f64(rhs, model, vars),
                        ) {
                            (Some(l), Some(r)) => l == r,
                            _ => match (
                                Self::resolve_to_string(lhs, model, vars),
                                Self::resolve_to_string(rhs, model, vars),
                            ) {
                                (Some(l), Some(r)) => l == r,
                                _ => false, // P1.1 fix: cannot evaluate — return false (assume violated)
                            },
                        },
                    }
                }
                BinOp::NeCmp => {
                    match (
                        Self::resolve_to_i64(lhs, model, vars),
                        Self::resolve_to_i64(rhs, model, vars),
                    ) {
                        (Some(l), Some(r)) => l != r,
                        _ => match (
                            Self::resolve_to_f64(lhs, model, vars),
                            Self::resolve_to_f64(rhs, model, vars),
                        ) {
                            (Some(l), Some(r)) => l != r,
                            _ => match (
                                Self::resolve_to_string(lhs, model, vars),
                                Self::resolve_to_string(rhs, model, vars),
                            ) {
                                (Some(l), Some(r)) => l != r,
                                _ => false, // P1.1 fix: cannot evaluate — return false (assume violated)
                            },
                        },
                    }
                }
                BinOp::Lt => {
                    match (
                        Self::resolve_to_i64(lhs, model, vars),
                        Self::resolve_to_i64(rhs, model, vars),
                    ) {
                        (Some(l), Some(r)) => l < r,
                        _ => match (
                            Self::resolve_to_f64(lhs, model, vars),
                            Self::resolve_to_f64(rhs, model, vars),
                        ) {
                            (Some(l), Some(r)) => l < r,
                            _ => false,
                        },
                    }
                }
                BinOp::Gt => {
                    match (
                        Self::resolve_to_i64(lhs, model, vars),
                        Self::resolve_to_i64(rhs, model, vars),
                    ) {
                        (Some(l), Some(r)) => l > r,
                        _ => match (
                            Self::resolve_to_f64(lhs, model, vars),
                            Self::resolve_to_f64(rhs, model, vars),
                        ) {
                            (Some(l), Some(r)) => l > r,
                            _ => false,
                        },
                    }
                }
                BinOp::Le => {
                    match (
                        Self::resolve_to_i64(lhs, model, vars),
                        Self::resolve_to_i64(rhs, model, vars),
                    ) {
                        (Some(l), Some(r)) => l <= r,
                        _ => match (
                            Self::resolve_to_f64(lhs, model, vars),
                            Self::resolve_to_f64(rhs, model, vars),
                        ) {
                            (Some(l), Some(r)) => l <= r,
                            _ => false,
                        },
                    }
                }
                BinOp::Ge => {
                    match (
                        Self::resolve_to_i64(lhs, model, vars),
                        Self::resolve_to_i64(rhs, model, vars),
                    ) {
                        (Some(l), Some(r)) => l >= r,
                        _ => match (
                            Self::resolve_to_f64(lhs, model, vars),
                            Self::resolve_to_f64(rhs, model, vars),
                        ) {
                            (Some(l), Some(r)) => l >= r,
                            _ => false,
                        },
                    }
                }
                _ => {
                    let l = Self::eval_expr_on_model(lhs, model, vars);
                    let r = Self::eval_expr_on_model(rhs, model, vars);
                    match op {
                        BinOp::And => l && r,
                        BinOp::Or => l || r,
                        _ => false,
                    }
                }
            },
            Expr::Unary(UnOp::Not, inner) => !Self::eval_expr_on_model(inner, model, vars),
            Expr::Spawn(inner) => Self::eval_expr_on_model(inner, model, vars),
            Expr::Await(inner) => Self::eval_expr_on_model(inner, model, vars),
            _ => true, // unsupported expression types: assume satisfied (avoid false positives in counterexample)
        }
    }

    fn build_failure_narrative(
        &self,
        func: &FuncDef,
        counterexample: &Counterexample,
        requires_exprs: &[Expr],
        ensures_exprs: &[Expr],
        requires_spans: &[Span],
        ensures_spans: &[Span],
    ) -> Diagnostic {
        let func_name = &func.name;

        // Build function signature string for the header
        let param_strs: Vec<String> = func
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, crate::core::fmt_type(&p.ty)))
            .collect();
        let ret_str = func
            .ret
            .as_ref()
            .map(crate::core::fmt_type)
            .unwrap_or_default();

        let input_assignments: Vec<&(String, i64)> = counterexample
            .assignments
            .iter()
            .filter(|(name, _)| name != "result")
            .collect();
        let result_val = counterexample
            .assignments
            .iter()
            .find(|(name, _)| name == "result")
            .map(|(_, val)| *val);
        let result_real = counterexample
            .real_assignments
            .iter()
            .find(|(name, _)| name == "result")
            .map(|(_, val)| *val);

        let mut message = format!(
            "verification failed for '{}' ({} -> {}): postcondition not satisfied",
            func_name,
            param_strs.join(", "),
            if ret_str.is_empty() {
                "void".into()
            } else {
                ret_str
            },
        );

        // Show counterexample values as a block
        let mut counter_lines: Vec<String> = Vec::new();
        for (name, val) in &input_assignments {
            counter_lines.push(format!("    {} = {}", name, val));
        }
        for (name, val) in &counterexample.real_assignments {
            if name != "result" {
                counter_lines.push(format!("    {} = {:.6}", name, val));
            }
        }
        for (name, val) in &counterexample.string_assignments {
            if name != "result" {
                counter_lines.push(format!("    {} = \"{}\"", name, val));
            }
        }
        if !counter_lines.is_empty() {
            message.push_str(&format!("\ncounterexample:\n{}", counter_lines.join("\n")));
        }

        // Show body return value
        if let Some(result) = result_val {
            message.push_str(&format!("\nbody returns: result = {}", result));
        }
        if let Some(result) = result_real {
            message.push_str(&format!("\nbody returns: result = {:.6}", result));
        }

        // Show violated postconditions
        for &idx in counterexample.violated_indices.iter() {
            if let Some(ens) = ensures_exprs.get(idx) {
                message.push_str(&format!(
                    "\nensures {} is false for this input",
                    format_expr(ens)
                ));
            }
        }

        let primary_span = ensures_spans.first().copied().unwrap_or(func.meta.span);
        let mut diag = Diagnostic::error(message, primary_span).with_code("E0500");

        // Add preconditions as a note
        if !requires_exprs.is_empty() {
            let req_strs: Vec<String> = requires_exprs.iter().map(format_expr).collect();
            let req_span = requires_spans.first().copied().unwrap_or(func.meta.span);
            diag = diag.with_note(
                format!("preconditions (all satisfied): {}", req_strs.join(", ")),
                req_span,
            );
        }

        // Add each violated postcondition as a note
        for &idx in counterexample.violated_indices.iter() {
            if let Some(ens) = ensures_exprs.get(idx) {
                let ens_span = ensures_spans.get(idx).copied().unwrap_or(func.meta.span);
                diag = diag.with_note(
                    format!("postcondition '{}' evaluates to false", format_expr(ens)),
                    ens_span,
                );
            }
        }

        if let Some(hint) = self.generate_fix_hint(func, counterexample) {
            diag = diag.with_help(hint);
        }

        diag
    }

    fn generate_fix_hint(&self, func: &FuncDef, counterexample: &Counterexample) -> Option<String> {
        let param_names: Vec<String> = func.params.iter().map(|p| p.name.clone()).collect();
        let result_val = counterexample
            .assignments
            .iter()
            .find(|(name, _)| name == "result")
            .map(|(_, val)| *val);

        if let Some(result) = result_val {
            let body_is_trivial = func.body.iter().all(|s| match s.unlocated() {
                Stmt::Expr(expr) | Stmt::Return(Some(expr)) => {
                    matches!(expr.unlocated(), Expr::Literal(..))
                }
                _ => false,
            });
            if body_is_trivial {
                return Some(format!(
                    "the function body returns a constant value ({}) regardless of input. \
                     Consider computing the result from the parameters: e.g., `result = {}(...)`",
                    result, func.name
                ));
            }
        }

        let mut used_params: Vec<String> = Vec::new();
        for stmt in &func.body {
            collect_idents_in_stmt(stmt, &mut used_params);
        }
        let unused_params: Vec<&str> = param_names
            .iter()
            .filter(|p| !used_params.contains(p))
            .map(|s| s.as_str())
            .collect();
        if !unused_params.is_empty() {
            return Some(format!(
                "parameter(s) `{}` are not used in the function body. \
                 Ensure the result depends on all required inputs.",
                unused_params.join("`, `")
            ));
        }

        let body_is_simple = func.body.iter().all(|s| match s.unlocated() {
            Stmt::Expr(expr) | Stmt::Return(Some(expr)) => {
                matches!(expr.unlocated(), Expr::Binary(..))
            }
            _ => false,
        });

        if body_is_simple && !counterexample.violated_ensures.is_empty() {
            return Some(format!(
                "the function body performs simple arithmetic without edge-case handling. \
                 Review the postconditions: {} and add guards for boundary values.",
                counterexample.violated_ensures.join(", ")
            ));
        }

        None
    }

    /// Walk an expression tree looking for `Expr::Call(Ident(name), args)`
    /// and, for each call to a known function, assert the callee's ensures
    /// as Z3 constraints. This enables cross-module contract reasoning
    /// (e.g., caller can rely on callee's postconditions).
    ///
    /// M4 (audit-triage-0.35.25): a callee postcondition that `expr_to_z3_bool`
    /// cannot encode was silently dropped — the caller then proved against a
    /// weaker context, and a flip to Disproven was untraceable (red line #2).
    /// Thread a `caller_name` + `errors` vec (same contract as the H1
    /// callee-requires walker) and fail closed: the caller is reported
    /// "not verified" naming the unencodable postcondition.
    fn assert_callee_ensures_in_expr(
        &mut self,
        session: &mut SolverSession,
        expr: &Expr,
        vars: &mut Z3VarMap,
        caller_name: &str,
        errors: &mut Vec<(String, String, Span)>,
    ) {
        match expr.unlocated() {
            Expr::Call(callee, call_args) => {
                if let Expr::Ident(name) = callee.unlocated() {
                    // V-C4: only admit ensures from callees that already
                    // verified successfully. Failed/Unknown/unverified
                    // callees must not become axioms for the caller.
                    let callee_ok = self
                        .func_status
                        .get(name)
                        .is_some_and(|s| *s == VerifStatus::Verified);
                    if callee_ok {
                        if let Some(callee_func) = self.func_defs.get(name) {
                            let call_key = expr::call_var_key(name, call_args);
                            // v0.31.6: pre-create the call-result variable so the
                            // substituted ensures (`result` -> Ident(call_key))
                            // encodes via vars.get_int regardless of whether the
                            // call expression has been lowered yet. Without this,
                            // asserting callee ensures *before* the body/definedness
                            // encoding (needed so overflow obligations can use them)
                            // found no call_key var and silently dropped the axiom.
                            vars.get_or_create_int(&call_key);
                            // Clone callee data to avoid borrow conflict with
                            // expr::expr_to_z3_bool (which needs &mut Z3VarMap).
                            let callee_params = callee_func.params.clone();
                            let callee_ensures: Vec<Expr> = callee_func
                                .body
                                .iter()
                                .filter_map(|s| {
                                    if let Stmt::Ensures(e, _) = s.unlocated() {
                                        Some(e.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            let _ = callee_func;
                            for ens_expr in &callee_ensures {
                                let substituted = self.substitute_call(
                                    ens_expr,
                                    &callee_params,
                                    call_args,
                                    &call_key,
                                );
                                if let Some(z3_bool) = expr::expr_to_z3_bool(&substituted, vars) {
                                    session.assert(z3_bool);
                                } else {
                                    // M4 (audit-triage-0.35.25): the axiom could
                                    // not be encoded — previously dropped
                                    // silently, so the caller's proof ran
                                    // against a weaker context with no trace.
                                    // Fail closed with an explicit "not verified"
                                    // (mirrors H1 for preconditions).
                                    errors.push((
                                        caller_name.to_string(),
                                        format!(
                                            "call to '{}' has a postcondition that cannot be encoded for verification — not verified",
                                            name
                                        ),
                                        expr.meta()
                                            .map(|meta| meta.span)
                                            .or_else(|| {
                                                self.func_defs
                                                    .get(caller_name)
                                                    .map(|caller| caller.meta.span)
                                            })
                                            .unwrap_or(Span::UNKNOWN),
                                    ));
                                }
                            }
                        }
                    }
                }
                // Recurse into call arguments
                for arg in call_args {
                    self.assert_callee_ensures_in_expr(session, arg, vars, caller_name, errors);
                }
            }
            Expr::Binary(_, lhs, rhs) => {
                self.assert_callee_ensures_in_expr(session, lhs, vars, caller_name, errors);
                self.assert_callee_ensures_in_expr(session, rhs, vars, caller_name, errors);
            }
            Expr::Unary(_, inner) => {
                self.assert_callee_ensures_in_expr(session, inner, vars, caller_name, errors);
            }
            Expr::Field(obj, _) => {
                self.assert_callee_ensures_in_expr(session, obj, vars, caller_name, errors);
            }
            Expr::TupleIndex(obj, _) => {
                self.assert_callee_ensures_in_expr(session, obj, vars, caller_name, errors);
            }
            Expr::Old(inner) => {
                self.assert_callee_ensures_in_expr(session, inner, vars, caller_name, errors);
            }
            Expr::If {
                cond,
                then_: _,
                else_: _,
            } => {
                // V-C5: path-conditional arms — only condition is unconditional.
                self.assert_callee_ensures_in_expr(session, cond, vars, caller_name, errors);
            }
            Expr::Match(scrutinee, _arms) => {
                // V-C5: match arms are path-conditional; only scrutinee is always run.
                self.assert_callee_ensures_in_expr(session, scrutinee, vars, caller_name, errors);
            }
            Expr::Block(stmts) => {
                for stmt in stmts {
                    if let Stmt::Expr(e) = stmt.unlocated() {
                        self.assert_callee_ensures_in_expr(session, e, vars, caller_name, errors);
                    }
                }
            }
            Expr::Spawn(inner) => {
                self.assert_callee_ensures_in_expr(session, inner, vars, caller_name, errors);
            }
            Expr::Await(inner) => {
                self.assert_callee_ensures_in_expr(session, inner, vars, caller_name, errors);
            }
            Expr::Lambda { body, .. } => {
                for s in body {
                    self.assert_callee_ensures_in_stmt(session, s, vars, caller_name, errors);
                }
            }
            _ => {}
        }
    }

    /// Walk an expression tree modeling length-preserving builtins (sort, reverse).
    /// For each `sort(xs)` or `reverse(xs)` call, assert that the output length
    /// equals the input length: len(result) == len(xs).
    fn assert_builtin_length_preserving(
        &mut self,
        session: &mut SolverSession,
        expr: &Expr,
        vars: &mut Z3VarMap,
    ) {
        match expr.unlocated() {
            Expr::Call(callee, call_args) => {
                if let Expr::Ident(name) = callee.unlocated() {
                    if (name == "sort" || name == "reverse") && call_args.len() == 1 {
                        // len(sort(xs)) == len(xs)
                        if let Some(input_len) = expr::resolve_list_len(&call_args[0], vars) {
                            let len_key = expr::call_var_key("len", std::slice::from_ref(expr));
                            let output_len = vars.get_or_create_int(&len_key);
                            session.assert(output_len.eq(&input_len));
                        }
                    }
                }
                for arg in call_args {
                    self.assert_builtin_length_preserving(session, arg, vars);
                }
            }
            Expr::Binary(_, lhs, rhs) => {
                self.assert_builtin_length_preserving(session, lhs, vars);
                self.assert_builtin_length_preserving(session, rhs, vars);
            }
            Expr::Unary(_, inner) => self.assert_builtin_length_preserving(session, inner, vars),
            Expr::If { cond, then_, else_ } => {
                self.assert_builtin_length_preserving(session, cond, vars);
                if let Some(tail) = block_tail_expr(then_) {
                    self.assert_builtin_length_preserving(session, &tail, vars);
                }
                if let Some(eb) = else_ {
                    if let Some(tail) = block_tail_expr(eb) {
                        self.assert_builtin_length_preserving(session, &tail, vars);
                    }
                }
            }
            Expr::Block(stmts) => {
                if let Some(tail) = block_tail_expr(stmts) {
                    self.assert_builtin_length_preserving(session, &tail, vars);
                }
            }
            Expr::Match(_, arms) => {
                for arm in arms {
                    self.assert_builtin_length_preserving(session, &arm.body, vars);
                }
            }
            _ => {}
        }
    }

    /// Walk function body statements modeling length-preserving builtins.
    fn assert_builtin_length_preserving_in_block(
        &mut self,
        session: &mut SolverSession,
        block: &[Stmt],
        vars: &mut Z3VarMap,
    ) {
        for stmt in block {
            match stmt.unlocated() {
                Stmt::Expr(e) => self.assert_builtin_length_preserving(session, e, vars),
                Stmt::Return(Some(e)) => self.assert_builtin_length_preserving(session, e, vars),
                Stmt::If { cond, then_, else_ } => {
                    self.assert_builtin_length_preserving(session, cond, vars);
                    self.assert_builtin_length_preserving_in_block(session, then_, vars);
                    if let Some(eb) = else_ {
                        self.assert_builtin_length_preserving_in_block(session, eb, vars);
                    }
                }
                Stmt::Block(inner) | Stmt::Arena(inner) => {
                    self.assert_builtin_length_preserving_in_block(session, inner, vars);
                }
                Stmt::While { cond, body } => {
                    self.assert_builtin_length_preserving(session, cond, vars);
                    self.assert_builtin_length_preserving_in_block(session, body, vars);
                }
                Stmt::WhileLet { init, body, .. } => {
                    self.assert_builtin_length_preserving(session, init, vars);
                    self.assert_builtin_length_preserving_in_block(session, body, vars);
                }
                Stmt::Loop(body) | Stmt::Parasteps(body) => {
                    self.assert_builtin_length_preserving_in_block(session, body, vars);
                }
                Stmt::For { iterable, body, .. } => {
                    self.assert_builtin_length_preserving(session, iterable, vars);
                    self.assert_builtin_length_preserving_in_block(session, body, vars);
                }
                Stmt::Let { init: Some(e), .. } => {
                    self.assert_builtin_length_preserving(session, e, vars);
                }
                Stmt::Assign { target, value } => {
                    self.assert_builtin_length_preserving(session, target, vars);
                    self.assert_builtin_length_preserving(session, value, vars);
                }
                _ => {}
            }
        }
    }

    /// Walk function body statements looking for `Expr::Call` nodes and
    /// propagate callee ensures. This complements `assert_callee_ensures_in_expr`
    /// which only walks the tail expression tree. Together they ensure that
    /// calls in let-bindings, assignments, if-branches, etc. are also covered.
    fn assert_callee_ensures_in_block(
        &mut self,
        session: &mut SolverSession,
        stmts: &[Stmt],
        vars: &mut Z3VarMap,
        caller_name: &str,
        errors: &mut Vec<(String, String, Span)>,
    ) {
        for stmt in stmts {
            self.assert_callee_ensures_in_stmt(session, stmt, vars, caller_name, errors);
        }
    }

    fn assert_callee_ensures_in_stmt(
        &mut self,
        session: &mut SolverSession,
        stmt: &Stmt,
        vars: &mut Z3VarMap,
        caller_name: &str,
        errors: &mut Vec<(String, String, Span)>,
    ) {
        match stmt.unlocated() {
            Stmt::Expr(e) | Stmt::Return(Some(e)) => {
                self.assert_callee_ensures_in_expr(session, e, vars, caller_name, errors);
            }
            Stmt::Let {
                init: Some(init), ..
            }
            | Stmt::Assign { value: init, .. } => {
                self.assert_callee_ensures_in_expr(session, init, vars, caller_name, errors);
            }
            Stmt::SharedLet { init, .. } => {
                self.assert_callee_ensures_in_expr(session, init, vars, caller_name, errors);
            }
            Stmt::If {
                cond,
                then_: _,
                else_: _,
            } => {
                // V-C5: only the condition is always evaluated. Callee ensures
                // inside then/else are path-conditional; admitting them as
                // unconditional axioms is unsound. Skip branch bodies until
                // path-condition implication is implemented.
                self.assert_callee_ensures_in_expr(session, cond, vars, caller_name, errors);
            }
            Stmt::While { cond, body: _, .. }
            | Stmt::For {
                iterable: cond,
                body: _,
                ..
            } => {
                // V-C5: loop bodies may execute zero times — do not assert
                // callee ensures from body as axioms.
                self.assert_callee_ensures_in_expr(session, cond, vars, caller_name, errors);
            }
            Stmt::Loop(_body) => {
                // V-C5: skip unconditional body ensures (zero-iteration possible
                // only via break, but still path-sensitive).
            }
            Stmt::Block(body) | Stmt::Arena(body) | Stmt::Unsafe(body) | Stmt::Parasteps(body) => {
                self.assert_callee_ensures_in_block(session, body, vars, caller_name, errors);
            }
            _ => {}
        }
    }

    /// Walk the expand_lets body and check that every function call to a
    /// known callee satisfies the callee's requires (preconditions).
    fn check_callee_requires_in_block(
        &mut self,
        session: &mut SolverSession,
        stmts: &[Stmt],
        vars: &mut Z3VarMap,
        caller_name: &str,
        errors: &mut Vec<(String, String, crate::span::Span)>,
    ) {
        for stmt in stmts {
            self.check_callee_requires_in_stmt(session, stmt, vars, caller_name, errors);
        }
    }

    fn check_callee_requires_in_stmt(
        &mut self,
        session: &mut SolverSession,
        stmt: &Stmt,
        vars: &mut Z3VarMap,
        caller_name: &str,
        errors: &mut Vec<(String, String, crate::span::Span)>,
    ) {
        match stmt.unlocated() {
            Stmt::Expr(e) | Stmt::Return(Some(e)) | Stmt::Break(Some(e)) => {
                self.check_callee_requires_in_expr(session, e, vars, caller_name, errors);
            }
            Stmt::Let {
                init: Some(init), ..
            } => {
                self.check_callee_requires_in_expr(session, init, vars, caller_name, errors);
            }
            // H-25 (full-audit-2026-08-05-0656): the callee-REQUIRES walker
            // previously stopped at Let/If/While while the callee-ENSURES
            // walker already covered Assign/SharedLet/For. `z = pos(y)` /
            // `shared s = pos(y)` / `for v in [y] { pos(y) }` thus assumed
            // pos's ensures without ever discharging pos's requires: y > 0 —
            // a guaranteed-violation trap verified Proven. Unlike the ensures
            // walker (axioms must be unconditional), the requires walker is a
            // fail-closed safety check: every statement that CAN execute must
            // discharge the preconditions of the calls it may perform, so
            // Assign/SharedLet values AND loop bodies are all walked.
            Stmt::Assign { target, value } => {
                self.check_callee_requires_in_expr(session, target, vars, caller_name, errors);
                self.check_callee_requires_in_expr(session, value, vars, caller_name, errors);
            }
            Stmt::SharedLet { init, .. } => {
                self.check_callee_requires_in_expr(session, init, vars, caller_name, errors);
            }
            Stmt::If { cond, then_, else_ } => {
                self.check_callee_requires_in_expr(session, cond, vars, caller_name, errors);
                self.check_callee_requires_in_block(session, then_, vars, caller_name, errors);
                if let Some(eb) = else_ {
                    self.check_callee_requires_in_block(session, eb, vars, caller_name, errors);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.check_callee_requires_in_expr(session, cond, vars, caller_name, errors);
                self.check_callee_requires_in_block(session, body, vars, caller_name, errors);
            }
            Stmt::WhileLet { init, body, .. } => {
                self.check_callee_requires_in_expr(session, init, vars, caller_name, errors);
                self.check_callee_requires_in_block(session, body, vars, caller_name, errors);
            }
            Stmt::For { iterable, body, .. } => {
                self.check_callee_requires_in_expr(session, iterable, vars, caller_name, errors);
                self.check_callee_requires_in_block(session, body, vars, caller_name, errors);
            }
            Stmt::Loop(body)
            | Stmt::Block(body)
            | Stmt::Arena(body)
            | Stmt::Unsafe(body)
            | Stmt::Parasteps(body) => {
                self.check_callee_requires_in_block(session, body, vars, caller_name, errors);
            }
            _ => {}
        }
    }

    fn check_callee_requires_in_expr(
        &mut self,
        session: &mut SolverSession,
        expr: &Expr,
        vars: &mut Z3VarMap,
        caller_name: &str,
        errors: &mut Vec<(String, String, crate::span::Span)>,
    ) {
        match expr.unlocated() {
            Expr::Call(callee, call_args) => {
                if let Expr::Ident(name) = callee.unlocated() {
                    // Clone callee data to avoid borrow conflict with self.*
                    let callee_data: Option<(Vec<crate::ast::Param>, Vec<Expr>)> =
                        self.func_defs.get(name).map(|f| {
                            let params = f.params.clone();
                            let requires: Vec<Expr> = f
                                .body
                                .iter()
                                .filter_map(|s| {
                                    if let Stmt::Requires(e, _) = s.unlocated() {
                                        Some(e.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            (params, requires)
                        });
                    if let Some((callee_params, requires_exprs)) = callee_data {
                        for req in &requires_exprs {
                            let substituted = self.substitute_call(
                                req,
                                &callee_params,
                                call_args,
                                &format!("call_{}", name),
                            );
                            // 0.35.34 (H1): the walker was fail-OPEN — a
                            // precondition that could not be encoded
                            // (expr_to_z3_bool None: calls, strings, ...) was
                            // silently skipped, and a solver timeout (Unknown)
                            // was treated as satisfied. The caller then
                            // verified Proven while the runtime traps E0801
                            // at the call site. fail-closed: both become
                            // explicit "not verified" errors.
                            let z3_req = match expr::expr_to_z3_bool(&substituted, vars) {
                                Some(z) => z,
                                None => {
                                    errors.push((
                                        caller_name.to_string(),
                                        format!(
                                            "call to '{}' has a precondition that cannot be encoded for verification — not verified",
                                            name
                                        ),
                                        expr.meta()
                                            .map(|meta| meta.span)
                                            .or_else(|| {
                                                self.func_defs
                                                    .get(caller_name)
                                                    .map(|caller| caller.meta.span)
                                            })
                                            .unwrap_or(Span::UNKNOWN),
                                    ));
                                    return;
                                }
                            };
                            let (result, _) = session.check_scope(z3_req.not());
                            match result {
                                z3::SatResult::Sat => {
                                    errors.push((
                                        caller_name.to_string(),
                                        format!("call to '{}' may violate precondition", name),
                                        expr.meta()
                                            .map(|meta| meta.span)
                                            .or_else(|| {
                                                self.func_defs
                                                    .get(caller_name)
                                                    .map(|caller| caller.meta.span)
                                            })
                                            .unwrap_or(Span::UNKNOWN),
                                    ));
                                    return;
                                }
                                // Unknown = solver timeout/incomplete: the
                                // precondition could NOT be discharged, so
                                // the call must not verify (fail-closed).
                                z3::SatResult::Unknown => {
                                    errors.push((
                                        caller_name.to_string(),
                                        format!(
                                            "call to '{}': precondition satisfaction could not be decided (solver timeout) — not verified",
                                            name
                                        ),
                                        expr.meta()
                                            .map(|meta| meta.span)
                                            .or_else(|| {
                                                self.func_defs
                                                    .get(caller_name)
                                                    .map(|caller| caller.meta.span)
                                            })
                                            .unwrap_or(Span::UNKNOWN),
                                    ));
                                    return;
                                }
                                z3::SatResult::Unsat => {}
                            }
                        }
                    }
                }
                for arg in call_args {
                    self.check_callee_requires_in_expr(session, arg, vars, caller_name, errors);
                }
            }
            Expr::Binary(_, lhs, rhs) => {
                self.check_callee_requires_in_expr(session, lhs, vars, caller_name, errors);
                self.check_callee_requires_in_expr(session, rhs, vars, caller_name, errors);
            }
            Expr::Unary(_, inner) => {
                self.check_callee_requires_in_expr(session, inner, vars, caller_name, errors);
            }
            Expr::Field(obj, _) => {
                self.check_callee_requires_in_expr(session, obj, vars, caller_name, errors);
            }
            // H-25: index positions execute too (`arr[danger(i)] = 1`,
            // `xs[danger(i)]`).
            Expr::Index(obj, index) => {
                self.check_callee_requires_in_expr(session, obj, vars, caller_name, errors);
                self.check_callee_requires_in_expr(session, index, vars, caller_name, errors);
            }
            Expr::TupleIndex(obj, _) => {
                self.check_callee_requires_in_expr(session, obj, vars, caller_name, errors);
            }
            _ => {}
        }
    }

    /// Build a mapping from let-variable names to their init expressions.
    /// Used to expand `let y = double(x); y` into `double(x)` so that the
    /// verifier can see the function call in the tail expression.
    fn build_let_subst(&self, stmts: &[Stmt]) -> HashMap<String, Expr> {
        let mut subst = HashMap::new();
        Self::build_let_subst_in_block(stmts, &mut subst);
        subst
    }

    fn build_let_subst_in_block(stmts: &[Stmt], subst: &mut HashMap<String, Expr>) {
        for stmt in stmts {
            match stmt.unlocated() {
                Stmt::Let {
                    pat:
                        Pattern {
                            kind: PatternKind::Variable(name),
                            ..
                        },
                    init: Some(init),
                    ..
                } => {
                    let init_expr: &Expr = init;
                    subst.insert(name.clone(), init_expr.clone());
                }
                Stmt::Let { .. } => {}
                Stmt::Block(body)
                | Stmt::Arena(body)
                | Stmt::Unsafe(body)
                | Stmt::Parasteps(body)
                | Stmt::OnFailure(body)
                | Stmt::Loop(body) => {
                    Self::build_let_subst_in_block(body, subst);
                }
                Stmt::If { then_, else_, .. } => {
                    Self::build_let_subst_in_block(then_, subst);
                    if let Some(else_block) = else_ {
                        Self::build_let_subst_in_block(else_block, subst);
                    }
                }
                Stmt::While { body, .. } | Stmt::WhileLet { body, .. } | Stmt::For { body, .. } => {
                    Self::build_let_subst_in_block(body, subst);
                }
                Stmt::Expr(e) => {
                    Self::build_let_subst_in_expr(e, subst);
                }
                Stmt::Assign { target, value } => {
                    // V-C2: simple `name = expr` updates the substitution so
                    // later uses of `name` see the assigned value, not the
                    // original let-init (flat store model, no SSA).
                    if let Expr::Ident(name) = target.unlocated() {
                        let value_expr: &Expr = value;
                        subst.insert(name.clone(), value_expr.clone());
                    }
                    Self::build_let_subst_in_expr(target, subst);
                    Self::build_let_subst_in_expr(value, subst);
                }
                Stmt::Return(Some(e)) | Stmt::Break(Some(e)) => {
                    Self::build_let_subst_in_expr(e, subst);
                }
                Stmt::SharedLet { init, .. } => {
                    Self::build_let_subst_in_expr(init, subst);
                }
                _ => {}
            }
        }
    }

    fn build_let_subst_in_expr(expr: &Expr, subst: &mut HashMap<String, Expr>) {
        match expr.unlocated() {
            Expr::Binary(_, lhs, rhs) => {
                Self::build_let_subst_in_expr(lhs, subst);
                Self::build_let_subst_in_expr(rhs, subst);
            }
            Expr::Unary(_, inner) => Self::build_let_subst_in_expr(inner, subst),
            Expr::If { cond, then_, else_ } => {
                Self::build_let_subst_in_expr(cond, subst);
                Self::build_let_subst_in_block(then_, subst);
                if let Some(e) = else_ {
                    Self::build_let_subst_in_block(e, subst);
                }
            }
            Expr::Block(stmts) => Self::build_let_subst_in_block(stmts, subst),
            Expr::Match(inner, arms) => {
                Self::build_let_subst_in_expr(inner, subst);
                for arm in arms {
                    Self::build_let_subst_in_expr(&arm.body, subst);
                }
            }
            Expr::Call(callee, args) => {
                Self::build_let_subst_in_expr(callee, subst);
                for a in args {
                    Self::build_let_subst_in_expr(a, subst);
                }
            }
            _ => {}
        }
    }

    /// Recursively expand let-variables in an expression using the substitution map.
    fn expand_lets_in_expr(expr: &Expr, subst: &HashMap<String, Expr>) -> Expr {
        let mut expanding: Vec<String> = Vec::new();
        Self::expand_lets_in_expr_guarded(expr, subst, &mut expanding)
    }

    /// C-7 family (full-audit-2026-08-05-0656 §1): a shadowing binding
    /// `let x = x + 1` makes the flat let-substitution SELF-REFERENTIAL
    /// (`x → x + 1`, whose RHS mentions `x` again). Unrestricted expansion
    /// recurses forever and overflows the stack — a user-source crash of
    /// `mimi verify` (VERIFIED: stack-overflow abort on the 90ac9bdc binary).
    /// The guard stops re-expanding a name already in flight and keeps the
    /// (still name-flat) substitution conservative instead of cyclic.
    fn expand_lets_in_expr_guarded(
        expr: &Expr,
        subst: &HashMap<String, Expr>,
        expanding: &mut Vec<String>,
    ) -> Expr {
        // When a tail identifier expands to its let initializer, the call
        // expression's own source location is authoritative. Do not overwrite
        // it with the identifier's later use-site metadata.
        if let Expr::Ident(name) = expr.unlocated() {
            if let Some(replacement) = subst.get(name) {
                if !expanding.iter().any(|n| n == name) {
                    expanding.push(name.clone());
                    let expanded = Self::expand_lets_in_expr_guarded(replacement, subst, expanding);
                    expanding.pop();
                    return expanded;
                }
                // Self-referential cycle: keep the identifier unexpanded.
                let kept = expr.unlocated().clone();
                return match expr.meta() {
                    Some(meta) => kept.with_meta(meta),
                    None => kept,
                };
            }
        }
        let transformed = match expr.unlocated() {
            Expr::Ident(_) => expr.unlocated().clone(),
            Expr::Binary(op, lhs, rhs) => Expr::Binary(
                *op,
                Box::new(Self::expand_lets_in_expr_guarded(lhs, subst, expanding)),
                Box::new(Self::expand_lets_in_expr_guarded(rhs, subst, expanding)),
            ),
            Expr::Unary(op, inner) => Expr::Unary(
                *op,
                Box::new(Self::expand_lets_in_expr_guarded(inner, subst, expanding)),
            ),
            Expr::Call(callee, args) => Expr::Call(
                Box::new(Self::expand_lets_in_expr_guarded(callee, subst, expanding)),
                args.iter()
                    .map(|a| Self::expand_lets_in_expr_guarded(a, subst, expanding))
                    .collect(),
            ),
            Expr::Field(obj, name) => Expr::Field(
                Box::new(Self::expand_lets_in_expr_guarded(obj, subst, expanding)),
                name.clone(),
            ),
            Expr::Old(inner) => Expr::Old(Box::new(Self::expand_lets_in_expr_guarded(
                inner, subst, expanding,
            ))),
            Expr::Block(block) => Expr::Block(
                block
                    .iter()
                    .map(|s| Self::expand_lets_in_stmt(s, subst))
                    .collect(),
            ),
            Expr::If { cond, then_, else_ } => Expr::If {
                cond: Box::new(Self::expand_lets_in_expr(cond, subst)),
                then_: then_
                    .iter()
                    .map(|s| Self::expand_lets_in_stmt(s, subst))
                    .collect(),
                else_: else_.as_ref().map(|b| {
                    b.iter()
                        .map(|s| Self::expand_lets_in_stmt(s, subst))
                        .collect()
                }),
            },
            Expr::Match(scrutinee, arms) => Expr::Match(
                Box::new(Self::expand_lets_in_expr(scrutinee, subst)),
                arms.iter()
                    .map(|arm| crate::ast::MatchArm {
                        meta: arm.meta,
                        pat: arm.pat.clone(),
                        guard: arm
                            .guard
                            .as_ref()
                            .map(|g| Self::expand_lets_in_expr(g, subst)),
                        body: Self::expand_lets_in_expr(&arm.body, subst),
                    })
                    .collect(),
            ),
            Expr::Spawn(inner) => Expr::Spawn(Box::new(Self::expand_lets_in_expr(inner, subst))),
            Expr::Await(inner) => Expr::Await(Box::new(Self::expand_lets_in_expr(inner, subst))),
            Expr::Lambda { params, ret, body } => Expr::Lambda {
                params: params.clone(),
                ret: ret.clone(),
                body: body
                    .iter()
                    .map(|s| Self::expand_lets_in_stmt(s, subst))
                    .collect(),
            },
            Expr::Comprehension {
                expr,
                var,
                iter,
                guard,
            } => Expr::Comprehension {
                expr: Box::new(Self::expand_lets_in_expr(expr, subst)),
                var: var.clone(),
                iter: Box::new(Self::expand_lets_in_expr(iter, subst)),
                guard: guard
                    .as_ref()
                    .map(|g| Box::new(Self::expand_lets_in_expr(g, subst))),
            },
            _ => expr.unlocated().clone(),
        };
        match expr.meta() {
            Some(meta) => transformed.with_meta(meta),
            None => transformed,
        }
    }

    fn expand_lets_in_stmt(stmt: &Stmt, subst: &HashMap<String, Expr>) -> Stmt {
        let transformed = match stmt.unlocated() {
            Stmt::Expr(e) => Stmt::Expr(Self::expand_lets_in_expr(e, subst)),
            Stmt::Return(e) => {
                Stmt::Return(e.as_ref().map(|e| Self::expand_lets_in_expr(e, subst)))
            }
            _ => stmt.unlocated().clone(),
        };
        match stmt.meta() {
            Some(meta) => transformed.with_meta(meta),
            None => transformed,
        }
    }

    /// Substitute `result` → `call_key` and formal param names → actual arg
    /// expressions in an ensures expression. Returns the substituted expression.
    fn substitute_call(
        &self,
        ensures: &Expr,
        params: &[Param],
        call_args: &[Expr],
        call_key: &str,
    ) -> Expr {
        // Simple recursive substitution. For `result`, replace with a fresh
        // Ident that matches the Z3 variable naming from expr::call_var_key.
        // For param names, replace with the actual call argument expressions.
        match ensures.unlocated() {
            Expr::Ident(name) if name == "result" => Expr::Ident(call_key.to_string()),
            Expr::Ident(name) => {
                if let Some(idx) = params.iter().position(|p| p.name == *name) {
                    if idx < call_args.len() {
                        return call_args[idx].clone();
                    }
                }
                ensures.clone()
            }
            Expr::Binary(op, lhs, rhs) => Expr::Binary(
                *op,
                Box::new(self.substitute_call(lhs, params, call_args, call_key)),
                Box::new(self.substitute_call(rhs, params, call_args, call_key)),
            ),
            Expr::Unary(op, inner) => Expr::Unary(
                *op,
                Box::new(self.substitute_call(inner, params, call_args, call_key)),
            ),
            Expr::Field(obj, name) => Expr::Field(
                Box::new(self.substitute_call(obj, params, call_args, call_key)),
                name.clone(),
            ),
            Expr::Old(inner) => Expr::Old(Box::new(
                self.substitute_call(inner, params, call_args, call_key),
            )),
            Expr::Literal(l) => Expr::Literal(l.clone()),
            _ => ensures.clone(),
        }
    }

    /// Collect simple assign targets (`name = …`) that appear inside any loop body.
    /// Used by V-H1 conservative preserve: if an invariant free var is assigned
    /// in a loop, we cannot claim Verified without a body⇒inv' proof.
    fn collect_loop_assigned_idents(stmts: &[Stmt], out: &mut Vec<String>) {
        for stmt in stmts {
            match stmt.unlocated() {
                Stmt::While { body, .. }
                | Stmt::WhileLet { body, .. }
                | Stmt::For { body, .. }
                | Stmt::Loop(body) => {
                    Self::collect_assigned_idents_in_block(body, out);
                }
                Stmt::If { then_, else_, .. } => {
                    Self::collect_loop_assigned_idents(then_, out);
                    if let Some(e) = else_ {
                        Self::collect_loop_assigned_idents(e, out);
                    }
                }
                Stmt::Block(body)
                | Stmt::Arena(body)
                | Stmt::Unsafe(body)
                | Stmt::Parasteps(body)
                | Stmt::OnFailure(body) => {
                    Self::collect_loop_assigned_idents(body, out);
                }
                _ => {}
            }
        }
    }

    fn collect_assigned_idents_in_block(stmts: &[Stmt], out: &mut Vec<String>) {
        for stmt in stmts {
            match stmt.unlocated() {
                Stmt::Assign { target, .. } => {
                    if let Expr::Ident(name) = target.unlocated() {
                        if !out.contains(name) {
                            out.push(name.clone());
                        }
                    }
                }
                Stmt::If { then_, else_, .. } => {
                    Self::collect_assigned_idents_in_block(then_, out);
                    if let Some(e) = else_ {
                        Self::collect_assigned_idents_in_block(e, out);
                    }
                }
                Stmt::While { body, .. }
                | Stmt::WhileLet { body, .. }
                | Stmt::For { body, .. }
                | Stmt::Loop(body)
                | Stmt::Block(body)
                | Stmt::Arena(body)
                | Stmt::Unsafe(body)
                | Stmt::Parasteps(body)
                | Stmt::OnFailure(body) => {
                    Self::collect_assigned_idents_in_block(body, out);
                }
                _ => {}
            }
        }
    }
}
