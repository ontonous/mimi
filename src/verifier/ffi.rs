use super::*;
use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};
use z3::SatResult;

impl super::Verifier {
    pub fn verify_ffi_call_sites(&mut self, file: &File) -> Vec<VerificationResult> {
        let mut results = Vec::new();
        let mut externs: HashMap<String, ExternFunc> = HashMap::new();
        Self::collect_externs(&file.items, &mut externs);
        let extern_names: HashSet<String> = externs.keys().cloned().collect();

        for item in &file.items {
            if let Item::Func(func) = item {
                if func.body.is_empty() {
                    continue;
                }
                let calls = Self::find_extern_calls_in_func(func, &extern_names);
                if calls.is_empty() {
                    continue;
                }
                self.solver.push();
                let vars = self.setup_ffi_func_vars(func);
                self.assert_func_requires(func, &vars);

                for (extern_name, args) in &calls {
                    if let Some(extern_func) = externs.get(extern_name.as_str()) {
                        let result = self.check_extern_call(
                            &func.name, extern_func, args, &vars,
                        );
                        results.push(result);
                    }
                }
                self.solver.pop(1);
            }
        }
        results
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

    fn find_extern_calls_in_func(func: &FuncDef, extern_names: &HashSet<String>) -> Vec<(String, Vec<Expr>)> {
        let mut calls = Vec::new();
        Self::find_extern_calls_in_block(&func.body, extern_names, &mut calls);
        calls
    }

    fn find_extern_calls_in_block(
        block: &[Stmt], extern_names: &HashSet<String>, calls: &mut Vec<(String, Vec<Expr>)>,
    ) {
        for stmt in block {
            match stmt {
                Stmt::Expr(e) | Stmt::Return(Some(e)) => {
                    Self::find_extern_calls_in_expr(e, extern_names, calls);
                }
                Stmt::If { then_, else_, .. } => {
                    Self::find_extern_calls_in_block(then_, extern_names, calls);
                    if let Some(else_block) = else_ {
                        Self::find_extern_calls_in_block(else_block, extern_names, calls);
                    }
                }
                Stmt::While { body, .. } | Stmt::For { body, .. } => {
                    Self::find_extern_calls_in_block(body, extern_names, calls);
                }
                Stmt::Block(inner) | Stmt::Arena(inner) | Stmt::Unsafe(inner) | Stmt::Parasteps(inner) => {
                    Self::find_extern_calls_in_block(inner, extern_names, calls);
                }
                Stmt::Let { init: Some(init), .. } | Stmt::Assign { value: init, .. } => {
                    Self::find_extern_calls_in_expr(init, extern_names, calls);
                }
                Stmt::SharedLet { init, .. } => {
                    Self::find_extern_calls_in_expr(init, extern_names, calls);
                }
                _ => {}
            }
        }
    }

    fn find_extern_calls_in_expr(
        expr: &Expr, extern_names: &HashSet<String>, calls: &mut Vec<(String, Vec<Expr>)>,
    ) {
        match expr {
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    if extern_names.contains(name.as_str()) {
                        calls.push((name.clone(), args.clone()));
                        return;
                    }
                }
                for arg in args {
                    Self::find_extern_calls_in_expr(arg, extern_names, calls);
                }
            }
            Expr::Binary(_, lhs, rhs) => {
                Self::find_extern_calls_in_expr(lhs, extern_names, calls);
                Self::find_extern_calls_in_expr(rhs, extern_names, calls);
            }
            Expr::Unary(_, inner) => {
                Self::find_extern_calls_in_expr(inner, extern_names, calls);
            }
            Expr::If { cond, then_, else_ } => {
                Self::find_extern_calls_in_expr(cond, extern_names, calls);
                Self::find_extern_calls_in_block(then_, extern_names, calls);
                if let Some(else_block) = else_ {
                    Self::find_extern_calls_in_block(else_block, extern_names, calls);
                }
            }
            Expr::Field(inner, _)
            | Expr::Index(inner, _)
            | Expr::Try(inner)
            | Expr::Spawn(inner)
            | Expr::Await(inner)
            | Expr::Old(inner) => {
                Self::find_extern_calls_in_expr(inner, extern_names, calls);
            }
            Expr::Tuple(items) | Expr::List(items) => {
                for item in items {
                    Self::find_extern_calls_in_expr(item, extern_names, calls);
                }
            }
            Expr::Block(block) => {
                Self::find_extern_calls_in_block(block, extern_names, calls);
            }
            _ => {}
        }
    }

    fn setup_ffi_func_vars(&mut self, func: &FuncDef) -> Z3VarMap {
        let mut vars = Z3VarMap::new();
        for p in &func.params {
            if matches!(&p.ty, Type::Name(n, _) if n == "f64") {
                vars.insert_real(p.name.as_str(), Z3Real::new_const(p.name.as_str()));
            } else if matches!(&p.ty, Type::Name(n, _) if n == "string") {
                vars.insert_int(p.name.as_str(), Z3Int::new_const(p.name.as_str()));
                vars.insert_string_nonempty(p.name.as_str(), Z3Bool::new_const(format!("{}_ne", p.name)));
            } else {
                vars.insert_int(p.name.as_str(), Z3Int::new_const(p.name.as_str()));
            }
        }
        vars
    }

    fn assert_func_requires(&mut self, func: &FuncDef, vars: &Z3VarMap) {
        for stmt in &func.body {
            if let Stmt::Requires(expr, _) = stmt {
                if let Some(z3_bool) = self.expr_to_z3_bool(expr, vars) {
                    self.solver.assert(&z3_bool);
                }
            }
        }
    }

    fn check_extern_call(
        &mut self, caller_name: &str, extern_func: &ExternFunc, args: &[Expr], vars: &Z3VarMap,
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
                };
            }
        };

        let substituted = substitute_args(requires, &extern_func.params, args);

        let z3_requires = match self.expr_to_z3_bool(&substituted, vars) {
            Some(z) => z,
            None => {
                return VerificationResult {
                    func_name,
                    status: VerifStatus::Unknown,
                    message: "could not encode precondition in Z3".into(),
                    diagnostic: None,
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count: 1,
                };
            }
        };

        self.solver.push();
        self.solver.assert(&z3_requires.not());
        let constraint_count = 1;

        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.solver.check())) {
            Ok(SatResult::Unsat) => {
                self.solver.pop(1);
                VerificationResult {
                    func_name,
                    status: VerifStatus::Verified,
                    message: "precondition always satisfied".into(),
                    diagnostic: None,
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count,
                }
            }
            Ok(SatResult::Sat) => {
                self.solver.pop(1);
                let diag = Diagnostic::error(
                    format!(
                        "call to extern '{}' may violate precondition: {:?}",
                        extern_func.name, requires,
                    ),
                    Span::single(0, 0),
                );
                VerificationResult {
                    func_name,
                    status: VerifStatus::Failed,
                    message: "precondition may be violated".into(),
                    diagnostic: Some(diag),
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count,
                }
            }
            Ok(SatResult::Unknown) => {
                self.solver.pop(1);
                VerificationResult {
                    func_name,
                    status: VerifStatus::Unknown,
                    message: "precondition satisfiability unknown".into(),
                    diagnostic: None,
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count,
                }
            }
            Err(_) => {
                self.solver.pop(1);
                VerificationResult {
                    func_name,
                    status: VerifStatus::Unknown,
                    message: "verification timed out or crashed".into(),
                    diagnostic: None,
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count,
                }
            }
        }
    }
}

fn substitute_args(expr: &Expr, params: &[ExternParam], args: &[Expr]) -> Expr {
    if params.len() != args.len() {
        return expr.clone();
    }
    match expr {
        Expr::Ident(name) => {
            if let Some(idx) = params.iter().position(|p| p.name == *name) {
                if idx < args.len() {
                    return args[idx].clone();
                }
            }
            Expr::Ident(name.clone())
        }
        Expr::Binary(op, lhs, rhs) => {
            Expr::Binary(
                *op,
                Box::new(substitute_args(lhs, params, args)),
                Box::new(substitute_args(rhs, params, args)),
            )
        }
        Expr::Unary(op, inner) => {
            Expr::Unary(*op, Box::new(substitute_args(inner, params, args)))
        }
        Expr::Call(callee, callee_args) => {
            Expr::Call(
                Box::new(substitute_args(callee, params, args)),
                callee_args.iter().map(|a| substitute_args(a, params, args)).collect(),
            )
        }
        Expr::Field(inner, name) => {
            Expr::Field(Box::new(substitute_args(inner, params, args)), name.clone())
        }
        Expr::Index(target, index) => {
            Expr::Index(
                Box::new(substitute_args(target, params, args)),
                Box::new(substitute_args(index, params, args)),
            )
        }
        Expr::If { cond, then_, else_ } => {
            Expr::If {
                cond: Box::new(substitute_args(cond, params, args)),
                then_: then_.iter().map(|s| substitute_args_in_stmt(s, params, args)).collect(),
                else_: else_.as_ref().map(|b| b.iter().map(|s| substitute_args_in_stmt(s, params, args)).collect()),
            }
        }
        Expr::Old(inner) => {
            Expr::Old(Box::new(substitute_args(inner, params, args)))
        }
        Expr::Tuple(items) => {
            Expr::Tuple(items.iter().map(|i| substitute_args(i, params, args)).collect())
        }
        Expr::List(items) => {
            Expr::List(items.iter().map(|i| substitute_args(i, params, args)).collect())
        }
        Expr::Block(block) => {
            Expr::Block(block.iter().map(|s| substitute_args_in_stmt(s, params, args)).collect())
        }
        Expr::Literal(_) => expr.clone(),
        _ => expr.clone(),
    }
}

fn substitute_args_in_stmt(stmt: &Stmt, params: &[ExternParam], args: &[Expr]) -> Stmt {
    match stmt {
        Stmt::Expr(e) => Stmt::Expr(substitute_args(e, params, args)),
        Stmt::Return(e) => Stmt::Return(e.as_ref().map(|e| substitute_args(e, params, args))),
        Stmt::Let { pat, ty, init, mut_, ref_ } => {
            Stmt::Let {
                pat: pat.clone(),
                ty: ty.clone(),
                init: init.as_ref().map(|e| substitute_args(e, params, args)),
                mut_: *mut_,
                ref_: *ref_,
            }
        }
        Stmt::If { cond, then_, else_ } => {
            Stmt::If {
                cond: substitute_args(cond, params, args),
                then_: then_.iter().map(|s| substitute_args_in_stmt(s, params, args)).collect(),
                else_: else_.as_ref().map(|b| b.iter().map(|s| substitute_args_in_stmt(s, params, args)).collect()),
            }
        }
        Stmt::Assign { target, value } => {
            Stmt::Assign {
                target: substitute_args(target, params, args),
                value: substitute_args(value, params, args),
            }
        }
        _ => stmt.clone(),
    }
}
