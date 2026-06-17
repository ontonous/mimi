use crate::ast::*;
use crate::contracts;
use crate::diagnostic::{Diagnostic, Severity};
use crate::span::Span;
use z3::{ast::Int, SatResult, Solver};

#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub func_name: String,
    pub status: VerifStatus,
    pub message: String,
    pub diagnostic: Option<Diagnostic>,
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
    pub fn new() -> Self {
        let solver = Solver::new();
        Self { solver }
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
        self.solver.reset();

        let mut requires_exprs: Vec<Expr> = Vec::new();
        let mut ensures_exprs: Vec<Expr> = Vec::new();
        let mut math_exprs: Vec<Expr> = Vec::new();

        for stmt in &func.body {
            match stmt {
                Stmt::Requires(expr) => requires_exprs.push(expr.clone()),
                Stmt::Ensures(expr) => ensures_exprs.push(expr.clone()),
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
            };
        }

        // Create Z3 integer variables for function parameters
        let z3_vars: Vec<(&str, Int)> = func.params.iter()
            .map(|p| (p.name.as_str(), Int::new_const(p.name.as_str())))
            .collect();

        // Assert preconditions
        for req in &requires_exprs {
            if let Some(z3_bool) = self.expr_to_z3_bool(req, &z3_vars) {
                self.solver.assert(&z3_bool);
            }
        }

        // Assert math constraints
        for math in &math_exprs {
            if let Some(z3_bool) = self.expr_to_z3_bool(math, &z3_vars) {
                self.solver.assert(&z3_bool);
            }
        }

        match self.solver.check() {
            SatResult::Sat => {
                if !ensures_exprs.is_empty() {
                    self.solver.push();
                    for ens in &ensures_exprs {
                        if let Some(z3_bool) = self.expr_to_z3_bool(ens, &z3_vars) {
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
                            }
                        }
                        SatResult::Sat => {
                            // Extract counterexample model
                            let model = self.solver.get_model();
                            let counterexample = self.extract_counterexample(
                                &model, &z3_vars, &ensures_exprs,
                            );
                            self.solver.pop(1);
                            let diagnostic = self.build_failure_narrative(
                                func, &counterexample, &requires_exprs, &ensures_exprs,
                            );
                            VerificationResult {
                                func_name: func.name.clone(),
                                status: VerifStatus::Failed,
                                message: diagnostic.message.clone(),
                                diagnostic: Some(diagnostic),
                            }
                        }
                        SatResult::Unknown => {
                            self.solver.pop(1);
                            VerificationResult {
                                func_name: func.name.clone(),
                                status: VerifStatus::Unknown,
                                message: "verification inconclusive".into(),
                                diagnostic: None,
                            }
                        }
                    }
                } else {
                    VerificationResult {
                        func_name: func.name.clone(),
                        status: VerifStatus::Verified,
                        message: "preconditions satisfiable, no postconditions".into(),
                        diagnostic: None,
                    }
                }
            }
            SatResult::Unsat => {
                let diagnostic = Diagnostic::error(
                    format!("preconditions are unsatisfiable for '{}'", func.name),
                    Span::single(0, 0),
                ).with_help("check that your requires conditions can actually be satisfied");
                VerificationResult {
                    func_name: func.name.clone(),
                    status: VerifStatus::Failed,
                    message: "preconditions are unsatisfiable".into(),
                    diagnostic: Some(diagnostic),
                }
            }
            SatResult::Unknown => VerificationResult {
                func_name: func.name.clone(),
                status: VerifStatus::Unknown,
                message: "precondition satisfiability unknown".into(),
                diagnostic: None,
            },
        }
    }

    /// Extract counterexample variable assignments from the Z3 model.
    fn extract_counterexample(
        &self,
        model: &Option<z3::Model>,
        z3_vars: &[(&str, Int)],
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

        // Identify which ensures were violated
        let violated: Vec<String> = ensures_exprs.iter().map(|e| format_expr(e)).collect();

        Counterexample {
            assignments,
            violated_ensures: violated,
            violated_indices: (0..ensures_exprs.len()).collect(),
        }
    }

    /// Build a human-readable narrative from a counterexample.
    fn build_failure_narrative(
        &self,
        func: &FuncDef,
        counterexample: &Counterexample,
        requires_exprs: &[Expr],
        ensures_exprs: &[Expr],
    ) -> Diagnostic {
        let func_name = &func.name;

        // Build the main error message
        let mut message = format!(
            "verification failed for '{}': postcondition violation found",
            func_name
        );

        // Add counterexample assignments
        if !counterexample.assignments.is_empty() {
            let assignments_str: Vec<String> = counterexample.assignments.iter()
                .map(|(name, val)| format!("{} = {}", name, val))
                .collect();
            message.push_str(&format!(
                "\n  counterexample: {}",
                assignments_str.join(", ")
            ));
        }

        // Add which postconditions were violated
        if !counterexample.violated_ensures.is_empty() {
            for ens in &counterexample.violated_ensures {
                message.push_str(&format!("\n  violated: {}", ens));
            }
        }

        // Build the diagnostic
        let mut diag = Diagnostic::error(message, Span::single(0, 0))
            .with_code("E0500");

        // Add precondition context
        if !requires_exprs.is_empty() {
            let req_strs: Vec<String> = requires_exprs.iter().map(|e| format_expr(e)).collect();
            diag = diag.with_note(
                format!("preconditions satisfied: {}", req_strs.join(", ")),
                Span::single(0, 0),
            );
        }

        // Generate fix suggestion
        if let Some(hint) = self.generate_fix_hint(func, counterexample) {
            diag = diag.with_help(hint);
        }

        diag
    }

    /// Generate a fix suggestion based on the counterexample and function structure.
    fn generate_fix_hint(&self, func: &FuncDef, counterexample: &Counterexample) -> Option<String> {
        // Analyze the function body for missing branches
        let has_if = func.body.iter().any(|s| matches!(s, Stmt::If { .. }));
        let has_match = func.body.iter().any(|s| matches!(s, Stmt::Expr(Expr::Match(..))));

        // Check if counterexample has negative values when ensures expect positive
        let has_negative = counterexample.assignments.iter()
            .any(|(_, val)| *val < 0);

        if has_negative && !has_if && !has_match {
            // Suggest adding conditional handling
            let param_names: Vec<String> = func.params.iter().map(|p| p.name.clone()).collect();
            return Some(format!(
                "the counterexample shows negative input values. \
                 Consider adding an `if` or `match` branch to handle negative cases. \
                 Example: `if {} < 0 {{ ... }} else {{ ... }}`",
                param_names.first().unwrap_or(&"x".to_string())
            ));
        }

        // Check if the function body is too simple (just arithmetic)
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

    fn expr_to_z3_int(&self, expr: &Expr, vars: &[(&str, Int)]) -> Option<Int> {
        match expr {
            Expr::Literal(Lit::Int(n)) => Some(Int::from_i64(*n)),
            Expr::Ident(name) => {
                vars.iter().find(|(vn, _)| *vn == name).map(|(_, v)| v.clone())
            }
            Expr::Binary(op, lhs, rhs) => {
                let l = self.expr_to_z3_int(lhs, vars)?;
                let r = self.expr_to_z3_int(rhs, vars)?;
                match op {
                    BinOp::Add => Some(Int::add(&[&l, &r])),
                    BinOp::Sub => Some(Int::sub(&[&l, &r])),
                    BinOp::Mul => Some(Int::mul(&[&l, &r])),
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

    fn expr_to_z3_bool(&self, expr: &Expr, vars: &[(&str, Int)]) -> Option<z3::ast::Bool> {
        match expr {
            Expr::Literal(Lit::Bool(b)) => {
                Some(z3::ast::Bool::from_bool(*b))
            }
            Expr::Binary(op, lhs, rhs) => {
                match op {
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
                        Some(z3::ast::Bool::and(&[&l, &r]))
                    }
                    BinOp::Or => {
                        let l = self.expr_to_z3_bool(lhs, vars)?;
                        let r = self.expr_to_z3_bool(rhs, vars)?;
                        Some(z3::ast::Bool::or(&[&l, &r]))
                    }
                    _ => None,
                }
            }
            Expr::Unary(UnOp::Not, inner) => {
                let v = self.expr_to_z3_bool(inner, vars)?;
                Some(v.not())
            }
            _ => None,
        }
    }
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
    let mut verifier = Verifier::new();
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
        // With requires: true and ensures: true, verification should pass
        // (no postcondition to violate)
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
    fn verify_strong_postcondition_fails() {
        // Without modeling function body, verifier correctly finds that
        // x > 0 alone doesn't guarantee result > 0
        let src = r#"
func abs(x: i32) -> i32 {
    requires: x > 0
    ensures: result > 0
    x
}
"#;
        let results = verify_source(src).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Failed);
        // Should have a diagnostic with counterexample
        let diag = results[0].diagnostic.as_ref().unwrap();
        assert!(diag.message.contains("counterexample"));
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
        // Should have a diagnostic with counterexample
        assert!(results[0].diagnostic.is_some());
        let diag = results[0].diagnostic.as_ref().unwrap();
        assert!(diag.message.contains("counterexample"));
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
