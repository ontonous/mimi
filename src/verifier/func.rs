use crate::ast::*;
use crate::contracts;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::verifier::ctx::{Counterexample, VerificationResult, VerifStatus, Z3VarMap};
use crate::verifier::helpers::{
    collect_idents_in_stmt, extract_body_return, format_expr, parse_contract_expr,
};
use std::time::Instant;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};
use z3::SatResult;

impl crate::verifier::Verifier {
    pub(crate) fn verify_items(&mut self, items: &[Item], results: &mut Vec<VerificationResult>) {
        for item in items {
            match item {
                Item::Func(f) => {
                    if !f.body.is_empty() {
                        results.push(self.verify_func(f));
                    }
                }
                Item::Module(m) => self.verify_items(&m.items, results),
                Item::ExternBlock(block) => {
                    for func in &block.funcs {
                        if func.requires.is_some() || func.ensures.is_some() {
                            results.push(self.verify_extern_func(func));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn verify_extern_func(&mut self, func: &ExternFunc) -> VerificationResult {
        let start = Instant::now();
        self.solver.reset();

        let requires_expr = func.requires.as_ref();
        let ensures_expr = func.ensures.as_ref();

        let returns_real = func
            .ret
            .as_ref()
            .map_or(false, |t| matches!(t, Type::Name(n, _) if n == "f64"));

        let mut vars = Z3VarMap::new();

        for p in &func.params {
            if matches!(&p.ty, Type::Name(n, _) if n == "f64") {
                vars.insert_real(p.name.as_str(), Z3Real::new_const(p.name.as_str()));
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
            if let Some(z3_bool) = self.expr_to_z3_bool(req, &mut vars) {
                self.solver.assert(&z3_bool);
            }
        }

        match self.check_safe() {
            SatResult::Unsat => VerificationResult {
                func_name: format!("extern {}", func.name),
                status: VerifStatus::Failed,
                message: "preconditions are unsatisfiable".into(),
                diagnostic: Some(
                    // ExternFunc lacks a pos field; add one to AST for proper span propagation
                    Diagnostic::error(
                        format!("extern function '{}' has unsatisfiable requires", func.name),
                        Span::single(0, 0),
                    )
                    .with_help("check that your requires conditions can actually be satisfied"),
                ),
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
            },
            SatResult::Unknown => VerificationResult {
                func_name: format!("extern {}", func.name),
                status: VerifStatus::Unknown,
                message: "precondition satisfiability unknown".into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
            },
            SatResult::Sat => {
                if let Some(ens) = ensures_expr {
                    self.solver.push();
                    if let Some(z3_not_ens) = self.expr_to_z3_bool(ens, &mut vars).map(|b| b.not()) {
                        self.solver.assert(&z3_not_ens);
                        match self.check_safe() {
                            SatResult::Unsat => {
                                self.solver.pop(1);
                                VerificationResult {
                                    func_name: format!("extern {}", func.name),
                                    status: VerifStatus::Verified,
                                    message: "postconditions always satisfied given preconditions"
                                        .into(),
                                    diagnostic: None,
                                    duration_us: start.elapsed().as_micros() as u64,
                                    constraint_count,
                                }
                            }
                            SatResult::Sat | SatResult::Unknown => {
                                self.solver.pop(1);
                                VerificationResult {
                                    func_name: format!("extern {}", func.name),
                                    status: VerifStatus::Verified,
                                    message:
                                        "extern contracts are consistent (preconditions do not statically guarantee postconditions; runtime verification required)"
                                            .into(),
                                    diagnostic: None,
                                    duration_us: start.elapsed().as_micros() as u64,
                                    constraint_count,
                                }
                            }
                        }
                    } else {
                        self.solver.pop(1);
                        VerificationResult {
                            func_name: format!("extern {}", func.name),
                            status: VerifStatus::Unknown,
                            message: "could not encode ensures for Z3".into(),
                            diagnostic: None,
                            duration_us: start.elapsed().as_micros() as u64,
                            constraint_count,
                        }
                    }
                } else {
                    VerificationResult {
                        func_name: format!("extern {}", func.name),
                        status: VerifStatus::Verified,
                        message: "preconditions satisfiable".into(),
                        diagnostic: None,
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count,
                    }
                }
            }
        }
    }

    fn verify_func(&mut self, func: &FuncDef) -> VerificationResult {
        let start = Instant::now();

        // Shared parameters use abstract heap encoding:
        // shared identity → opaque Int variable,
        // field accesses → fresh Z3 variables (handled by Expr::Field encoding).
        // This allows verifying scalar-field contracts on shared params.
        self.solver.reset();

        let mut requires_exprs: Vec<Expr> = Vec::new();
        let mut ensures_exprs: Vec<Expr> = Vec::new();
        let mut invariant_exprs: Vec<Expr> = Vec::new();
        let mut math_exprs: Vec<Expr> = Vec::new();
        let mut requires_spans: Vec<Span> = Vec::new();
        let mut ensures_spans: Vec<Span> = Vec::new();
        let mut invariant_spans: Vec<Span> = Vec::new();

        for stmt in &func.body {
            match stmt {
                Stmt::Requires(expr, span) => {
                    requires_exprs.push(expr.clone());
                    requires_spans.push(*span);
                }
                Stmt::Ensures(expr, span) => {
                    ensures_exprs.push(expr.clone());
                    ensures_spans.push(*span);
                }
                Stmt::Invariant(expr, span) => {
                    invariant_exprs.push(expr.clone());
                    invariant_spans.push(*span);
                }
                Stmt::Math(exprs) => math_exprs.extend(exprs.clone()),
                Stmt::MmsBlock {
                    content: text,
                    span: mms_span,
                    ..
                } => {
                    let contract = contracts::extract_contracts(text);
                    for _ in &contract.requires {
                        requires_spans.push(*mms_span);
                    }
                    for req_text in &contract.requires {
                        if let Ok(expr) = parse_contract_expr(req_text) {
                            requires_exprs.push(expr);
                        }
                    }
                    for _ in &contract.ensures {
                        ensures_spans.push(*mms_span);
                    }
                    for ens_text in &contract.ensures {
                        if let Ok(expr) = parse_contract_expr(ens_text) {
                            ensures_exprs.push(expr);
                        }
                    }
                    for math_text in &contract.math {
                        if let Ok(expr) = parse_contract_expr(math_text) {
                            math_exprs.push(expr);
                        }
                    }
                }
                _ => {}
            }
        }

        if requires_exprs.is_empty() && ensures_exprs.is_empty() && math_exprs.is_empty() {
            return VerificationResult {
                func_name: func.name.clone(),
                status: VerifStatus::Unknown,
                message: "no contracts to verify".into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count: 0,
            };
        }

        let returns_real = func
            .ret
            .as_ref()
            .map_or(false, |t| matches!(t, Type::Name(n, _) if n == "f64"));

        let mut vars = Z3VarMap::new();
        let mut old_names: Vec<String> = Vec::with_capacity(func.params.len());

        for p in &func.params {
            if matches!(&p.ty, Type::Name(n, _) if n == "f64") {
                vars.insert_real(p.name.as_str(), Z3Real::new_const(p.name.as_str()));
            } else if matches!(&p.ty, Type::Name(n, _) if n == "string") {
                vars.insert_int(p.name.as_str(), Z3Int::new_const(p.name.as_str()));
                vars.insert_string_nonempty(
                    p.name.as_str(),
                    Z3Bool::new_const(format!("{}_ne", p.name)),
                );
                vars.insert_string_len(
                    p.name.as_str(),
                    Z3Int::new_const(format!("{}_len", p.name)),
                );
            } else {
                vars.insert_int(p.name.as_str(), Z3Int::new_const(p.name.as_str()));
            }
            old_names.push(format!("old_{}", p.name));
        }

        if returns_real {
            let z3_result = Z3Real::new_const("result");
            vars.insert_real("result", z3_result.clone());
        } else {
            let z3_result = Z3Int::new_const("result");
            vars.insert_int("result", z3_result.clone());
        }

        for (i, p) in func.params.iter().enumerate() {
            let old_name = old_names[i].as_str();
            if matches!(&p.ty, Type::Name(n, _) if n == "f64") {
                vars.insert_real(old_name, Z3Real::new_const(old_name));
            } else if matches!(&p.ty, Type::Name(n, _) if n == "string") {
                vars.insert_int(old_name, Z3Int::new_const(old_name));
                vars.insert_string_nonempty(
                    old_name,
                    Z3Bool::new_const(format!("{}_ne", old_name)),
                );
                vars.insert_string_len(
                    old_name,
                    Z3Int::new_const(format!("{}_len", old_name)),
                );
            } else {
                vars.insert_int(old_name, Z3Int::new_const(old_name));
            }
        }

        let body_return = extract_body_return(&func.body);

        for req in &requires_exprs {
            if let Some(z3_bool) = self.expr_to_z3_bool(req, &mut vars) {
                self.solver.assert(&z3_bool);
            }
        }

        for math in &math_exprs {
            if let Some(z3_bool) = self.expr_to_z3_bool(math, &mut vars) {
                self.solver.assert(&z3_bool);
            }
        }

        for inv in &invariant_exprs {
            if let Some(z3_bool) = self.expr_to_z3_bool(inv, &mut vars) {
                self.solver.assert(&z3_bool);
            }
        }

        for (i, p) in func.params.iter().enumerate() {
            let old_name = old_names[i].as_str();
            let param_z3 = vars.get_int(p.name.as_str()).cloned();
            let old_z3 = vars.get_int(old_name).cloned();
            if let (Some(pv), Some(ov)) = (param_z3, old_z3) {
                self.solver.assert(&ov.eq(&pv));
            }
        }

        for (i, p) in func.params.iter().enumerate() {
            let old_name = old_names[i].as_str();
            let param_z3 = vars.get_real(p.name.as_str()).cloned();
            let old_z3 = vars.get_real(old_name).cloned();
            if let (Some(pv), Some(ov)) = (param_z3, old_z3) {
                self.solver.assert(&ov.eq(&pv));
            }
        }

        if let Some(ref return_expr) = body_return {
            if returns_real {
                if let Some(body_z3) = self.expr_to_z3_real(return_expr, &mut vars) {
                    if let Some(r) = vars.get_real("result") {
                        self.solver.assert(&r.eq(&body_z3));
                    }
                }
            } else if let Some(body_z3) = self.expr_to_z3_int(return_expr, &mut vars) {
                if let Some(i) = vars.get_int("result") {
                    self.solver.assert(&i.eq(&body_z3));
                }
            }
        } else if func.ret.is_some() {
            // No return expression found but function has a return type:
            // bind result to 0 so postconditions don't pass vacuously.
            if returns_real {
                if let Some(r) = vars.get_real("result") {
                    let zero = Z3Real::from_int(&Z3Int::from_i64(0));
                    self.solver.assert(&r.eq(&zero));
                }
            } else {
                if let Some(i) = vars.get_int("result") {
                    self.solver.assert(&i.eq(&Z3Int::from_i64(0)));
                }
            }
        }

        let num_real_params = func
            .params
            .iter()
            .filter(|p| matches!(&p.ty, Type::Name(n, _) if n == "f64"))
            .count();
        let constraint_count = requires_exprs.len()
            + invariant_exprs.len()
            + math_exprs.len()
            + func.params.len() // old_* equality constraints (int)
            + num_real_params // old_* equality constraints (real)
            + if body_return.is_some() { 1 } else { 0 };

        match self.check_safe() {
            SatResult::Sat => {
                if !ensures_exprs.is_empty() {
                    self.solver.push();
                    for ens in &ensures_exprs {
                        if let Some(z3_bool) = self.expr_to_z3_bool(ens, &mut vars) {
                            self.solver.assert(&z3_bool.not());
                        }
                    }
                    match self.check_safe() {
                        SatResult::Unsat => {
                            self.solver.pop(1);
                            VerificationResult {
                                func_name: func.name.clone(),
                                status: VerifStatus::Verified,
                                message: "postconditions verified".into(),
                                diagnostic: None,
                                duration_us: start.elapsed().as_micros() as u64,
                                constraint_count,
                            }
                        }
                        SatResult::Sat => {
                            let model = self.solver.get_model();
                            let counterexample =
                                self.extract_counterexample(&model, &vars, &ensures_exprs);
                            self.solver.pop(1);
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
                                diagnostic: Some(diagnostic),
                                duration_us: start.elapsed().as_micros() as u64,
                                constraint_count,
                            }
                        }
                        SatResult::Unknown => {
                            self.solver.pop(1);
                            VerificationResult {
                                func_name: func.name.clone(),
                                status: VerifStatus::Unknown,
                                message: "verification inconclusive".into(),
                                diagnostic: None,
                                duration_us: start.elapsed().as_micros() as u64,
                                constraint_count,
                            }
                        }
                    }
                } else {
                    VerificationResult {
                        func_name: func.name.clone(),
                        status: VerifStatus::Verified,
                        message: "preconditions satisfiable, no postconditions".into(),
                        diagnostic: None,
                        duration_us: start.elapsed().as_micros() as u64,
                        constraint_count,
                    }
                }
            }
            SatResult::Unsat => {
                let req_span = requires_spans
                    .first()
                    .copied()
                    .unwrap_or_else(|| Span::single(func.pos.0, func.pos.1));
                let diagnostic = Diagnostic::error(
                    format!("preconditions are unsatisfiable for '{}'", func.name),
                    req_span,
                )
                .with_help("check that your requires conditions can actually be satisfied");
                VerificationResult {
                    func_name: func.name.clone(),
                    status: VerifStatus::Failed,
                    message: "preconditions are unsatisfiable".into(),
                    diagnostic: Some(diagnostic),
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count,
                }
            }
            SatResult::Unknown => VerificationResult {
                func_name: func.name.clone(),
                status: VerifStatus::Unknown,
                message: "precondition satisfiability unknown".into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
            },
        }
    }

    fn extract_counterexample(
        &self,
        model: &Option<z3::Model>,
        vars: &Z3VarMap,
        ensures_exprs: &[Expr],
    ) -> Counterexample {
        let mut assignments = Vec::new();
        let mut real_assignments = Vec::new();

        if let Some(model) = model {
            for (name, z3_var) in &vars.int_vars {
                if name == "result" || name.starts_with("old_") {
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
                if name == "result" || name.starts_with("old_") {
                    continue;
                }
                if let Some(val) = model.eval(z3_var, true) {
                    if let Some((num, den)) = val.as_rational() {
                        let f = (num as f64) / (den as f64);
                        real_assignments.push((name.clone(), f));
                    }
                }
            }
            if let Some(z3_var) = vars.real_vars.get("result") {
                if let Some(val) = model.eval(z3_var, true) {
                    if let Some((num, den)) = val.as_rational() {
                        let f = (num as f64) / (den as f64);
                        real_assignments.push(("result".to_string(), f));
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
            violated_ensures: violated,
            violated_indices,
        }
    }

    /// Try to resolve an expression to a concrete i64 value from the model.
    fn resolve_to_i64(expr: &Expr, model: &z3::Model, vars: &Z3VarMap) -> Option<i64> {
        match expr {
            Expr::Literal(Lit::Int(n)) => Some(*n),
            Expr::Ident(name) => vars.get_int(name).and_then(|z3_var| {
                model.eval(z3_var, true).and_then(|v| v.as_i64())
            }),
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    vars.get_int(&old_name).and_then(|z3_var| {
                        model.eval(z3_var, true).and_then(|v| v.as_i64())
                    })
                } else {
                    None
                }
            }
            Expr::Binary(op, lhs, rhs) => {
                let l = Self::resolve_to_i64(lhs, model, vars)?;
                let r = Self::resolve_to_i64(rhs, model, vars)?;
                match op {
                    BinOp::Add => Some(l + r),
                    BinOp::Sub => Some(l - r),
                    BinOp::Mul => Some(l * r),
                    BinOp::Div => Some(l / r),
                    BinOp::Mod => Some(l % r),
                    _ => None,
                }
            }
            Expr::Unary(UnOp::Neg, inner) => Self::resolve_to_i64(inner, model, vars).map(|v| -v),
            _ => None,
        }
    }

    /// Try to resolve an expression to a concrete f64 value from the model.
    fn resolve_to_f64(expr: &Expr, model: &z3::Model, vars: &Z3VarMap) -> Option<f64> {
        match expr {
            Expr::Literal(Lit::Int(n)) => Some(*n as f64),
            Expr::Literal(Lit::Float(f)) => Some(*f),
            Expr::Ident(name) => vars
                .get_real(name)
                .and_then(|z3_var| {
                    model
                        .eval(z3_var, true)
                        .and_then(|v| v.as_rational())
                        .map(|(num, den)| num as f64 / den as f64)
                })
                .or_else(|| {
                    vars.get_int(name)
                        .and_then(|z3_var| model.eval(z3_var, true).and_then(|v| v.as_i64()))
                        .map(|v| v as f64)
                }),
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    vars.get_real(&old_name)
                        .and_then(|z3_var| {
                            model
                                .eval(z3_var, true)
                                .and_then(|v| v.as_rational())
                                .map(|(num, den)| num as f64 / den as f64)
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
            Expr::Unary(UnOp::Neg, inner) => {
                Self::resolve_to_f64(inner, model, vars).map(|v| -v)
            }
            _ => None,
        }
    }

    fn eval_expr_on_model(expr: &Expr, model: &z3::Model, vars: &Z3VarMap) -> bool {
        match expr {
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
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    if let Some(z3_var) = vars.get_int(&old_name) {
                        match model.eval(z3_var, true) {
                            Some(val) => val.as_i64().map(|i| i != 0).unwrap_or(false),
                            None => false,
                        }
                    } else if let Some(z3_var) = vars.get_real(&old_name) {
                        model
                            .eval(z3_var, true)
                            .and_then(|v| v.as_rational())
                            .map(|(num, _den)| num != 0)
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
                            _ => false,
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
                            _ => false,
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
            _ => false,
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
            "verification failed for '{}': postcondition violation",
            func_name
        );

        let mut all_parts: Vec<String> = Vec::new();
        for (name, val) in &input_assignments {
            all_parts.push(format!("{} = {}", name, val));
        }
        for (name, val) in &counterexample.real_assignments {
            if name != "result" {
                all_parts.push(format!("{} = {:.6}", name, val));
            }
        }
        if !all_parts.is_empty() {
            message.push_str(&format!("\n  with inputs: {}", all_parts.join(", ")));
        }

        if let Some(result) = result_val {
            message.push_str(&format!("\n  body returns: result = {}", result));
        }
        if let Some(result) = result_real {
            message.push_str(&format!("\n  body returns: result = {:.6}", result));
        }

        for &idx in counterexample.violated_indices.iter() {
            if let Some(ens) = ensures_exprs.get(idx) {
                message.push_str(&format!("\n  but ensures {} = false", format_expr(ens)));
            }
        }

        let primary_span = ensures_spans
            .first()
            .copied()
            .unwrap_or_else(|| Span::single(func.pos.0, func.pos.1));
        let mut diag = Diagnostic::error(message, primary_span).with_code("E0500");

        if !requires_exprs.is_empty() {
            let req_strs: Vec<String> = requires_exprs.iter().map(format_expr).collect();
            let req_span = requires_spans
                .first()
                .copied()
                .unwrap_or_else(|| Span::single(func.pos.0, func.pos.1));
            diag = diag.with_note(
                format!("preconditions satisfied: {}", req_strs.join(", ")),
                req_span,
            );
        }

        for &idx in counterexample.violated_indices.iter() {
            if let Some(ens) = ensures_exprs.get(idx) {
                let ens_span = ensures_spans
                    .get(idx)
                    .copied()
                    .unwrap_or_else(|| Span::single(func.pos.0, func.pos.1));
                diag = diag.with_note(
                    format!("postcondition '{}' is false", format_expr(ens)),
                    ens_span,
                );
            }
        }

        if let Some(hint) = self.generate_fix_hint(func, counterexample) {
            diag = diag.with_help(hint);
        }

        diag
    }

    fn generate_fix_hint(
        &self,
        func: &FuncDef,
        counterexample: &Counterexample,
    ) -> Option<String> {
        let param_names: Vec<String> = func.params.iter().map(|p| p.name.clone()).collect();
        let result_val = counterexample
            .assignments
            .iter()
            .find(|(name, _)| name == "result")
            .map(|(_, val)| *val);

        if let Some(result) = result_val {
            let body_is_trivial = func.body.iter().all(|s| {
                matches!(
                    s,
                    Stmt::Expr(Expr::Literal(..)) | Stmt::Return(Some(Expr::Literal(..)))
                )
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

        let body_is_simple = func.body.iter().all(|s| {
            matches!(
                s,
                Stmt::Expr(Expr::Binary(..)) | Stmt::Return(Some(Expr::Binary(..)))
            )
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
}
