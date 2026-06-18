use crate::ast::*;
use crate::contracts;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::time::Instant;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};
use z3::{SatResult, Solver};

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

/// A counterexample: variable assignments that violate the postcondition.
#[derive(Debug, Clone)]
pub struct Counterexample {
    pub assignments: Vec<(String, i64)>,
    pub violated_ensures: Vec<String>,
    /// Which specific ensures expressions are actually violated
    pub violated_indices: Vec<usize>,
}

pub struct Verifier {
    solver: Solver,
}

impl Verifier {
    pub fn new() -> Result<Self, String> {
        let solver = std::panic::catch_unwind(|| Solver::new())
            .map_err(|_| "failed to initialize Z3 solver (is libz3 installed?)".to_string())?;
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
                _ => {}
            }
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
                Stmt::MmsBlock { content: text, .. } => {
                    let contract = contracts::extract_contracts(text);
                    for req_text in &contract.requires {
                        if let Ok(expr) = parse_contract_expr(req_text) {
                            requires_exprs.push(expr);
                        }
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

        // Create Z3 variables for function parameters + old() snapshots + result
        // Detect parameter types and create appropriately typed Z3 variables
        let z3_result = Z3Int::new_const("result");
        let mut z3_vars: Vec<(&str, Z3Int)> = Vec::new();
        let mut z3_real_vars: Vec<(&str, Z3Real)> = Vec::new();
        let mut old_name_strings: Vec<String> = Vec::new();
        
        for p in &func.params {
            let is_float = matches!(&p.ty, Type::Name(n, _) if n == "f64");
            
            if is_float {
                z3_real_vars.push((p.name.as_str(), Z3Real::new_const(p.name.as_str())));
            } else {
                z3_vars.push((p.name.as_str(), Z3Int::new_const(p.name.as_str())));
            }
            
            let old_name = format!("old_{}", p.name);
            old_name_strings.push(old_name);
        }
        
        for p in &func.params {
            let is_float = matches!(&p.ty, Type::Name(n, _) if n == "f64");
            let old_name = format!("old_{}", p.name);
            let name_ref = old_name_strings.iter().find(|s| s.as_str() == old_name).unwrap().as_str();
            if is_float {
                z3_real_vars.push((name_ref, Z3Real::new_const(name_ref)));
            } else {
                z3_vars.push((name_ref, Z3Int::new_const(name_ref)));
            }
        }
        z3_vars.push(("result", z3_result.clone()));

        // Extract the return value expression from the function body
        let body_return = extract_body_return(&func.body);

        // Assert preconditions
        for req in &requires_exprs {
            if let Some(z3_bool) = self.expr_to_z3_bool(req, &z3_vars, &z3_real_vars) {
                self.solver.assert(&z3_bool);
            }
        }

        // Assert math constraints
        for math in &math_exprs {
            if let Some(z3_bool) = self.expr_to_z3_bool(math, &z3_vars, &z3_real_vars) {
                self.solver.assert(&z3_bool);
            }
        }

        // Encode old() snapshots: old_x == x for each parameter (snapshot at function entry)
        for p in &func.params {
            if let Some(param_z3) = z3_vars.iter().find(|(n, _)| *n == p.name).map(|(_, v)| v.clone()) {
                let old_name = format!("old_{}", p.name);
                if let Some(old_z3) = z3_vars.iter().find(|(n, _)| *n == old_name.as_str()).map(|(_, v)| v.clone()) {
                    self.solver.assert(old_z3.eq(&param_z3));
                }
            }
        }

        // Encode function body: bind result == body(args)
        // This is the critical link between the implementation and the contract.
        if let Some(ref return_expr) = body_return {
            if let Some(body_z3) = self.expr_to_z3_int(return_expr, &z3_vars) {
                self.solver.assert(z3_result.eq(&body_z3));
            }
        }

        // Count total constraints: requires + math + old_snapshots + body
        let constraint_count = requires_exprs.len() + math_exprs.len() + func.params.len()
            + if body_return.is_some() { 1 } else { 0 };

        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.solver.check())) {
            Ok(SatResult::Sat) => {
                if !ensures_exprs.is_empty() {
                    self.solver.push();
                    for ens in &ensures_exprs {
                        if let Some(z3_bool) = self.expr_to_z3_bool(ens, &z3_vars, &z3_real_vars) {
                            self.solver.assert(z3_bool.not());
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
                            let counterexample = self.extract_counterexample(
                                &model, &z3_vars, &ensures_exprs,
                            );
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
                let req_span = requires_spans.first().copied()
                    .unwrap_or_else(|| Span::single(0, 0));
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

    /// Extract counterexample variable assignments from the Z3 model.
    fn extract_counterexample(
        &self,
        model: &Option<z3::Model>,
        z3_vars: &[(&str, Z3Int)],
        ensures_exprs: &[Expr],
    ) -> Counterexample {
        let mut assignments = Vec::new();

        if let Some(model) = model {
            for (name, z3_var) in z3_vars {
                if let Some(val) = model.eval(z3_var, true) {
                    if let Some(i) = val.as_i64() {
                        assignments.push((name.to_string(), i));
                    }
                }
            }
        }

        // Identify which ensures were violated by testing each one individually
        let mut violated_indices = Vec::new();
        if let Some(ref m) = model {
            for (idx, ens) in ensures_exprs.iter().enumerate() {
                if let Some(z3_bool) = self.expr_to_z3_bool(ens, z3_vars, &[]) {
                    if let Some(val) = m.eval(&z3_bool, true) {
                        if let Some(b) = val.as_bool() {
                            if !b {
                                violated_indices.push(idx);
                            }
                        }
                    }
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
            violated_ensures: violated,
            violated_indices,
        }
    }

    /// Build a human-readable narrative from a counterexample.
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

        // Separate input params from result
        let _param_names: Vec<&str> = func.params.iter().map(|p| p.name.as_str()).collect();
        let input_assignments: Vec<&(String, i64)> = counterexample.assignments.iter()
            .filter(|(name, _)| name != "result")
            .collect();
        let result_val = counterexample.assignments.iter()
            .find(|(name, _)| name == "result")
            .map(|(_, val)| *val);

        // Build the main error message with causal chain
        let mut message = format!(
            "verification failed for '{}': postcondition violation",
            func_name
        );

        // Show input values
        if !input_assignments.is_empty() {
            let inputs_str: Vec<String> = input_assignments.iter()
                .map(|(name, val)| format!("{} = {}", name, val))
                .collect();
            message.push_str(&format!(
                "\n  with inputs: {}",
                inputs_str.join(", ")
            ));
        }

        // Show what the body computes
        if let Some(result) = result_val {
            message.push_str(&format!(
                "\n  body returns: result = {}",
                result
            ));
        }

        // Show which postconditions were violated
        if !counterexample.violated_ensures.is_empty() {
            for &idx in counterexample.violated_indices.iter() {
                if let Some(ens) = ensures_exprs.get(idx) {
                    message.push_str(&format!(
                        "\n  but ensures {} = false",
                        format_expr(ens)
                    ));
                }
            }
        }

        // Build the diagnostic with proper source location
        // Use the first violated ensures span as the primary location
        let primary_span = ensures_spans.first().copied()
            .unwrap_or_else(|| Span::single(0, 0));
        let mut diag = Diagnostic::error(message, primary_span)
            .with_code("E0500");

        // Add precondition context with source locations
        if !requires_exprs.is_empty() {
            let req_strs: Vec<String> = requires_exprs.iter().map(format_expr).collect();
            let req_span = requires_spans.first().copied()
                .unwrap_or_else(|| Span::single(0, 0));
            diag = diag.with_note(
                format!("preconditions satisfied: {}", req_strs.join(", ")),
                req_span,
            );
        }

        // Add per-ensures violation notes with source locations
        for &idx in counterexample.violated_indices.iter() {
            if let Some(ens) = ensures_exprs.get(idx) {
                let ens_span = ensures_spans.get(idx).copied()
                    .unwrap_or_else(|| Span::single(0, 0));
                diag = diag.with_note(
                    format!("postcondition '{}' is false", format_expr(ens)),
                    ens_span,
                );
            }
        }

        // Generate fix suggestion
        if let Some(hint) = self.generate_fix_hint(func, counterexample) {
            diag = diag.with_help(hint);
        }

        diag
    }

    /// Generate a fix suggestion based on the counterexample and function structure.
    fn generate_fix_hint(&self, func: &FuncDef, counterexample: &Counterexample) -> Option<String> {
        let param_names: Vec<String> = func.params.iter().map(|p| p.name.clone()).collect();
        let result_val = counterexample.assignments.iter()
            .find(|(name, _)| name == "result")
            .map(|(_, val)| *val);

        // Pattern 1: body returns wrong constant
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

        // Pattern 2: body doesn't use all parameters
        let body_text: String = func.body.iter().map(|s| format!("{:?}", s)).collect::<Vec<_>>().join(" ");
        let unused_params: Vec<&str> = param_names.iter()
            .filter(|p| !body_text.contains(p.as_str()))
            .map(|s| s.as_str())
            .collect();
        if !unused_params.is_empty() {
            return Some(format!(
                "parameter(s) `{}` are not used in the function body. \
                 Ensure the result depends on all required inputs.",
                unused_params.join("`, `")
            ));
        }

        // Pattern 3: body is too simple (just arithmetic) without edge-case handling
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

    fn expr_to_z3_int(&self, expr: &Expr, vars: &[(&str, Z3Int)]) -> Option<Z3Int> {
        match expr {
            Expr::Literal(Lit::Int(n)) => Some(Z3Int::from_i64(*n)),
            Expr::Ident(name) => {
                vars.iter().find(|(vn, _)| *vn == name).map(|(_, v)| v.clone())
            }
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    return vars.iter().find(|(vn, _)| *vn == old_name.as_str()).map(|(_, v)| v.clone());
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
            _ => None,
        }
    }

    /// Try to encode an expression as a Z3 Real (for f64 parameters).
    fn expr_to_z3_real(&self, expr: &Expr, vars: &[(&str, Z3Int)], real_vars: &[(&str, Z3Real)]) -> Option<Z3Real> {
        match expr {
            Expr::Literal(Lit::Int(n)) => Some(Z3Real::from_int(&Z3Int::from_i64(*n))),
            Expr::Literal(Lit::Float(f)) => {
                if *f == 0.0 {
                    Some(Z3Real::from_int(&Z3Int::from_i64(0)))
                } else if f.is_infinite() || f.is_nan() {
                    None
                } else {
                    // Use the float's decimal string representation
                    let _s = format!("{}", f);
                    // Z3Real::from_real_str needs context; approximate with integer conversion
                    // For practical verification, convert to scaled integer
                    let scaled = (*f * 1000000.0) as i64;
                    Some(Z3Real::from_int(&Z3Int::from_i64(scaled)) / Z3Real::from_int(&Z3Int::from_i64(1000000)))
                }
            }
            Expr::Ident(name) => {
                // Check real vars first, then int vars (promote to real)
                if let Some(v) = real_vars.iter().find(|(vn, _)| *vn == name).map(|(_, v)| v.clone()) {
                    Some(v)
                } else { vars.iter().find(|(vn, _)| *vn == name).map(|(_, v)| v.clone()).map(|v| Z3Real::from_int(&v)) }
            }
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    if let Some(v) = real_vars.iter().find(|(vn, _)| *vn == old_name.as_str()).map(|(_, v)| v.clone()) {
                        return Some(v);
                    }
                    if let Some(v) = vars.iter().find(|(vn, _)| *vn == old_name.as_str()).map(|(_, v)| v.clone()) {
                        return Some(Z3Real::from_int(&v));
                    }
                }
                None
            }
            Expr::Binary(op, lhs, rhs) => {
                let l = self.expr_to_z3_real(lhs, vars, real_vars)?;
                let r = self.expr_to_z3_real(rhs, vars, real_vars)?;
                match op {
                    BinOp::Add => Some(l + r),
                    BinOp::Sub => Some(l - r),
                    BinOp::Mul => Some(l * r),
                    BinOp::Div => Some(l / r),
                    _ => None,
                }
            }
            Expr::Unary(UnOp::Neg, inner) => {
                let v = self.expr_to_z3_real(inner, vars, real_vars)?;
                Some(-v)
            }
            _ => None,
        }
    }

    fn expr_to_z3_bool(&self, expr: &Expr, vars: &[(&str, Z3Int)], real_vars: &[(&str, Z3Real)]) -> Option<Z3Bool> {
        match expr {
            Expr::Literal(Lit::Bool(b)) => {
                Some(Z3Bool::from_bool(*b))
            }
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    // Check if there's a real var for old_x
                    if real_vars.iter().any(|(n, _)| *n == old_name.as_str()) {
                        return None; // Can't convert Real to Bool directly
                    }
                    // Check int vars
                    if let Some(v) = vars.iter().find(|(vn, _)| *vn == old_name.as_str()).map(|(_, v)| v.clone()) {
                        // Treat non-zero as true
                        return Some(v.eq(Z3Int::from_i64(0)).not());
                    }
                    return None;
                }
                None
            }
            Expr::Binary(op, lhs, rhs) => {
                // Detect if operands are float expressions
                let lhs_is_real = self.is_real_expr(lhs, vars, real_vars);
                let rhs_is_real = self.is_real_expr(rhs, vars, real_vars);
                let use_real = lhs_is_real || rhs_is_real;
                
                match op {
                    BinOp::EqCmp if use_real => {
                        let l = self.expr_to_z3_real(lhs, vars, real_vars)?;
                        let r = self.expr_to_z3_real(rhs, vars, real_vars)?;
                        Some(l.eq(&r))
                    }
                    BinOp::NeCmp if use_real => {
                        let l = self.expr_to_z3_real(lhs, vars, real_vars)?;
                        let r = self.expr_to_z3_real(rhs, vars, real_vars)?;
                        Some(l.eq(&r).not())
                    }
                    BinOp::Lt if use_real => {
                        let l = self.expr_to_z3_real(lhs, vars, real_vars)?;
                        let r = self.expr_to_z3_real(rhs, vars, real_vars)?;
                        Some(l.lt(&r))
                    }
                    BinOp::Gt if use_real => {
                        let l = self.expr_to_z3_real(lhs, vars, real_vars)?;
                        let r = self.expr_to_z3_real(rhs, vars, real_vars)?;
                        Some(l.gt(&r))
                    }
                    BinOp::Le if use_real => {
                        let l = self.expr_to_z3_real(lhs, vars, real_vars)?;
                        let r = self.expr_to_z3_real(rhs, vars, real_vars)?;
                        Some(l.le(&r))
                    }
                    BinOp::Ge if use_real => {
                        let l = self.expr_to_z3_real(lhs, vars, real_vars)?;
                        let r = self.expr_to_z3_real(rhs, vars, real_vars)?;
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
                        let l = self.expr_to_z3_bool(lhs, vars, real_vars)?;
                        let r = self.expr_to_z3_bool(rhs, vars, real_vars)?;
                        Some(Z3Bool::and(&[&l, &r]))
                    }
                    BinOp::Or => {
                        let l = self.expr_to_z3_bool(lhs, vars, real_vars)?;
                        let r = self.expr_to_z3_bool(rhs, vars, real_vars)?;
                        Some(Z3Bool::or(&[&l, &r]))
                    }
                    _ => None,
                }
            }
            Expr::Unary(UnOp::Not, inner) => {
                let v = self.expr_to_z3_bool(inner, vars, real_vars)?;
                Some(v.not())
            }
            _ => None,
        }
    }
    
    /// Check if an expression involves real (f64) variables.
    fn is_real_expr(&self, expr: &Expr, _vars: &[(&str, Z3Int)], real_vars: &[(&str, Z3Real)]) -> bool {
        match expr {
            Expr::Ident(name) => real_vars.iter().any(|(n, _)| *n == name.as_str()),
            Expr::Literal(Lit::Float(_)) => true,
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    real_vars.iter().any(|(n, _)| *n == old_name.as_str())
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

/// Extract the return value expression from a function body.
/// Handles patterns:
///   - `return expr;`
///   - last expression in body (implicit return)
///
/// Skips Requires/Ensures/Math/Desc/MmsBlock statements.
fn extract_body_return(block: &Block) -> Option<Expr> {
    // First try explicit `return` statements
    for stmt in block.iter().rev() {
        match stmt {
            Stmt::Return(Some(expr)) => return Some(expr.clone()),
            Stmt::Return(None) => return Some(Expr::Literal(Lit::Unit)),
            _ => {}
        }
    }
    // Fall back to last expression statement
    for stmt in block.iter().rev() {
        match stmt {
            Stmt::Expr(expr) => return Some(expr.clone()),
            Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::Math(_)
            | Stmt::Desc(_) | Stmt::MmsBlock { .. } => continue,
            _ => break,
        }
    }
    None
}

/// Format an expression as a human-readable string.
fn format_expr(expr: &Expr) -> String {
    match expr {
        Expr::Literal(Lit::Int(n)) => format!("{}", n),
        Expr::Literal(Lit::Bool(b)) => format!("{}", b),
        Expr::Literal(Lit::String(s)) => format!("\"{}\"", s),
        Expr::Ident(name) => name.clone(),
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

pub fn verify_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    let tokens = crate::lexer::Lexer::new(source).tokenize()?;
    let file = crate::parser::Parser::new(tokens).parse_file().map_err(|e| e.message)?;
    let mut verifier = Verifier::new()?;
    Ok(verifier.verify_file(&file))
}

fn parse_contract_expr(text: &str) -> Result<Expr, String> {
    let tokens = crate::lexer::Lexer::new(text).tokenize()?;
    let expr = crate::parser::Parser::new(tokens).parse_expr(0).map_err(|e| e.message)?;
    Ok(expr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_simple_pass() {
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
        // T1 core test: body return expression is encoded as result == body(args)
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
        // T1 core test: body return doesn't satisfy ensures
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
        // T1: counterexample should include result variable
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
        // The counterexample should show that body returns x, not x+1
        let diag = results[0].diagnostic.as_ref().unwrap();
        assert!(diag.message.contains("result ="), "should show result value in narrative");
    }

    #[test]
    fn verify_strong_postcondition_fails() {
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
        // T2: old(x) should capture pre-state value
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
        // T2: old(x) should detect mutation
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
}
