use crate::ast::*;
use crate::contracts;
use z3::{ast::Int, SatResult, Solver};

#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub func_name: String,
    pub status: VerifStatus,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifStatus {
    Verified,
    Failed,
    Unknown,
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
                            }
                        }
                        SatResult::Sat => {
                            self.solver.pop(1);
                            VerificationResult {
                                func_name: func.name.clone(),
                                status: VerifStatus::Failed,
                                message: "postcondition violation found".into(),
                            }
                        }
                        SatResult::Unknown => {
                            self.solver.pop(1);
                            VerificationResult {
                                func_name: func.name.clone(),
                                status: VerifStatus::Unknown,
                                message: "verification inconclusive".into(),
                            }
                        }
                    }
                } else {
                    VerificationResult {
                        func_name: func.name.clone(),
                        status: VerifStatus::Verified,
                        message: "preconditions satisfiable, no postconditions".into(),
                    }
                }
            }
            SatResult::Unsat => VerificationResult {
                func_name: func.name.clone(),
                status: VerifStatus::Failed,
                message: "preconditions are unsatisfiable".into(),
            },
            SatResult::Unknown => VerificationResult {
                func_name: func.name.clone(),
                status: VerifStatus::Unknown,
                message: "precondition satisfiability unknown".into(),
            },
        }
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
