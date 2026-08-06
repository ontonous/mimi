use super::*;
use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::verifier::expr;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use z3::ast::String as Z3String;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};
use z3::SatResult;

impl VerifierCtx {
    pub fn verify_ffi_call_sites(
        &mut self,
        session: &mut SolverSession,
        file: &File,
    ) -> Vec<VerificationResult> {
        let mut externs: HashMap<String, ExternFunc> = HashMap::new();
        Self::collect_externs(&file.items, &mut externs);
        self.verify_ffi_call_sites_with_externs(session, file, &externs)
    }

    pub fn verify_ffi_call_sites_with_externs(
        &mut self,
        session: &mut SolverSession,
        file: &File,
        externs: &HashMap<String, ExternFunc>,
    ) -> Vec<VerificationResult> {
        let mut results = Vec::new();
        let extern_names: HashSet<String> = externs.keys().cloned().collect();
        self.verify_ffi_items_with_externs(
            session,
            &file.items,
            externs,
            &extern_names,
            &mut results,
        );
        results
    }

    /// Wave-2 (wave1-review §5.8): call-site discovery descends into
    /// `Item::Module` — the Wave-1 walker made If/While/For conditions,
    /// Match, Defer, etc. exhaustive INSIDE a function body, but functions
    /// nested in modules were never visited at all, so `--verify-ffi` stayed
    /// blind to every extern call they contain.
    fn verify_ffi_items_with_externs(
        &mut self,
        session: &mut SolverSession,
        items: &[Item],
        externs: &HashMap<String, ExternFunc>,
        extern_names: &HashSet<String>,
        results: &mut Vec<VerificationResult>,
    ) {
        for item in items {
            match item {
                Item::Func(func) => {
                    if func.body.is_empty() {
                        continue;
                    }
                    let calls = Self::find_extern_calls_in_func(func, extern_names);
                    if calls.is_empty() {
                        continue;
                    }
                    session.push();
                    let mut vars = self.setup_ffi_func_vars(session, func);
                    self.assert_func_requires(session, func, &mut vars);

                    for (extern_name, args, call_span) in &calls {
                        if let Some(extern_func) = externs.get(extern_name.as_str()) {
                            let result = self.check_extern_call(
                                session,
                                &func.name,
                                extern_func,
                                args,
                                &mut vars,
                                *call_span,
                            );
                            results.push(result);
                        }
                    }
                    session.pop();
                }
                Item::Module(m) => {
                    self.verify_ffi_items_with_externs(
                        session,
                        &m.items,
                        externs,
                        extern_names,
                        results,
                    );
                }
                _ => {}
            }
        }
    }

    fn collect_externs(items: &[Item], externs: &mut HashMap<String, ExternFunc>) {
        for item in items {
            match item {
                Item::ExternBlock(block) => {
                    for func in &block.funcs {
                        externs.insert(func.name.clone(), func.clone());
                    }
                }
                Item::Module(m) => Self::collect_externs(&m.items, externs),
                _ => {}
            }
        }
    }

    fn find_extern_calls_in_func(
        func: &FuncDef,
        extern_names: &HashSet<String>,
    ) -> Vec<(String, Vec<Expr>, Span)> {
        let mut calls = Vec::new();
        Self::find_extern_calls_in_block(&func.body, extern_names, &mut calls);
        for (_, _, span) in &mut calls {
            if span.start_line == 0 || span.start_col == 0 {
                *span = func.meta.span;
            }
        }
        calls
    }

    /// AU-V2 (full-audit-2026-08-05 §11, HIGH): unified recursive walker for
    /// extern call-site discovery.
    ///
    /// The old walker matched `stmt.unlocated()` with a `_ => {}` catch-all
    /// and silently skipped every unlisted form — If/While conditions, For
    /// iterables, IfLet/WhileLet inits, Match scrutinees, Alloc bodies, Defer
    /// bodies, IeeeFloat/OnFailure blocks, Pinned exprs, Assign targets,
    /// Drop/Break values, contract clauses. An extern call in
    /// `while dangerous(ptr) { ... }` was never checked — precisely what
    /// `--verify-ffi` exists to catch.
    ///
    /// The walkers below are exhaustive over every Stmt/Expr variant with NO
    /// catch-all arm: adding a new AST variant is a compile error here
    /// instead of a silent verification hole. Fail-closed by construction.
    fn find_extern_calls_in_block(
        block: &[Stmt],
        extern_names: &HashSet<String>,
        calls: &mut Vec<(String, Vec<Expr>, Span)>,
    ) {
        for stmt in block {
            Self::find_extern_calls_in_stmt(stmt, extern_names, calls);
        }
    }

    fn find_extern_calls_in_stmt(
        stmt: &Stmt,
        extern_names: &HashSet<String>,
        calls: &mut Vec<(String, Vec<Expr>, Span)>,
    ) {
        match stmt {
            Stmt::Located { stmt: inner, .. } => {
                Self::find_extern_calls_in_stmt(inner, extern_names, calls);
            }
            Stmt::Expr(e) | Stmt::Return(Some(e)) | Stmt::Break(Some(e)) | Stmt::Drop(e) => {
                Self::find_extern_calls_in_expr(e, extern_names, calls);
            }
            Stmt::Return(None) | Stmt::Break(None) | Stmt::Continue | Stmt::Ellipsis => {}
            Stmt::Let {
                init: Some(init), ..
            } => {
                Self::find_extern_calls_in_expr(init, extern_names, calls);
            }
            Stmt::Let { init: None, .. } => {}
            Stmt::SharedLet { init, .. } => {
                Self::find_extern_calls_in_expr(init, extern_names, calls);
            }
            Stmt::If { cond, then_, else_ } => {
                // AU-V2: the condition is an expression position too.
                Self::find_extern_calls_in_expr(cond, extern_names, calls);
                Self::find_extern_calls_in_block(then_, extern_names, calls);
                if let Some(else_block) = else_ {
                    Self::find_extern_calls_in_block(else_block, extern_names, calls);
                }
            }
            Stmt::IfLet {
                init, then_, else_, ..
            } => {
                Self::find_extern_calls_in_expr(init, extern_names, calls);
                Self::find_extern_calls_in_block(then_, extern_names, calls);
                if let Some(else_block) = else_ {
                    Self::find_extern_calls_in_block(else_block, extern_names, calls);
                }
            }
            Stmt::While { cond, body } => {
                // AU-V2: `while dangerous(ptr) { ... }` — the condition was
                // the headline hole.
                Self::find_extern_calls_in_expr(cond, extern_names, calls);
                Self::find_extern_calls_in_block(body, extern_names, calls);
            }
            Stmt::WhileLet { init, body, .. } => {
                Self::find_extern_calls_in_expr(init, extern_names, calls);
                Self::find_extern_calls_in_block(body, extern_names, calls);
            }
            Stmt::For { iterable, body, .. } => {
                // AU-V2: the iterable was skipped (body was scanned).
                Self::find_extern_calls_in_expr(iterable, extern_names, calls);
                Self::find_extern_calls_in_block(body, extern_names, calls);
            }
            Stmt::Loop(body)
            | Stmt::Block(body)
            | Stmt::Arena(body)
            | Stmt::Unsafe(body)
            | Stmt::IeeeFloat(body)
            | Stmt::Defer(body)
            | Stmt::OnFailure(body)
            | Stmt::Parasteps(body) => {
                Self::find_extern_calls_in_block(body, extern_names, calls);
            }
            Stmt::Alloc { body, .. } => {
                // AU-V2: alloc block initializers/statements.
                Self::find_extern_calls_in_block(body, extern_names, calls);
            }
            Stmt::Pinned { expr, body, .. } => {
                Self::find_extern_calls_in_expr(expr, extern_names, calls);
                Self::find_extern_calls_in_block(body, extern_names, calls);
            }
            Stmt::Assign { target, value } => {
                // AU-V2: the target too — `arr[dangerous(ptr)] = 1`.
                Self::find_extern_calls_in_expr(target, extern_names, calls);
                Self::find_extern_calls_in_expr(value, extern_names, calls);
            }
            Stmt::Requires(expr, _) | Stmt::Ensures(expr, _) | Stmt::Invariant(expr, _) => {
                // Contract clauses execute (interpreter contract checking);
                // an extern call inside them is a call site like any other.
                Self::find_extern_calls_in_expr(expr, extern_names, calls);
            }
            Stmt::Math(exprs) => {
                for expr in exprs {
                    Self::find_extern_calls_in_expr(expr, extern_names, calls);
                }
            }
            Stmt::Func(nested) => {
                // Nested function definitions are reachable from the
                // enclosing function. Scanned fail-closed: arguments that
                // reference the nested function's own parameters are unknown
                // to the caller's vars map and degrade to SolverUnknown in
                // `check_extern_call` rather than being silently skipped.
                Self::find_extern_calls_in_block(&nested.body, extern_names, calls);
            }
            // No expression positions: Desc/Rule are intent text, MmsBlock is
            // a super-comment skipped by all tool paths (AGENTS.md §10).
            Stmt::Desc(_, _) | Stmt::Rule(_, _) | Stmt::MmsBlock { .. } => {}
        }
    }

    fn find_extern_calls_in_expr(
        expr: &Expr,
        extern_names: &HashSet<String>,
        calls: &mut Vec<(String, Vec<Expr>, Span)>,
    ) {
        // Strip Located wrappers once, remembering the outermost span so a
        // discovered call reports its source location.
        let mut current = expr;
        let mut span: Option<Span> = None;
        while let Expr::Located { meta, expr: inner } = current {
            if span.is_none() {
                span = Some(meta.span);
            }
            current = inner;
        }
        let call_span = span.unwrap_or(Span::UNKNOWN);

        match current {
            // Defensive: unreachable after the strip loop, but kept explicit
            // so the match stays exhaustive with no catch-all.
            Expr::Located { expr: inner, .. } => {
                Self::find_extern_calls_in_expr(inner, extern_names, calls);
            }
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.unlocated() {
                    if extern_names.contains(name.as_str()) {
                        calls.push((name.clone(), args.clone(), call_span));
                        // AU-V2: do NOT stop here — the old walker returned
                        // early, so `dangerous(other_dangerous(ptr))` never
                        // discovered the nested call. Keep walking callee and
                        // arguments below.
                    }
                }
                Self::find_extern_calls_in_expr(callee, extern_names, calls);
                for arg in args {
                    Self::find_extern_calls_in_expr(arg, extern_names, calls);
                }
            }
            Expr::Binary(_, lhs, rhs) => {
                Self::find_extern_calls_in_expr(lhs, extern_names, calls);
                Self::find_extern_calls_in_expr(rhs, extern_names, calls);
            }
            Expr::Unary(_, inner)
            | Expr::Field(inner, _)
            | Expr::Try(inner)
            | Expr::OptionalChain(inner, _)
            | Expr::Spawn(inner)
            | Expr::Await(inner)
            | Expr::Old(inner)
            | Expr::TypeOf(inner)
            | Expr::QuoteInterpolate(inner)
            | Expr::TupleIndex(inner, _)
            | Expr::NamedArg(_, inner)
            | Expr::Cast(inner, _) => {
                Self::find_extern_calls_in_expr(inner, extern_names, calls);
            }
            Expr::Index(inner, index) => {
                // AU-V2: the index expression too — `arr[dangerous(ptr)]`.
                Self::find_extern_calls_in_expr(inner, extern_names, calls);
                Self::find_extern_calls_in_expr(index, extern_names, calls);
            }
            Expr::SliceExpr { target, start, end } => {
                Self::find_extern_calls_in_expr(target, extern_names, calls);
                if let Some(start) = start {
                    Self::find_extern_calls_in_expr(start, extern_names, calls);
                }
                if let Some(end) = end {
                    Self::find_extern_calls_in_expr(end, extern_names, calls);
                }
            }
            Expr::Tuple(items)
            | Expr::List(items)
            | Expr::SetLiteral(items)
            | Expr::Turbofish(_, _, items) => {
                for item in items {
                    Self::find_extern_calls_in_expr(item, extern_names, calls);
                }
            }
            Expr::MapLiteral { entries } => {
                for (key, value) in entries {
                    Self::find_extern_calls_in_expr(key, extern_names, calls);
                    Self::find_extern_calls_in_expr(value, extern_names, calls);
                }
            }
            Expr::Comprehension {
                expr, iter, guard, ..
            } => {
                Self::find_extern_calls_in_expr(expr, extern_names, calls);
                Self::find_extern_calls_in_expr(iter, extern_names, calls);
                if let Some(guard) = guard {
                    Self::find_extern_calls_in_expr(guard, extern_names, calls);
                }
            }
            Expr::Match(scrutinee, arms) => {
                // AU-V2: scrutinee, arm guards and arm bodies were skipped.
                Self::find_extern_calls_in_expr(scrutinee, extern_names, calls);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        Self::find_extern_calls_in_expr(guard, extern_names, calls);
                    }
                    Self::find_extern_calls_in_expr(&arm.body, extern_names, calls);
                }
            }
            Expr::Record { fields, .. } => {
                for field in fields {
                    Self::find_extern_calls_in_expr(&field.value, extern_names, calls);
                }
            }
            Expr::If { cond, then_, else_ } => {
                Self::find_extern_calls_in_expr(cond, extern_names, calls);
                Self::find_extern_calls_in_block(then_, extern_names, calls);
                if let Some(else_block) = else_ {
                    Self::find_extern_calls_in_block(else_block, extern_names, calls);
                }
            }
            Expr::Lambda { body, .. } => {
                Self::find_extern_calls_in_block(body, extern_names, calls);
            }
            Expr::Block(block) | Expr::Arena(block) | Expr::Comptime(block) => {
                Self::find_extern_calls_in_block(block, extern_names, calls);
            }
            Expr::Literal(lit) => {
                // f-string interpolations are expression positions.
                if let Lit::FString(parts) = lit {
                    for part in parts {
                        if let FStringPart::Interp(interp) = part {
                            Self::find_extern_calls_in_expr(interp, extern_names, calls);
                        }
                    }
                }
            }
            // No expression positions: Ident resolves at the call check
            // above; TypeInfo carries only a Type. Deliberately NOT scanning
            // `Expr::Quote` bodies: quote! is an inert compile-time AST
            // template (AGENTS.md §9 — codegen never compiles comptime;
            // spliced code is runtime-generated and unreachable to any static
            // walk). Flagging template text that may never materialize would
            // produce spurious obligations rather than fail-closed coverage.
            Expr::Ident(_) | Expr::TypeInfo(_) | Expr::Quote(_) => {}
        }
    }

    fn setup_ffi_func_vars(&mut self, _session: &mut SolverSession, func: &FuncDef) -> Z3VarMap {
        let mut vars = Z3VarMap::new();
        for p in &func.params {
            if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "f64") {
                vars.insert_real(p.name.as_str(), Z3Real::new_const(p.name.as_str()));
            } else if matches!(p.ty.unlocated(), Type::Name(n, _) if n == "string") {
                vars.insert_int(p.name.as_str(), Z3Int::new_const(p.name.as_str()));
                vars.insert_string_nonempty(
                    p.name.as_str(),
                    Z3Bool::new_const(format!("{}.ne", p.name)),
                );
                vars.insert_string_len(
                    p.name.as_str(),
                    Z3Int::new_const(format!("{}.len", p.name)),
                );
                vars.insert_string_var(p.name.as_str(), Z3String::new_const(p.name.as_str()));
            } else {
                vars.insert_int(p.name.as_str(), Z3Int::new_const(p.name.as_str()));
            }
        }
        vars
    }

    fn assert_func_requires(
        &mut self,
        session: &mut SolverSession,
        func: &FuncDef,
        vars: &mut Z3VarMap,
    ) {
        for stmt in &func.body {
            if let Stmt::Requires(expr, _) = stmt.unlocated() {
                match expr::expr_to_z3_bool(expr, vars) {
                    Some(z3_bool) => session.assert(&z3_bool),
                    None => {
                        // HIGH fix: previously silently dropped unencodable
                        // requires. Now log a warning so users know their
                        // precondition was not verified.
                        eprintln!(
                            "[mimi verify] WARN: could not encode requires in function '{}' — precondition not asserted",
                            func.name
                        );
                    }
                }
            }
        }
    }

    fn check_extern_call(
        &mut self,
        session: &mut SolverSession,
        caller_name: &str,
        extern_func: &ExternFunc,
        args: &[Expr],
        vars: &mut Z3VarMap,
        caller_span: Span,
    ) -> VerificationResult {
        let start = Instant::now();
        let func_name = format!("{} calls {}", caller_name, extern_func.name);

        let requires = match &extern_func.requires {
            Some(r) => r,
            None => {
                return VerificationResult {
                    func_name,
                    status: VerifStatus::Verified,
                    message: "extern has no precondition".into(),
                    diagnostic: None,
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count: 0,
                    artifact: None,
                    trusted_subset_domain: None,
                };
            }
        };

        let substituted = substitute_args(requires, &extern_func.params, args);

        let z3_requires = match expr::expr_to_z3_bool(&substituted, vars) {
            Some(z) => z,
            None => {
                return VerificationResult {
                    func_name,
                    status: VerifStatus::SolverUnknown,
                    message: "could not encode precondition in Z3".into(),
                    diagnostic: None,
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count: 1,
                    artifact: None,
                    trusted_subset_domain: None,
                };
            }
        };

        let (result, _model) = session.check_scope(z3_requires.not());
        let constraint_count = 1;

        match result {
            SatResult::Unsat => VerificationResult {
                func_name,
                status: VerifStatus::Verified,
                message: "precondition always satisfied".into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
                artifact: None,
                trusted_subset_domain: None,
            },
            SatResult::Sat => {
                let diag = Diagnostic::error(
                    format!(
                        "call to extern '{}' may violate precondition: {:?}",
                        extern_func.name, requires,
                    ),
                    caller_span,
                )
                .with_help(format!(
                    "ensure all preconditions of '{}' are satisfied at call site",
                    extern_func.name,
                ));
                VerificationResult {
                    func_name,
                    status: VerifStatus::Failed,
                    message: "precondition may be violated".into(),
                    diagnostic: Some(diag),
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count,
                    artifact: None,
                    trusted_subset_domain: None,
                }
            }
            SatResult::Unknown => VerificationResult {
                func_name,
                status: VerifStatus::SolverUnknown,
                message: "precondition satisfiability unknown".into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
                artifact: None,
                trusted_subset_domain: None,
            },
        }
    }
}

fn substitute_args(expr: &Expr, params: &[ExternParam], args: &[Expr]) -> Expr {
    if params.len() != args.len() {
        // Mismatch: return false to fail closed (safe side) rather than
        // silently using un-substituted expressions (which could pass a
        // constraint that was meant to refer to different variables).
        return Expr::Literal(Lit::Bool(false));
    }
    match expr.unlocated() {
        Expr::Ident(name) => {
            if let Some(idx) = params.iter().position(|p| p.name == *name) {
                if idx < args.len() {
                    return args[idx].clone();
                }
            }
            Expr::Ident(name.clone())
        }
        Expr::Binary(op, lhs, rhs) => Expr::Binary(
            *op,
            Box::new(substitute_args(lhs, params, args)),
            Box::new(substitute_args(rhs, params, args)),
        ),
        Expr::Unary(op, inner) => Expr::Unary(*op, Box::new(substitute_args(inner, params, args))),
        Expr::Call(callee, callee_args) => Expr::Call(
            Box::new(substitute_args(callee, params, args)),
            callee_args
                .iter()
                .map(|a| substitute_args(a, params, args))
                .collect(),
        ),
        Expr::Field(inner, name) => {
            Expr::Field(Box::new(substitute_args(inner, params, args)), name.clone())
        }
        Expr::Index(target, index) => Expr::Index(
            Box::new(substitute_args(target, params, args)),
            Box::new(substitute_args(index, params, args)),
        ),
        Expr::If { cond, then_, else_ } => Expr::If {
            cond: Box::new(substitute_args(cond, params, args)),
            then_: then_
                .iter()
                .map(|s| substitute_args_in_stmt(s, params, args))
                .collect(),
            else_: else_.as_ref().map(|b| {
                b.iter()
                    .map(|s| substitute_args_in_stmt(s, params, args))
                    .collect()
            }),
        },
        Expr::Old(inner) => Expr::Old(Box::new(substitute_args(inner, params, args))),
        Expr::Tuple(items) => Expr::Tuple(
            items
                .iter()
                .map(|i| substitute_args(i, params, args))
                .collect(),
        ),
        Expr::List(items) => Expr::List(
            items
                .iter()
                .map(|i| substitute_args(i, params, args))
                .collect(),
        ),
        Expr::Block(block) => Expr::Block(
            block
                .iter()
                .map(|s| substitute_args_in_stmt(s, params, args))
                .collect(),
        ),
        Expr::Literal(_) => expr.clone(),
        _ => expr.clone(),
    }
}

fn substitute_args_in_stmt(stmt: &Stmt, params: &[ExternParam], args: &[Expr]) -> Stmt {
    let transformed = match stmt.unlocated() {
        Stmt::Expr(e) => Stmt::Expr(substitute_args(e, params, args)),
        Stmt::Return(e) => Stmt::Return(e.as_ref().map(|e| substitute_args(e, params, args))),
        Stmt::Let {
            pat,
            ty,
            init,
            mut_,
            ref_,
        } => Stmt::Let {
            pat: pat.clone(),
            ty: ty.clone(),
            init: init.as_ref().map(|e| substitute_args(e, params, args)),
            mut_: *mut_,
            ref_: *ref_,
        },
        Stmt::If { cond, then_, else_ } => Stmt::If {
            cond: substitute_args(cond, params, args),
            then_: then_
                .iter()
                .map(|s| substitute_args_in_stmt(s, params, args))
                .collect(),
            else_: else_.as_ref().map(|b| {
                b.iter()
                    .map(|s| substitute_args_in_stmt(s, params, args))
                    .collect()
            }),
        },
        Stmt::Assign { target, value } => Stmt::Assign {
            target: substitute_args(target, params, args),
            value: substitute_args(value, params, args),
        },
        Stmt::While { cond, body } => Stmt::While {
            cond: substitute_args(cond, params, args),
            body: body
                .iter()
                .map(|s| substitute_args_in_stmt(s, params, args))
                .collect(),
        },
        Stmt::WhileLet { pat, init, body } => Stmt::WhileLet {
            pat: pat.clone(),
            init: substitute_args(init, params, args),
            body: body
                .iter()
                .map(|s| substitute_args_in_stmt(s, params, args))
                .collect(),
        },
        Stmt::Loop(block) => Stmt::Loop(
            block
                .iter()
                .map(|s| substitute_args_in_stmt(s, params, args))
                .collect(),
        ),
        Stmt::For {
            var,
            iterable,
            body,
        } => Stmt::For {
            var: var.clone(),
            iterable: substitute_args(iterable, params, args),
            body: body
                .iter()
                .map(|s| substitute_args_in_stmt(s, params, args))
                .collect(),
        },
        Stmt::Block(block) => Stmt::Block(
            block
                .iter()
                .map(|s| substitute_args_in_stmt(s, params, args))
                .collect(),
        ),
        Stmt::Break(e) => Stmt::Break(e.as_ref().map(|e| substitute_args(e, params, args))),
        Stmt::Continue => Stmt::Continue,
        Stmt::Drop(e) => Stmt::Drop(substitute_args(e, params, args)),
        Stmt::Arena(block) => Stmt::Arena(
            block
                .iter()
                .map(|s| substitute_args_in_stmt(s, params, args))
                .collect(),
        ),
        Stmt::Unsafe(block) => Stmt::Unsafe(
            block
                .iter()
                .map(|s| substitute_args_in_stmt(s, params, args))
                .collect(),
        ),
        Stmt::OnFailure(block) => Stmt::OnFailure(
            block
                .iter()
                .map(|s| substitute_args_in_stmt(s, params, args))
                .collect(),
        ),
        Stmt::SharedLet {
            kind,
            name,
            ty,
            init,
        } => Stmt::SharedLet {
            kind: *kind,
            name: name.clone(),
            ty: ty.clone(),
            init: substitute_args(init, params, args),
        },
        _ => stmt.unlocated().clone(),
    };
    match stmt.meta() {
        Some(meta) => transformed.with_meta(meta),
        None => transformed,
    }
}
