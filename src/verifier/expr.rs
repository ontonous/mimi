use crate::ast::*;
use crate::verifier::ctx::Z3VarMap;
use crate::verifier::helpers::{block_tail_expr, extract_string_empty_cmp, is_string_empty_cmp};
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};

impl crate::verifier::Verifier {
    /// Encode an expression as a Z3 Int term.
    /// May create field access variables on-the-fly when encountering Expr::Field.
    pub(crate) fn expr_to_z3_int(&mut self, expr: &Expr, vars: &mut Z3VarMap) -> Option<Z3Int> {
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
            Expr::Field(obj, field) => {
                let base = self.field_var_name(obj);
                let key = format!("{}_{}", base, field);
                Some(vars.get_or_create_int(&key))
            }
            Expr::TupleIndex(obj, idx) => {
                let base = self.field_var_name(obj);
                let key = format!("{}_t{}", base, idx);
                Some(vars.get_or_create_int(&key))
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
                let else_z3 = else_
                    .as_ref()
                    .and_then(|b| block_tail_expr(b))
                    .and_then(|e| self.expr_to_z3_int(&e, vars))?;
                Some(cond_z3.ite(&then_z3, &else_z3))
            }
            Expr::Block(stmts) => {
                block_tail_expr(stmts)
                    .and_then(|e| self.expr_to_z3_int(&e, vars))
            }
            Expr::Match(expr, arms) => {
                let matched = self.expr_to_z3_int(expr, vars)?;
                self.encode_match_int(&matched, arms, vars)
            }
            Expr::Call(callee, call_args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    // Special-case len(s) — returns the string length variable.
                    if name == "len" && call_args.len() == 1 {
                        if let Expr::Ident(s) = &call_args[0] {
                            if let Some(len_var) = vars.get_string_len(s) {
                                return Some(len_var.clone());
                            }
                            // Fallback: try call_var_key for consistency.
                        }
                    }
                    let call_key = self.call_var_key(name, call_args);
                    Some(vars.get_or_create_int(&call_key))
                } else {
                    None
                }
            }
            Expr::Spawn(inner) => self.expr_to_z3_int(inner, vars),
            Expr::Await(inner) => self.expr_to_z3_int(inner, vars),
            _ => None,
        }
    }

    /// Convert an expression to a Z3 variable name for field/identity access.
    /// Handles nested identities (e.g. p.x -> "p", old(p).x -> "old_p").
    fn field_var_name(&self, expr: &Expr) -> String {
        match expr {
            Expr::Ident(name) => name.clone(),
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    format!("old_{}", name)
                } else {
                    format!("old_{}", self.field_var_name(inner))
                }
            }
            Expr::Field(obj, field) => {
                format!("{}_{}", self.field_var_name(obj), field)
            }
            _ => format!("_{:?}", expr),
        }
    }

    pub(crate) fn expr_to_z3_real(&mut self, expr: &Expr, vars: &mut Z3VarMap) -> Option<Z3Real> {
        match expr {
            Expr::Literal(Lit::Int(n)) => Some(Z3Real::from_int(&Z3Int::from_i64(*n))),
            Expr::Literal(Lit::Float(f)) => {
                if *f == 0.0 {
                    Some(Z3Real::from_int(&Z3Int::from_i64(0)))
                } else if f.is_infinite() || f.is_nan() {
                    None
                } else {
                    // Encode as exact rational via Z3Real::from_rational_str(num, den).
                    // Uses f64::to_string() which produces the shortest decimal
                    // representation that uniquely identifies the float value.
                    // This avoids the i64 overflow from the old PRECISION scaling
                    // approach (which capped out around |f| > 9e3).
                    self.float_to_z3_real(*f)
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
            Expr::Field(obj, field) => {
                let base = self.field_var_name(obj);
                let key = format!("{}_{}", base, field);
                if let Some(v) = vars.get_real(&key) {
                    Some(v.clone())
                } else if let Some(v) = vars.get_int(&key) {
                    Some(Z3Real::from_int(v))
                } else {
                    Some(vars.get_or_create_real(&key))
                }
            }
            Expr::TupleIndex(obj, idx) => {
                let base = self.field_var_name(obj);
                let key = format!("{}_t{}", base, idx);
                if let Some(v) = vars.get_real(&key) {
                    Some(v.clone())
                } else if let Some(v) = vars.get_int(&key) {
                    Some(Z3Real::from_int(v))
                } else {
                    Some(vars.get_or_create_real(&key))
                }
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
                let else_z3 = else_
                    .as_ref()
                    .and_then(|b| block_tail_expr(b))
                    .and_then(|e| self.expr_to_z3_real(&e, vars))?;
                Some(cond_z3.ite(&then_z3, &else_z3))
            }
            Expr::Block(stmts) => {
                block_tail_expr(stmts)
                    .and_then(|e| self.expr_to_z3_real(&e, vars))
            }
            Expr::Match(expr, arms) => {
                let matched = self.expr_to_z3_real(expr, vars)?;
                self.encode_match_real(&matched, arms, vars)
            }
            Expr::Call(callee, call_args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    // Special-case len(s) for string length in real context.
                    if name == "len" && call_args.len() == 1 {
                        if let Expr::Ident(s) = &call_args[0] {
                            if let Some(len_var) = vars.get_string_len(s) {
                                return Some(Z3Real::from_int(len_var));
                            }
                        }
                    }
                    let call_key = self.call_var_key(name, call_args);
                    if let Some(v) = vars.get_real(&call_key) {
                        Some(v.clone())
                    } else {
                        Some(vars.get_or_create_real(&call_key))
                    }
                } else {
                    None
                }
            }
            Expr::Spawn(inner) => self.expr_to_z3_real(inner, vars),
            Expr::Await(inner) => self.expr_to_z3_real(inner, vars),
            _ => None,
        }
    }

    pub(crate) fn expr_to_z3_bool(&mut self, expr: &Expr, vars: &mut Z3VarMap) -> Option<Z3Bool> {
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
            Expr::Field(obj, field) => {
                let base = self.field_var_name(obj);
                let key = format!("{}_{}", base, field);
                if let Some(v) = vars.get_int(&key) {
                    Some(v.ne(&Z3Int::from_i64(0)))
                } else if let Some(v) = vars.get_real(&key) {
                    Some(v.ne(&Z3Real::from_int(&Z3Int::from_i64(0))))
                } else {
                    let fresh = vars.get_or_create_int(&key);
                    Some(fresh.ne(&Z3Int::from_i64(0)))
                }
            }
            Expr::TupleIndex(obj, idx) => {
                let base = self.field_var_name(obj);
                let key = format!("{}_t{}", base, idx);
                if let Some(v) = vars.get_int(&key) {
                    Some(v.ne(&Z3Int::from_i64(0)))
                } else if let Some(v) = vars.get_real(&key) {
                    Some(v.ne(&Z3Real::from_int(&Z3Int::from_i64(0))))
                } else {
                    let fresh = vars.get_or_create_int(&key);
                    Some(fresh.ne(&Z3Int::from_i64(0)))
                }
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
                let else_z3 = else_
                    .as_ref()
                    .and_then(|b| block_tail_expr(b))
                    .and_then(|e| self.expr_to_z3_bool(&e, vars))?;
                Some(cond_z3.ite(&then_z3, &else_z3))
            }
            Expr::Block(stmts) => {
                block_tail_expr(stmts)
                    .and_then(|e| self.expr_to_z3_bool(&e, vars))
            }
            Expr::Match(expr, arms) => {
                let matched = self.expr_to_z3_int(expr, vars)?;
                self.encode_match_bool(&matched, arms, vars)
            }
            Expr::Call(callee, call_args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    // Special-case len(s) for string length in bool context.
                    if name == "len" && call_args.len() == 1 {
                        if let Expr::Ident(s) = &call_args[0] {
                            if let Some(len_var) = vars.get_string_len(s) {
                                return Some(len_var.ne(&Z3Int::from_i64(0)));
                            }
                        }
                    }
                    let call_key = self.call_var_key(name, call_args);
                    if let Some(v) = vars.get_int(&call_key) {
                        Some(v.ne(&Z3Int::from_i64(0)))
                    } else {
                        let fresh = vars.get_or_create_int(&call_key);
                        Some(fresh.ne(&Z3Int::from_i64(0)))
                    }
                } else {
                    None
                }
            }
            Expr::Spawn(inner) => self.expr_to_z3_bool(inner, vars),
            Expr::Await(inner) => self.expr_to_z3_bool(inner, vars),
            _ => None,
        }
    }

    pub(crate) fn is_real_expr(&self, expr: &Expr, vars: &Z3VarMap) -> bool {
        match expr {
            Expr::Ident(name) => vars.is_real(name),
            Expr::Literal(Lit::Float(_)) => true,
            Expr::Old(inner) => {
                if let Expr::Ident(name) = inner.as_ref() {
                    let old_name = format!("old_{}", name);
                    vars.is_real(&old_name)
                } else {
                    // Handle old(p.x) — use field_var_name for nested access
                    let old_name = format!("old_{}", self.field_var_name(inner));
                    vars.is_real(&old_name)
                }
            }
            Expr::Field(obj, field) => {
                let key = format!("{}_{}", self.field_var_name(obj), field);
                vars.is_real(&key)
            }
            Expr::TupleIndex(obj, idx) => {
                let key = format!("{}_t{}", self.field_var_name(obj), idx);
                vars.is_real(&key)
            }
            Expr::Binary(_, lhs, rhs) => {
                self.is_real_expr(lhs, vars) || self.is_real_expr(rhs, vars)
            }
            Expr::Unary(_, inner) => self.is_real_expr(inner, vars),
            Expr::Block(stmts) => {
                block_tail_expr(stmts).map_or(false, |e| self.is_real_expr(&e, vars))
            }
            Expr::Match(expr, arms) => {
                if self.is_real_expr(expr, vars) { true }
                else { arms.iter().any(|a| self.is_real_expr(&a.body, vars)) }
            }
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    if name == "len" {
                        return false; // len() always returns int
                    }
                    args.iter().any(|a| self.is_real_expr(a, vars))
                } else {
                    false
                }
            }
            Expr::Spawn(inner) => self.is_real_expr(inner, vars),
            Expr::Await(inner) => self.is_real_expr(inner, vars),
            _ => false,
        }
    }

    /// Build a deterministic Z3 variable key for a function call expression.
    /// Uses the function name and field-var-name of each argument to create
    /// a unique key, so the same call with the same args maps to the same
    /// Z3 variable (functional consistency within a provedure).
    pub(crate) fn call_var_key(&self, name: &str, args: &[Expr]) -> String {
        let mut parts = vec![format!("call_{}", name)];
        for a in args {
            parts.push(self.field_var_name(a));
        }
        parts.join("_")
    }

    /// Encode an f64 value as an exact Z3 rational using string representation.
    /// Uses Rust's standard f64-to-string conversion which produces the shortest
    /// decimal that uniquely identifies the float value, then parses it as a
    /// rational (num_str / 10^precision). This avoids i64 overflow from the
    /// previous PRECISION-scaling approach.
    /// Encode a pattern match condition: returns a Z3 boolean that is true
    /// when the pattern matches the given encoded matched term.
    fn pattern_matches_z3(&mut self, matched: &Z3Int, pat: &Pattern, vars: &mut Z3VarMap) -> Option<Z3Bool> {
        match pat {
            Pattern::Wildcard => Some(Z3Bool::from_bool(true)),
            Pattern::Variable(_) => Some(Z3Bool::from_bool(true)),
            Pattern::Literal(Lit::Int(n)) => Some(matched.eq(&Z3Int::from_i64(*n))),
            Pattern::Literal(Lit::Bool(b)) => {
                let b_int = Z3Int::from_i64(if *b { 1 } else { 0 });
                Some(matched.eq(&b_int))
            }
            _ => None, // Constructor, Tuple, etc. not yet supported
        }
    }

    /// Build a Z3 ite chain for match expression with int result type.
    /// Each arm is guarded by its pattern condition, building nested ite.
    fn encode_match_int(
        &mut self,
        matched: &Z3Int,
        arms: &[MatchArm],
        vars: &mut Z3VarMap,
    ) -> Option<Z3Int> {
        let mut result: Option<Z3Int> = None;
        for (i, arm) in arms.iter().rev().enumerate() {
            let arm_val = self.expr_to_z3_int(&arm.body, vars)?;
            // Last arm in reverse = first match arm (most specific).
            // If it's a Wildcard, it's also the default — just use its value.
            if i == 0 && matches!(arm.pat, Pattern::Wildcard) {
                result = Some(arm_val);
                continue;
            }
            let base_cond = self.pattern_matches_z3(matched, &arm.pat, vars)?;
            let cond = if let Some(ref guard_expr) = arm.guard {
                if let Some(g) = self.expr_to_z3_bool(guard_expr, vars) {
                    Z3Bool::and(&[&base_cond, &g])
                } else {
                    return None;
                }
            } else {
                base_cond
            };
            result = Some(match result {
                Some(prev) => cond.ite(&arm_val, &prev),
                None => cond.ite(&arm_val, &Z3Int::from_i64(0)),
            });
        }
        result
    }

    /// Build a Z3 ite chain for match expression with real result type.
    fn encode_match_real(
        &mut self,
        matched: &Z3Real,
        arms: &[MatchArm],
        vars: &mut Z3VarMap,
    ) -> Option<Z3Real> {
        let matched_int = Z3Int::from_i64(0);
        let mut result: Option<Z3Real> = None;
        for (i, arm) in arms.iter().rev().enumerate() {
            let arm_val = self.expr_to_z3_real(&arm.body, vars)?;
            if i == 0 && matches!(arm.pat, Pattern::Wildcard) {
                result = Some(arm_val);
                continue;
            }
            let base_cond = if let Pattern::Literal(Lit::Float(f)) = &arm.pat {
                if let Some(f_lit) = self.float_to_z3_real(*f) {
                    matched.eq(&f_lit)
                } else {
                    return None;
                }
            } else {
                let int_cond = self.pattern_matches_z3(&matched_int, &arm.pat, vars)?;
                int_cond
            };
            let cond = if let Some(ref guard_expr) = arm.guard {
                if let Some(g) = self.expr_to_z3_bool(guard_expr, vars) {
                    Z3Bool::and(&[&base_cond, &g])
                } else {
                    return None;
                }
            } else {
                base_cond
            };
            result = Some(match result {
                Some(prev) => cond.ite(&arm_val, &prev),
                None => cond.ite(&arm_val, &Z3Real::from_int(&Z3Int::from_i64(0))),
            });
        }
        result
    }

    /// Build a Z3 ite chain for match expression with bool result type.
    fn encode_match_bool(
        &mut self,
        matched: &Z3Int,
        arms: &[MatchArm],
        vars: &mut Z3VarMap,
    ) -> Option<Z3Bool> {
        let mut result: Option<Z3Bool> = None;
        for (i, arm) in arms.iter().rev().enumerate() {
            let arm_val = self.expr_to_z3_bool(&arm.body, vars)?;
            if i == 0 && matches!(arm.pat, Pattern::Wildcard) {
                result = Some(arm_val);
                continue;
            }
            let base_cond = self.pattern_matches_z3(matched, &arm.pat, vars)?;
            let cond = if let Some(ref guard_expr) = arm.guard {
                if let Some(g) = self.expr_to_z3_bool(guard_expr, vars) {
                    Z3Bool::and(&[&base_cond, &g])
                } else {
                    return None;
                }
            } else {
                base_cond
            };
            result = Some(match result {
                Some(prev) => cond.ite(&arm_val, &prev),
                None => cond.ite(&arm_val, &Z3Bool::from_bool(false)),
            });
        }
        result
    }

    fn float_to_z3_real(&self, f: f64) -> Option<Z3Real> {
        if f == 0.0 {
            return Some(Z3Real::from_int(&Z3Int::from_i64(0)));
        }
        if f.is_infinite() || f.is_nan() {
            return None;
        }
        // Use to_string() for shortest unique decimal representation.
        let s = format!("{}", f);
        if let Some(dot) = s.find('.') {
            let num_str: String = s.chars().filter(|&c| c != '.').collect();
            let precision = s.len() - dot - 1;
            let den_str = format!("1{}", "0".repeat(precision));
            Z3Real::from_rational_str(&num_str, &den_str)
        } else {
            // Integer-valued float: use integer directly (no overflow from precise ints).
            Z3Real::from_rational_str(&s, "1")
        }
    }
}
