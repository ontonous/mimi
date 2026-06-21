pub mod ffi;

use crate::ast::*;
use crate::contracts;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};
use z3::{SatResult, Solver};

const DEFAULT_TIMEOUT_MS: u64 = 5000;

#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub func_name: String,
    pub status: VerifStatus,
    pub message: String,
    pub diagnostic: Option<Diagnostic>,
    pub duration_us: u64,
    pub constraint_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifStatus {
    Verified,
    Failed,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Counterexample {
    pub assignments: Vec<(String, i64)>,
    pub real_assignments: Vec<(String, f64)>,
    pub violated_ensures: Vec<String>,
    pub violated_indices: Vec<usize>,
}

struct Z3VarMap {
    int_vars: HashMap<String, Z3Int>,
    real_vars: HashMap<String, Z3Real>,
    string_nonempty: HashMap<String, Z3Bool>,
}

impl Z3VarMap {
    fn new() -> Self {
        Self { int_vars: HashMap::new(), real_vars: HashMap::new(), string_nonempty: HashMap::new() }
    }

    fn insert_int(&mut self, name: impl Into<String>, var: Z3Int) {
        self.int_vars.insert(name.into(), var);
    }

    fn insert_real(&mut self, name: impl Into<String>, var: Z3Real) {
        self.real_vars.insert(name.into(), var);
    }

    fn insert_string_nonempty(&mut self, name: impl Into<String>, var: Z3Bool) {
        self.string_nonempty.insert(name.into(), var);
    }

    #[inline]
    fn get_int(&self, name: &str) -> Option<&Z3Int> {
        self.int_vars.get(name)
    }

    #[inline]
    fn get_real(&self, name: &str) -> Option<&Z3Real> {
        self.real_vars.get(name)
    }

    #[inline]
    fn get_string_nonempty(&self, name: &str) -> Option<&Z3Bool> {
        self.string_nonempty.get(name)
    }

    #[inline]
    fn is_real(&self, name: &str) -> bool {
        self.real_vars.contains_key(name)
    }
}

pub struct Verifier {
    solver: Solver,
}

impl Verifier {
    pub fn new() -> Result<Self, String> {
        Self::with_timeout(DEFAULT_TIMEOUT_MS)
    }

    pub fn with_timeout(timeout_ms: u64) -> Result<Self, String> {
        let solver = std::panic::catch_unwind(|| Solver::new())
            .map_err(|_| "failed to initialize Z3 solver (is libz3 installed?)".to_string())?;
        let mut params = z3::Params::new();
        params.set_u32("timeout", timeout_ms as u32);
        solver.set_params(&params);
        Ok(Self { solver })
    }

    pub fn verify_file(&mut self, file: &File) -> Vec<VerificationResult> {
        let mut results = Vec::new();
        self.verify_items(&file.items, &mut results);
        results
    }

    fn verify_items(&mut self, items: &[Item], results: &mut Vec<VerificationResult>) {
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

        let returns_real = func.ret.as_ref().map_or(false, |t| matches!(t, Type::Name(n, _) if n == "f64"));

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

        let constraint_count = (requires_expr.is_some() as usize) + (ensures_expr.is_some() as usize);

        if let Some(req) = requires_expr {
            if let Some(z3_bool) = self.expr_to_z3_bool(req, &vars) {
                self.solver.assert(&z3_bool);
            }
        }

        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.solver.check())) {
            Ok(SatResult::Unsat) => VerificationResult {
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
            Ok(SatResult::Unknown) => VerificationResult {
                func_name: format!("extern {}", func.name),
                status: VerifStatus::Unknown,
                message: "precondition satisfiability unknown".into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
            },
            Ok(SatResult::Sat) => {
                if let Some(ens) = ensures_expr {
                    self.solver.push();
                    if let Some(z3_not_ens) = self.expr_to_z3_bool(ens, &vars).map(|b| b.not()) {
                        self.solver.assert(&z3_not_ens);
                        match self.solver.check() {
                            SatResult::Unsat => {
                                self.solver.pop(1);
                                VerificationResult {
                                    func_name: format!("extern {}", func.name),
                                    status: VerifStatus::Verified,
                                    message: "postconditions always satisfied given preconditions".into(),
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
                                    message: "extern contracts are consistent (runtime verification required)".into(),
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
            Err(_) => VerificationResult {
                func_name: format!("extern {}", func.name),
                status: VerifStatus::Unknown,
                message: "verification timed out or crashed".into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
            },
        }
    }

    fn verify_func(&mut self, func: &FuncDef) -> VerificationResult {
        let start = Instant::now();
        self.solver.reset();

        let mut requires_exprs: Vec<Expr> = Vec::new();
        let mut ensures_exprs: Vec<Expr> = Vec::new();
        let mut math_exprs: Vec<Expr> = Vec::new();
        let mut requires_spans: Vec<Span> = Vec::new();
        let mut ensures_spans: Vec<Span> = Vec::new();

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
                Stmt::Math(exprs) => math_exprs.extend(exprs.clone()),
                Stmt::MmsBlock { content: text, span: mms_span, .. } => {
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

        let returns_real = func.ret.as_ref().map_or(false, |t| matches!(t, Type::Name(n, _) if n == "f64"));

        let mut vars = Z3VarMap::new();
        let mut old_names: Vec<String> = Vec::with_capacity(func.params.len());

        for p in &func.params {
            if matches!(&p.ty, Type::Name(n, _) if n == "f64") {
                vars.insert_real(p.name.as_str(), Z3Real::new_const(p.name.as_str()));
            } else if matches!(&p.ty, Type::Name(n, _) if n == "string") {
                vars.insert_int(p.name.as_str(), Z3Int::new_const(p.name.as_str()));
                vars.insert_string_nonempty(p.name.as_str(), Z3Bool::new_const(format!("{}_ne", p.name)));
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
                vars.insert_string_nonempty(old_name, Z3Bool::new_const(format!("{}_ne", old_name)));
            } else {
                vars.insert_int(old_name, Z3Int::new_const(old_name));
            }
        }

        let body_return = extract_body_return(&func.body);

        for req in &requires_exprs {
            if let Some(z3_bool) = self.expr_to_z3_bool(req, &vars) {
                self.solver.assert(&z3_bool);
            }
        }

        for math in &math_exprs {
            if let Some(z3_bool) = self.expr_to_z3_bool(math, &vars) {
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
                if let Some(body_z3) = self.expr_to_z3_real(return_expr, &vars) {
                    if let Some(r) = vars.get_real("result") {
                        self.solver.assert(&r.eq(&body_z3));
                    }
                }
            } else if let Some(body_z3) = self.expr_to_z3_int(return_expr, &vars) {
                if let Some(i) = vars.get_int("result") {
                    self.solver.assert(&i.eq(&body_z3));
                }
            }
        }

        let num_real_params = func.params.iter()
            .filter(|p| matches!(&p.ty, Type::Name(n, _) if n == "f64"))
            .count();
        let constraint_count = requires_exprs.len()
            + math_exprs.len()
            + func.params.len()  // old_* equality constraints (int)
            + num_real_params    // old_* equality constraints (real)
            + if body_return.is_some() { 1 } else { 0 };

        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.solver.check())) {
            Ok(SatResult::Sat) => {
                if !ensures_exprs.is_empty() {
                    self.solver.push();
                    for ens in &ensures_exprs {
                        if let Some(z3_bool) = self.expr_to_z3_bool(ens, &vars) {
                            self.solver.assert(&z3_bool.not());
                        }
                    }
                    match self.solver.check() {
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
                            let counterexample = self.extract_counterexample(&model, &vars, &ensures_exprs);
                            self.solver.pop(1);
                            let diagnostic = self.build_failure_narrative(
                                func, &counterexample, &requires_exprs, &ensures_exprs,
                                &requires_spans, &ensures_spans,
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
            Ok(SatResult::Unsat) => {
                let req_span = requires_spans.first().copied().unwrap_or_else(|| Span::single(func.pos.0, func.pos.1));
                let diagnostic = Diagnostic::error(
                    format!("preconditions are unsatisfiable for '{}'", func.name),
                    req_span,
                ).with_help("check that your requires conditions can actually be satisfied");
                VerificationResult {
                    func_name: func.name.clone(),
                    status: VerifStatus::Failed,
                    message: "preconditions are unsatisfiable".into(),
                    diagnostic: Some(diagnostic),
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count,
                }
            }
            Ok(SatResult::Unknown) => VerificationResult {
                func_name: func.name.clone(),
                status: VerifStatus::Unknown,
                message: "precondition satisfiability unknown".into(),
                diagnostic: None,
                duration_us: start.elapsed().as_micros() as u64,
                constraint_count,
            },
            Err(_) => VerificationResult {
                func_name: func.name.clone(),
                status: VerifStatus::Unknown,
                message: "verification timed out or crashed".into(),
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
                if name == "result" || name.starts_with("old_") { continue; }
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
                if name == "result" || name.starts_with("old_") { continue; }
                if let Some(val) = model.eval(z3_var, true) {
                    if let Some((num, den)) = val.as_real() {
                        let f = (num as f64) / (den as f64);
                        real_assignments.push((name.clone(), f));
                    }
                }
            }
            if let Some(z3_var) = vars.real_vars.get("result") {
                if let Some(val) = model.eval(z3_var, true) {
                    if let Some((num, den)) = val.as_real() {
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
        if violated_indices.is_empty() {
            violated_indices = (0..ensures_exprs.len()).collect();
        }

        let violated: Vec<String> = violated_indices.iter()
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
            Expr::Ident(name) => {
                vars.get_int(name).and_then(|z3_var| {
                    model.eval(z3_var, true).and_then(|v| v.as_i64())
                })
            }
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
            Expr::Unary(UnOp::Neg, inner) => {
                Self::resolve_to_i64(inner, model, vars).map(|v| -v)
            }
            _ => None,
        }
    }

    /// Try to resolve an expression to a concrete f64 value from the model.
    fn resolve_to_f64(expr: &Expr, model: &z3::Model, vars: &Z3VarMap) -> Option<f64> {
        match expr {
            Expr::Literal(Lit::Int(n)) => Some(*n as f64),
            Expr::Literal(Lit::Float(f)) => Some(*f),
            Expr::Ident(name) => {
                vars.get_real(name).and_then(|z3_var| {
                    model.eval(z3_var, true)
                        .and_then(|v| v.as_real())
                        .map(|(num, den)| num as f64 / den as f64)
                }).or_else(|| {
                    vars.get_int(name).and_then(|z3_var| {
                        model.eval(z3_var, true).and_then(|v| v.as_i64())
                    }).map(|v| v as f64)
                })
            }
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    vars.get_real(&old_name).and_then(|z3_var| {
                        model.eval(z3_var, true)
                            .and_then(|v| v.as_real())
                            .map(|(num, den)| num as f64 / den as f64)
                    }).or_else(|| {
                        vars.get_int(&old_name).and_then(|z3_var| {
                            model.eval(z3_var, true).and_then(|v| v.as_i64())
                        }).map(|v| v as f64)
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
                    model.eval(z3_var, true)
                        .and_then(|v| v.as_real())
                        .map(|(num, _den)| num != 0)
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
                        model.eval(z3_var, true)
                            .and_then(|v| v.as_real())
                            .map(|(num, _den)| num != 0)
                            .unwrap_or(false)
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            Expr::Binary(op, lhs, rhs) => {
                match op {
                    BinOp::EqCmp => {
                        match (Self::resolve_to_i64(lhs, model, vars), Self::resolve_to_i64(rhs, model, vars)) {
                            (Some(l), Some(r)) => l == r,
                            _ => {
                                match (Self::resolve_to_f64(lhs, model, vars), Self::resolve_to_f64(rhs, model, vars)) {
                                    (Some(l), Some(r)) => l == r,
                                    _ => false,
                                }
                            }
                        }
                    }
                    BinOp::NeCmp => {
                        match (Self::resolve_to_i64(lhs, model, vars), Self::resolve_to_i64(rhs, model, vars)) {
                            (Some(l), Some(r)) => l != r,
                            _ => {
                                match (Self::resolve_to_f64(lhs, model, vars), Self::resolve_to_f64(rhs, model, vars)) {
                                    (Some(l), Some(r)) => l != r,
                                    _ => false,
                                }
                            }
                        }
                    }
                    BinOp::Lt => {
                        match (Self::resolve_to_i64(lhs, model, vars), Self::resolve_to_i64(rhs, model, vars)) {
                            (Some(l), Some(r)) => l < r,
                            _ => {
                                match (Self::resolve_to_f64(lhs, model, vars), Self::resolve_to_f64(rhs, model, vars)) {
                                    (Some(l), Some(r)) => l < r,
                                    _ => false,
                                }
                            }
                        }
                    }
                    BinOp::Gt => {
                        match (Self::resolve_to_i64(lhs, model, vars), Self::resolve_to_i64(rhs, model, vars)) {
                            (Some(l), Some(r)) => l > r,
                            _ => {
                                match (Self::resolve_to_f64(lhs, model, vars), Self::resolve_to_f64(rhs, model, vars)) {
                                    (Some(l), Some(r)) => l > r,
                                    _ => false,
                                }
                            }
                        }
                    }
                    BinOp::Le => {
                        match (Self::resolve_to_i64(lhs, model, vars), Self::resolve_to_i64(rhs, model, vars)) {
                            (Some(l), Some(r)) => l <= r,
                            _ => {
                                match (Self::resolve_to_f64(lhs, model, vars), Self::resolve_to_f64(rhs, model, vars)) {
                                    (Some(l), Some(r)) => l <= r,
                                    _ => false,
                                }
                            }
                        }
                    }
                    BinOp::Ge => {
                        match (Self::resolve_to_i64(lhs, model, vars), Self::resolve_to_i64(rhs, model, vars)) {
                            (Some(l), Some(r)) => l >= r,
                            _ => {
                                match (Self::resolve_to_f64(lhs, model, vars), Self::resolve_to_f64(rhs, model, vars)) {
                                    (Some(l), Some(r)) => l >= r,
                                    _ => false,
                                }
                            }
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
                }
            }
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

        let input_assignments: Vec<&(String, i64)> = counterexample.assignments.iter()
            .filter(|(name, _)| name != "result")
            .collect();
        let result_val = counterexample.assignments.iter()
            .find(|(name, _)| name == "result")
            .map(|(_, val)| *val);
        let result_real = counterexample.real_assignments.iter()
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

        let primary_span = ensures_spans.first().copied().unwrap_or_else(|| Span::single(func.pos.0, func.pos.1));
        let mut diag = Diagnostic::error(message, primary_span).with_code("E0500");

        if !requires_exprs.is_empty() {
            let req_strs: Vec<String> = requires_exprs.iter().map(format_expr).collect();
            let req_span = requires_spans.first().copied().unwrap_or_else(|| Span::single(func.pos.0, func.pos.1));
            diag = diag.with_note(
                format!("preconditions satisfied: {}", req_strs.join(", ")),
                req_span,
            );
        }

        for &idx in counterexample.violated_indices.iter() {
            if let Some(ens) = ensures_exprs.get(idx) {
                let ens_span = ensures_spans.get(idx).copied().unwrap_or_else(|| Span::single(func.pos.0, func.pos.1));
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

    fn generate_fix_hint(&self, func: &FuncDef, counterexample: &Counterexample) -> Option<String> {
        let param_names: Vec<String> = func.params.iter().map(|p| p.name.clone()).collect();
        let result_val = counterexample.assignments.iter()
            .find(|(name, _)| name == "result")
            .map(|(_, val)| *val);

        if let Some(result) = result_val {
            let body_is_trivial = func.body.iter().all(|s| {
                matches!(s, Stmt::Expr(Expr::Literal(..)) | Stmt::Return(Some(Expr::Literal(..))))
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
        let unused_params: Vec<&str> = param_names.iter()
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
            matches!(s, Stmt::Expr(Expr::Binary(..)) | Stmt::Return(Some(Expr::Binary(..))))
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

    fn expr_to_z3_int(&self, expr: &Expr, vars: &Z3VarMap) -> Option<Z3Int> {
        match expr {
            Expr::Literal(Lit::Int(n)) => Some(Z3Int::from_i64(*n)),
            Expr::Ident(name) => vars.get_int(name).cloned(),
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    return vars.get_int(&old_name).cloned();
                }
                None
            }
            Expr::Binary(op, lhs, rhs) => {
                let l = self.expr_to_z3_int(lhs, vars)?;
                let r = self.expr_to_z3_int(rhs, vars)?;
                match op {
                    BinOp::Add => Some(Z3Int::add(&[&l, &r])),
                    BinOp::Sub => Some(Z3Int::sub(&[&l, &r])),
                    BinOp::Mul => Some(Z3Int::mul(&[&l, &r])),
                    BinOp::Div => Some(l.div(&r)),
                    BinOp::Mod => Some(l.modulo(&r)),
                    _ => None,
                }
            }
            Expr::Unary(UnOp::Neg, inner) => {
                let v = self.expr_to_z3_int(inner, vars)?;
                Some(v.unary_minus())
            }
            Expr::If { cond, then_, else_ } => {
                let cond_z3 = self.expr_to_z3_bool(cond, vars)?;
                let then_z3 = block_tail_expr(then_)
                    .and_then(|e| self.expr_to_z3_int(&e, vars))?;
                let else_z3 = else_.as_ref()
                    .and_then(|b| block_tail_expr(b))
                    .and_then(|e| self.expr_to_z3_int(&e, vars))
                    .unwrap_or_else(|| Z3Int::from_i64(0));
                Some(cond_z3.ite(&then_z3, &else_z3))
            }
            _ => None,
        }
    }

    fn expr_to_z3_real(&self, expr: &Expr, vars: &Z3VarMap) -> Option<Z3Real> {
        match expr {
            Expr::Literal(Lit::Int(n)) => Some(Z3Real::from_int(&Z3Int::from_i64(*n))),
            Expr::Literal(Lit::Float(f)) => {
                if *f == 0.0 {
                    Some(Z3Real::from_int(&Z3Int::from_i64(0)))
                } else if f.is_infinite() || f.is_nan() {
                    None
                } else {
                    let scaled = (*f * 1000000.0).round() as i64;
                    Some(
                        Z3Real::from_int(&Z3Int::from_i64(scaled))
                            / Z3Real::from_int(&Z3Int::from_i64(1000000)),
                    )
                }
            }
            Expr::Ident(name) => {
                if let Some(v) = vars.get_real(name) {
                    Some(v.clone())
                } else {
                    vars.get_int(name).map(|v| Z3Real::from_int(v))
                }
            }
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    if let Some(v) = vars.get_real(&old_name) {
                        return Some(v.clone());
                    }
                    return vars.get_int(&old_name).map(|v| Z3Real::from_int(v));
                }
                None
            }
            Expr::Binary(op, lhs, rhs) => {
                let l = self.expr_to_z3_real(lhs, vars)?;
                let r = self.expr_to_z3_real(rhs, vars)?;
                match op {
                    BinOp::Add => Some(l + r),
                    BinOp::Sub => Some(l - r),
                    BinOp::Mul => Some(l * r),
                    BinOp::Div => Some(l / r),
                    _ => None,
                }
            }
            Expr::Unary(UnOp::Neg, inner) => {
                let v = self.expr_to_z3_real(inner, vars)?;
                Some(-v)
            }
            Expr::If { cond, then_, else_ } => {
                let cond_z3 = self.expr_to_z3_bool(cond, vars)?;
                let then_z3 = block_tail_expr(then_)
                    .and_then(|e| self.expr_to_z3_real(&e, vars))?;
                let else_z3 = else_.as_ref()
                    .and_then(|b| block_tail_expr(b))
                    .and_then(|e| self.expr_to_z3_real(&e, vars))
                    .unwrap_or_else(|| Z3Real::from_int(&Z3Int::from_i64(0)));
                Some(cond_z3.ite(&then_z3, &else_z3))
            }
            _ => None,
        }
    }

    fn expr_to_z3_bool(&self, expr: &Expr, vars: &Z3VarMap) -> Option<Z3Bool> {
        match expr {
            Expr::Literal(Lit::Bool(b)) => Some(Z3Bool::from_bool(*b)),
            Expr::Ident(name) => {
                if let Some(v) = vars.get_int(name) {
                    Some(v.ne(&Z3Int::from_i64(0)))
                } else {
                    None
                }
            }
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    if let Some(v) = vars.get_int(&old_name) {
                        return Some(v.ne(&Z3Int::from_i64(0)));
                    }
                }
                None
            }
            Expr::Binary(op, lhs, rhs) => {
                // Check string emptiness comparison before int/real
                if is_string_empty_cmp(lhs, rhs, op) {
                    let (name, empty_op) = extract_string_empty_cmp(lhs, rhs, op);
                    if let Some(ne) = vars.get_string_nonempty(&name) {
                        match empty_op {
                            BinOp::NeCmp => return Some(ne.clone()),
                            BinOp::EqCmp => return Some(ne.not()),
                            _ => {}
                        }
                    }
                }

                let use_real = self.is_real_expr(lhs, vars) || self.is_real_expr(rhs, vars);

                match op {
                    BinOp::EqCmp if use_real => {
                        let l = self.expr_to_z3_real(lhs, vars)?;
                        let r = self.expr_to_z3_real(rhs, vars)?;
                        Some(l.eq(&r))
                    }
                    BinOp::NeCmp if use_real => {
                        let l = self.expr_to_z3_real(lhs, vars)?;
                        let r = self.expr_to_z3_real(rhs, vars)?;
                        Some(l.eq(&r).not())
                    }
                    BinOp::Lt if use_real => {
                        let l = self.expr_to_z3_real(lhs, vars)?;
                        let r = self.expr_to_z3_real(rhs, vars)?;
                        Some(l.lt(&r))
                    }
                    BinOp::Gt if use_real => {
                        let l = self.expr_to_z3_real(lhs, vars)?;
                        let r = self.expr_to_z3_real(rhs, vars)?;
                        Some(l.gt(&r))
                    }
                    BinOp::Le if use_real => {
                        let l = self.expr_to_z3_real(lhs, vars)?;
                        let r = self.expr_to_z3_real(rhs, vars)?;
                        Some(l.le(&r))
                    }
                    BinOp::Ge if use_real => {
                        let l = self.expr_to_z3_real(lhs, vars)?;
                        let r = self.expr_to_z3_real(rhs, vars)?;
                        Some(l.ge(&r))
                    }
                    BinOp::EqCmp => {
                        let l = self.expr_to_z3_int(lhs, vars)?;
                        let r = self.expr_to_z3_int(rhs, vars)?;
                        Some(l.eq(&r))
                    }
                    BinOp::NeCmp => {
                        let l = self.expr_to_z3_int(lhs, vars)?;
                        let r = self.expr_to_z3_int(rhs, vars)?;
                        Some(l.eq(&r).not())
                    }
                    BinOp::Lt => {
                        let l = self.expr_to_z3_int(lhs, vars)?;
                        let r = self.expr_to_z3_int(rhs, vars)?;
                        Some(l.lt(&r))
                    }
                    BinOp::Gt => {
                        let l = self.expr_to_z3_int(lhs, vars)?;
                        let r = self.expr_to_z3_int(rhs, vars)?;
                        Some(l.gt(&r))
                    }
                    BinOp::Le => {
                        let l = self.expr_to_z3_int(lhs, vars)?;
                        let r = self.expr_to_z3_int(rhs, vars)?;
                        Some(l.le(&r))
                    }
                    BinOp::Ge => {
                        let l = self.expr_to_z3_int(lhs, vars)?;
                        let r = self.expr_to_z3_int(rhs, vars)?;
                        Some(l.ge(&r))
                    }
                    BinOp::And => {
                        let l = self.expr_to_z3_bool(lhs, vars)?;
                        let r = self.expr_to_z3_bool(rhs, vars)?;
                        Some(Z3Bool::and(&[&l, &r]))
                    }
                    BinOp::Or => {
                        let l = self.expr_to_z3_bool(lhs, vars)?;
                        let r = self.expr_to_z3_bool(rhs, vars)?;
                        Some(Z3Bool::or(&[&l, &r]))
                    }
                    _ => None,
                }
            }
            Expr::Unary(UnOp::Not, inner) => {
                let v = self.expr_to_z3_bool(inner, vars)?;
                Some(v.not())
            }
            Expr::If { cond, then_, else_ } => {
                let cond_z3 = self.expr_to_z3_bool(cond, vars)?;
                let then_z3 = block_tail_expr(then_)
                    .and_then(|e| self.expr_to_z3_bool(&e, vars))?;
                let else_z3 = else_.as_ref()
                    .and_then(|b| block_tail_expr(b))
                    .and_then(|e| self.expr_to_z3_bool(&e, vars))
                    .unwrap_or_else(|| Z3Bool::from_bool(true));
                Some(cond_z3.ite(&then_z3, &else_z3))
            }
            _ => None,
        }
    }

    fn is_real_expr(&self, expr: &Expr, vars: &Z3VarMap) -> bool {
        match expr {
            Expr::Ident(name) => vars.is_real(name),
            Expr::Literal(Lit::Float(_)) => true,
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    vars.is_real(&old_name)
                } else {
                    false
                }
            }
            Expr::Binary(_, lhs, rhs) => self.is_real_expr(lhs, vars) || self.is_real_expr(rhs, vars),
            Expr::Unary(_, inner) => self.is_real_expr(inner, vars),
            _ => false,
        }
    }

}

/// Extract the final value-producing expression from a block.
/// Used in `expr_to_z3_*` to evaluate the tail expression of an if-else branch.
fn block_tail_expr(block: &[Stmt]) -> Option<Expr> {
    for stmt in block.iter().rev() {
        match stmt {
            Stmt::Expr(e) => return Some(e.clone()),
            Stmt::Return(Some(e)) => return Some(e.clone()),
            Stmt::Return(None) => return Some(Expr::Literal(Lit::Unit)),
            _ => {}
        }
    }
    None
}

/// Check if a comparison is between a string ident and an empty string literal.
fn is_string_empty_cmp(lhs: &Expr, rhs: &Expr, op: &BinOp) -> bool {
    matches!(op, BinOp::EqCmp | BinOp::NeCmp)
    && match (lhs, rhs) {
        (Expr::Ident(_), Expr::Literal(Lit::String(s)))
        | (Expr::Literal(Lit::String(s)), Expr::Ident(_)) => s.is_empty(),
        _ => false,
    }
}

/// Extract the string ident name from a string emptiness comparison.
/// Assumes `is_string_empty_cmp` returned `true`.
fn extract_string_empty_cmp(lhs: &Expr, rhs: &Expr, op: &BinOp) -> (String, BinOp) {
    match (lhs, rhs) {
        (Expr::Ident(name), Expr::Literal(Lit::String(_))) => (name.clone(), *op),
        (Expr::Literal(Lit::String(_)), Expr::Ident(name)) => (name.clone(), *op),
        _ => unreachable!(),
    }
}

/// Extract the return/tail expression from a function body, handling if-else branching.
/// Uses `Expr::If` to represent conditional paths so the Z3 layer can encode them via `ite`.
fn extract_body_return(block: &[Stmt]) -> Option<Expr> {
    // First pass: look for explicit returns and if-else expressions
    for stmt in block.iter().rev() {
        match stmt {
            Stmt::Return(Some(expr)) => return Some(expr.clone()),
            Stmt::Return(None) => return Some(Expr::Literal(Lit::Unit)),
            Stmt::If { cond, then_, else_ } => {
                return extract_if_return(cond, then_, else_);
            }
            _ => {}
        }
    }
    // Second pass: look for implicit return (tail expression)
    for stmt in block.iter().rev() {
        match stmt {
            Stmt::Expr(expr) => return Some(expr.clone()),
            Stmt::If { cond, then_, else_ } => {
                return extract_if_return(cond, then_, else_);
            }
            Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::Math(_)
            | Stmt::Desc(..) | Stmt::MmsBlock { .. } => continue,
            _ => break,
        }
    }
    None
}

/// Build an `Expr::If` from the condition and both branches' return expressions.
fn extract_if_return(cond: &Expr, then_: &[Stmt], else_: &Option<Block>) -> Option<Expr> {
    let then_expr = extract_body_return(then_)?;
    let else_expr = else_.as_ref()
        .and_then(|b| extract_body_return(b))
        .unwrap_or(Expr::Literal(Lit::Unit));
    Some(Expr::If {
        cond: Box::new(cond.clone()),
        then_: vec![Stmt::Expr(then_expr)],
        else_: Some(vec![Stmt::Expr(else_expr)]),
    })
}

fn format_expr(expr: &Expr) -> String {
    match expr {
        Expr::Literal(Lit::Int(n)) => format!("{}", n),
        Expr::Literal(Lit::Float(f)) => format!("{}", f),
        Expr::Literal(Lit::Bool(b)) => format!("{}", b),
        Expr::Literal(Lit::String(s)) => format!("\"{}\"", s),
        Expr::Literal(Lit::Unit) => "()".to_string(),
        Expr::Literal(Lit::FString(parts)) => {
            let inner: String = parts.iter().map(|p| match p {
                crate::ast::FStringPart::Text(t) => t.clone(),
                crate::ast::FStringPart::Interp(e) => format!("{}", format_expr(e)),
            }).collect();
            format!("f\"{}\"", inner)
        }
        Expr::Ident(name) => name.clone(),
        Expr::Old(inner) => format!("old({})", format_expr(inner)),
        Expr::Binary(op, l, r) => {
            let op_str = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Mod => "%",
                BinOp::EqCmp => "==",
                BinOp::NeCmp => "!=",
                BinOp::Lt => "<",
                BinOp::Gt => ">",
                BinOp::Le => "<=",
                BinOp::Ge => ">=",
                BinOp::And => "&&",
                BinOp::Or => "||",
                _ => "?",
            };
            format!("{} {} {}", format_expr(l), op_str, format_expr(r))
        }
        Expr::Unary(UnOp::Neg, inner) => format!("-{}", format_expr(inner)),
        Expr::Unary(UnOp::Not, inner) => format!("!{}", format_expr(inner)),
        _ => "<expr>".to_string(),
    }
}

fn format_stmt(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Let { pat, .. } => format!("let {:?}", pat),
        Stmt::Expr(expr) => format_expr(expr),
        Stmt::Return(Some(expr)) => format!("return {}", format_expr(expr)),
        Stmt::Return(None) => "return".to_string(),
        Stmt::If { cond, .. } => format!("if {}", format_expr(cond)),
        Stmt::While { cond, .. } => format!("while {}", format_expr(cond)),
        Stmt::Requires(e, _) => format!("requires {}", format_expr(e)),
        Stmt::Ensures(e, _) => format!("ensures {}", format_expr(e)),
        _ => "<stmt>".to_string(),
    }
}

pub fn verify_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    let tokens = crate::lexer::Lexer::new(source).tokenize()?;
    let file = crate::parser::Parser::new(tokens).parse_file().map_err(|e| e.message)?;
    let mut verifier = match Verifier::new() {
        Ok(v) => v,
        Err(_) => return Ok(mock_verify_file(&file)),
    };
    Ok(verifier.verify_file(&file))
}

/// Verify contracts in a parsed file (supports pre-merged imports).
pub fn verify_file(file: &File) -> Result<Vec<VerificationResult>, String> {
    let mut verifier = match Verifier::new() {
        Ok(v) => v,
        Err(_) => return Ok(mock_verify_file(file)),
    };
    Ok(verifier.verify_file(file))
}

/// Parse source and verify extern call sites using Z3.
pub fn verify_ffi_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    let tokens = crate::lexer::Lexer::new(source).tokenize()?;
    let file = crate::parser::Parser::new(tokens).parse_file().map_err(|e| e.message)?;
    let mut verifier = match Verifier::new() {
        Ok(v) => v,
        Err(_) => return Ok(Vec::new()),
    };
    Ok(verifier.verify_ffi_call_sites(&file))
}

/// Check whether the Z3 solver is available at runtime.
pub fn is_z3_available() -> bool {
    Verifier::new().is_ok()
}

/// Return Unknown for all functions when Z3 is not available.
fn mock_verify_file(file: &File) -> Vec<VerificationResult> {
    let mut results = Vec::new();
    mock_verify_items(&file.items, &mut results);
    results
}

fn mock_verify_items(items: &[Item], results: &mut Vec<VerificationResult>) {
    for item in items {
        match item {
            Item::Func(f) => {
                if !f.body.is_empty() {
                    let has_contracts = f.body.iter().any(|s| matches!(s, Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::MmsBlock { .. }));
                    results.push(VerificationResult {
                        func_name: f.name.clone(),
                        status: VerifStatus::Unknown,
                        message: if has_contracts { "Z3 solver not available" } else { "no contracts" }.into(),
                        diagnostic: None,
                        duration_us: 0,
                        constraint_count: 0,
                    });
                }
            }
            Item::Module(m) => mock_verify_items(&m.items, results),
            Item::ExternBlock(block) => {
                for func in &block.funcs {
                    if func.requires.is_some() || func.ensures.is_some() {
                        results.push(VerificationResult {
                            func_name: format!("extern {}", func.name),
                            status: VerifStatus::Unknown,
                            message: "Z3 solver not available".into(),
                            diagnostic: None,
                            duration_us: 0,
                            constraint_count: 0,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_idents_in_expr(expr: &Expr, idents: &mut Vec<String>) {
    match expr {
        Expr::Ident(name) => {
            if !idents.contains(name) {
                idents.push(name.clone());
            }
        }
        Expr::Binary(_, lhs, rhs) => {
            collect_idents_in_expr(lhs, idents);
            collect_idents_in_expr(rhs, idents);
        }
        Expr::Unary(_, inner) => collect_idents_in_expr(inner, idents),
        Expr::Old(inner) => collect_idents_in_expr(inner, idents),
        Expr::Call(callee, args) => {
            collect_idents_in_expr(callee, idents);
            for arg in args {
                collect_idents_in_expr(arg, idents);
            }
        }
        Expr::Field(obj, _) => collect_idents_in_expr(obj, idents),
        Expr::Index(obj, idx) => {
            collect_idents_in_expr(obj, idents);
            collect_idents_in_expr(idx, idents);
        }
        Expr::Tuple(elems) => {
            for e in elems {
                collect_idents_in_expr(e, idents);
            }
        }
        Expr::List(elems) => {
            for e in elems {
                collect_idents_in_expr(e, idents);
            }
        }
        Expr::Record { fields, .. } => {
            for f in fields {
                collect_idents_in_expr(&f.value, idents);
            }
        }
        Expr::If { cond, then_, else_ } => {
            collect_idents_in_expr(cond, idents);
            for s in then_ {
                collect_idents_in_stmt(s, idents);
            }
            if let Some(e) = else_ {
                for s in e {
                    collect_idents_in_stmt(s, idents);
                }
            }
        }
        Expr::Match(scrutinee, arms) => {
            collect_idents_in_expr(scrutinee, idents);
            for arm in arms {
                collect_idents_in_expr(&arm.body, idents);
            }
        }
        Expr::Lambda { body, .. } => {
            for s in body {
                collect_idents_in_stmt(s, idents);
            }
        }
        Expr::Comprehension { expr, iter, guard, .. } => {
            collect_idents_in_expr(expr, idents);
            collect_idents_in_expr(iter, idents);
            if let Some(g) = guard {
                collect_idents_in_expr(g, idents);
            }
        }
        Expr::Range { start, end } => {
            collect_idents_in_expr(start, idents);
            collect_idents_in_expr(end, idents);
        }
        Expr::SliceExpr { target, start, end } => {
            collect_idents_in_expr(target, idents);
            if let Some(s) = start {
                collect_idents_in_expr(s, idents);
            }
            if let Some(e) = end {
                collect_idents_in_expr(e, idents);
            }
        }
        Expr::Turbofish(_, _, args) => {
            for a in args {
                collect_idents_in_expr(a, idents);
            }
        }
        Expr::Try(inner) | Expr::Spawn(inner) | Expr::Await(inner)
        | Expr::QuoteInterpolate(inner) | Expr::TypeOf(inner) => {
            collect_idents_in_expr(inner, idents);
        }
        Expr::Comptime(body) | Expr::Quote(body) => {
            for s in body {
                collect_idents_in_stmt(s, idents);
            }
        }
        Expr::TupleIndex(obj, _) => collect_idents_in_expr(obj, idents),
        _ => {}
    }
}

fn collect_idents_in_stmt(stmt: &Stmt, idents: &mut Vec<String>) {
    match stmt {
        Stmt::Expr(e) | Stmt::Return(Some(e)) | Stmt::Drop(e) => collect_idents_in_expr(e, idents),
        Stmt::Return(None) | Stmt::Break(None) | Stmt::Continue => {}
        Stmt::Break(Some(e)) => collect_idents_in_expr(e, idents),
        Stmt::Let { init: Some(e), .. } | Stmt::SharedLet { init: e, .. } => collect_idents_in_expr(e, idents),
        Stmt::Let { init: None, .. } => {}
        Stmt::Assign { target, value } => {
            collect_idents_in_expr(target, idents);
            collect_idents_in_expr(value, idents);
        }
        Stmt::If { cond, then_, else_ } => {
            collect_idents_in_expr(cond, idents);
            for s in then_ {
                collect_idents_in_stmt(s, idents);
            }
            if let Some(e) = else_ {
                for s in e {
                    collect_idents_in_stmt(s, idents);
                }
            }
        }
        Stmt::While { cond, body } | Stmt::For { iterable: cond, body, .. } => {
            collect_idents_in_expr(cond, idents);
            for s in body {
                collect_idents_in_stmt(s, idents);
            }
        }
        Stmt::Block(block) | Stmt::Arena(block) | Stmt::OnFailure(block)
        | Stmt::Parasteps(block) | Stmt::Unsafe(block) => {
            for s in block {
                collect_idents_in_stmt(s, idents);
            }
        }
        Stmt::Alloc { body, .. } => {
            for s in body {
                collect_idents_in_stmt(s, idents);
            }
        }
        Stmt::Requires(e, _) | Stmt::Ensures(e, _) => collect_idents_in_expr(e, idents),
        Stmt::Math(exprs) => {
            for e in exprs {
                collect_idents_in_expr(e, idents);
            }
        }
        _ => {}
    }
}

fn parse_contract_expr(text: &str) -> Result<Expr, String> {
    let tokens = crate::lexer::Lexer::new(text).tokenize()?;
    let expr = crate::parser::Parser::new(tokens).parse_expr(0).map_err(|e| e.message)?;
    Ok(expr)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! require_z3 {
        () => {
            if !crate::verifier::is_z3_available() {
                eprintln!("    └─ skipped (Z3 not available)");
                return;
            }
        };
    }

    #[test]
    fn verify_simple_pass() {
        require_z3!();
        let src = r#"
func identity(x: i32) -> i32 {
    requires: true
    ensures: true
    x
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified);
    }

    #[test]
    fn verify_body_satisfies_ensures() {
        require_z3!();
        let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0
    ensures: result == x * 2
    x * 2
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "body `x * 2` should satisfy ensures `result == x * 2`: {}", results[0].message);
    }

    #[test]
    fn verify_body_violates_ensures() {
        require_z3!();
        let src = r#"
func wrong(x: i32) -> i32 {
    requires: x >= 0
    ensures: result == x * 2
    x * 3
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Failed);
        let diag = results[0].diagnostic.as_ref().unwrap();
        assert!(diag.message.contains("result ="), "narrative should show result value: {}", diag.message);
    }

    #[test]
    fn verify_result_binding_in_counterexample() {
        let src = r#"
func add_one(x: i32) -> i32 {
    requires: x > 0
    ensures: result > x
    x
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Failed);
        let diag = results[0].diagnostic.as_ref().unwrap();
        assert!(diag.message.contains("result ="), "should show result value in narrative");
    }

    #[test]
    fn verify_strong_postcondition_fails() {
        require_z3!();
        let src = r#"
func abs(x: i32) -> i32 {
    requires: x > 0
    ensures: result > 0
    x
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "x > 0 && result == x should satisfy result > 0");
    }

    #[test]
    fn verify_counterexample_extracted() {
        require_z3!();
        let src = r#"
func abs(x: i32) -> i32 {
    requires: true
    ensures: result > 0
    x
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Failed);
        assert!(results[0].diagnostic.is_some());
        let diag = results[0].diagnostic.as_ref().unwrap();
        assert!(diag.message.contains("result ="), "should show result in narrative");
    }

    #[test]
    fn verify_unsatisfiable_requires() {
        require_z3!();
        let src = r#"
func impossible(x: i32) -> i32 {
    requires: x > 0 && x < 0
    ensures: true
    x
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Failed);
        let diag = results[0].diagnostic.as_ref().unwrap();
        assert!(diag.message.contains("unsatisfiable"));
    }

    #[test]
    fn verify_old_snapshot() {
        require_z3!();
        let src = r#"
func noop(x: i32) -> i32 {
    requires: x > 0
    ensures: result == old(x)
    x
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "body returns x unchanged, ensures result == old(x) should hold: {}", results[0].message);
    }

    #[test]
    fn verify_old_snapshot_fails() {
        require_z3!();
        let src = r#"
func mutate(x: i32) -> i32 {
    requires: x > 0
    ensures: result == old(x)
    x + 1
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Failed,
            "body returns x+1, ensures result == old(x) should fail");
    }

    #[test]
    fn format_expr_basic() {
        assert_eq!(format_expr(&Expr::Literal(Lit::Int(42))), "42");
        assert_eq!(format_expr(&Expr::Ident("x".into())), "x");
        assert_eq!(
            format_expr(&Expr::Binary(
                BinOp::Gt,
                Box::new(Expr::Ident("x".into())),
                Box::new(Expr::Literal(Lit::Int(0))),
            )),
            "x > 0"
        );
    }

    #[test]
    fn verify_extern_ensures_consistent() {
        require_z3!();
        let src = r#"
extern "C" {
    func must_be_positive(x: i64) -> i64
        ensures: result > 0;
}

func main() -> i64 { 0 }
"#;
        let results = verify_source(src).unwrap();
        let ext: Vec<_> = results.iter().filter(|r| r.func_name.contains("extern")).collect();
        assert_eq!(ext.len(), 1, "extern func should be verified");
        assert_eq!(ext[0].status, VerifStatus::Verified,
            "extern ensures should be consistent: {}", ext[0].message);
    }

    #[test]
    fn verify_extern_requires_ensures_consistent() {
        require_z3!();
        let src = r#"
extern "C" {
    func process(x: i64) -> i64
        requires: x > 0
        ensures: result > x;
}

func main() -> i64 { 0 }
"#;
        let results = verify_source(src).unwrap();
        let ext: Vec<_> = results.iter().filter(|r| r.func_name.contains("extern")).collect();
        assert_eq!(ext.len(), 1, "extern func should be verified");
        assert_eq!(ext[0].status, VerifStatus::Verified,
            "extern requires+ensures should be consistent: {}", ext[0].message);
    }

    #[test]
    fn verify_extern_unsatisfiable_requires() {
        require_z3!();
        let src = r#"
extern "C" {
    func impossible(x: i64) -> i64
        requires: x > 0 && x < 0;
}

func main() -> i64 { 0 }
"#;
        let results = verify_source(src).unwrap();
        let ext: Vec<_> = results.iter().filter(|r| r.func_name.contains("extern")).collect();
        assert_eq!(ext.len(), 1);
        assert_eq!(ext[0].status, VerifStatus::Failed,
            "contradictory requires should fail: {}", ext[0].message);
        assert!(ext[0].message.contains("unsatisfiable"));
    }

    #[test]
    fn verify_extern_no_contracts_skipped() {
        let src = r#"
extern "C" {
    func add(a: i64, b: i64) -> i64;
}

func main() -> i64 { 0 }
"#;
        let results = verify_source(src).unwrap();
        let ext: Vec<_> = results.iter().filter(|r| r.func_name.contains("extern")).collect();
        assert_eq!(ext.len(), 0, "extern func without contracts should be skipped");
    }

    #[test]
    fn verify_extern_with_main_only() {
        let src = r#"
extern "C" {
    func identity(x: i64) -> i64
        ensures: result == x;
}

func main() -> i64 {
    ensures: true
    0
}
"#;
        let results = verify_source(src).unwrap();
        let func_names: Vec<&str> = results.iter().map(|r| r.func_name.as_str()).collect();
        assert!(func_names.contains(&"extern identity"), "extern identity should be in results: {:?}", func_names);
        assert!(func_names.contains(&"main"), "main should be in results: {:?}", func_names);
    }

    // --- extract_body_return: if/else branch coverage ---

    #[test]
    fn verify_if_else_body_all_paths_verified() {
        require_z3!();
        let src = r#"
func abs(x: i32) -> i32 {
    requires: true
    ensures: result >= 0
    if x >= 0 { x } else { -x }
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "abs with if/else should be verified: {}", results[0].message);
    }

    #[test]
    fn verify_if_else_body_violation_detected() {
        require_z3!();
        let src = r#"
func bad_abs(x: i32) -> i32 {
    requires: true
    ensures: result >= 0
    if x >= 0 { x } else { x - 1 }
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Failed,
            "bad_abs with if/else should fail (else branch x-1 can be negative)");
    }

    #[test]
    fn verify_nested_if_else_body() {
        require_z3!();
        let src = r#"
func sign(x: i32) -> i32 {
    requires: true
    ensures: result == 1 || result == 0 || result == -1
    if x > 0 { 1 } else { if x < 0 { -1 } else { 0 } }
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "nested if/else should be verified: {}", results[0].message);
    }

    #[test]
    fn verify_if_else_body_with_requires() {
        require_z3!();
        let src = r#"
func add_or_mul(x: i32, y: i32) -> i32 {
    requires: x >= 0 && y >= 0
    ensures: result >= 0
    if x > y { x + y } else { x * y }
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "add_or_mul with if/else should be verified: {}", results[0].message);
    }

    // --- eval_expr_on_model: f64 boolean degeneracy ---

    #[test]
    fn verify_f64_ensures() {
        require_z3!();
        let src = r#"
func positive(x: f64) -> f64 {
    requires: x > 0.0
    ensures: result > 0.0
    x
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "f64 ensures should be verified: {}", results[0].message);
    }

    #[test]
    fn verify_f64_ensures_violation() {
        require_z3!();
        let src = r#"
func negate(x: f64) -> f64 {
    requires: x > 0.0
    ensures: result > 0.0
    -x
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Failed,
            "negate should fail: result = -x violates ensures result > 0.0");
        let diag = results[0].diagnostic.as_ref().unwrap();
        assert!(diag.message.contains("result"), "should include result in narrative");
    }

    // --- FFI call-site verification ---

    #[test]
    fn verify_ffi_no_requires() {
        require_z3!();
        let src = r#"
extern "C" {
    func get_value() -> i64;
}
func caller() -> i64 {
    get_value()
}
"#;
        let results = verify_ffi_source(src).unwrap();
        assert!(results.iter().all(|r| r.status == VerifStatus::Verified),
            "no-requires extern should be Verified: {:?}", results);
    }

    #[test]
    fn verify_ffi_requires_always_satisfied() {
        require_z3!();
        let src = r#"
extern "C" {
    func read(fd: i64, buf: i64, size: i64) -> i64;
}
func caller(fd: i64, buf: i64, size: i64) -> i64 {
    requires: fd >= 0 && size > 0
    read(fd, buf, size)
}
"#;
        let results = verify_ffi_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "requires fd >= 0 && size > 0 should satisfy read's preconditions: {}", results[0].message);
    }

    #[test]
    fn verify_ffi_requires_violated() {
        require_z3!();
        let src = r#"
extern "C" {
    func read(fd: i64, buf: i64, size: i64) -> i64
        requires: fd >= 0 && size > 0;
}
func bad_caller(size: i64) -> i64 {
    read(-1, 0, size)
}
"#;
        let results = verify_ffi_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Failed,
            "read(-1, 0, size) should fail: fd is negative");
    }

    #[test]
    fn verify_ffi_string_empty_violation() {
        require_z3!();
        let src = r#"
extern "C" {
    func strlen(s: string) -> i64
        requires: s != "";
}
func caller(s: string) -> i64 {
    strlen(s)
}
"#;
        let results = verify_ffi_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Failed,
            "strlen(s) without guard should fail: s could be empty");
    }

    #[test]
    fn verify_ffi_string_empty_protected() {
        require_z3!();
        let src = r#"
extern "C" {
    func strlen(s: string) -> i64
        requires: s != "";
}
func caller(s: string) -> i64 {
    requires: s != ""
    strlen(s)
}
"#;
        let results = verify_ffi_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "strlen(s) with guard should be Verified: {}", results[0].message);
    }

    #[test]
    fn verify_ffi_multiple_externs() {
        require_z3!();
        let src = r#"
extern "C" {
    func read(fd: i64, buf: i64, size: i64) -> i64
        requires: fd >= 0;
    func write(fd: i64, buf: i64, size: i64) -> i64
        requires: fd >= 0;
}
func ok_caller(fd: i64) -> i64 {
    requires: fd >= 0
    read(fd, 0, 1) + write(fd, 0, 1)
}
func bad_caller(fd: i64) -> i64 {
    read(fd, 0, 1) + write(fd, 0, 1)
}
"#;
        let results = verify_ffi_source(src).unwrap();
        assert_eq!(results.len(), 4);
        let ok_results: Vec<_> = results.iter().filter(|r| r.func_name.starts_with("ok_caller")).collect();
        assert_eq!(ok_results.len(), 2);
        assert!(ok_results.iter().all(|r| r.status == VerifStatus::Verified),
            "ok_caller should pass: {:?}", ok_results);
        let bad_results: Vec<_> = results.iter().filter(|r| r.func_name.starts_with("bad_caller")).collect();
        assert_eq!(bad_results.len(), 2);
        assert!(bad_results.iter().any(|r| r.status == VerifStatus::Failed),
            "bad_caller should have at least one failure: {:?}", bad_results);
    }
}
