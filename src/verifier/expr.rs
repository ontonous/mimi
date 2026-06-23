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
                    // Encode as rational using string representation for full precision.
                    // Use a high-precision denominator (10^15) to minimize rounding error.
                    const PRECISION: f64 = 1_000_000_000_000_000.0;
                    let scaled = (*f * PRECISION).round() as i64;
                    Some(
                        Z3Real::from_int(&Z3Int::from_i64(scaled))
                            / Z3Real::from_int(&Z3Int::from_i64(PRECISION as i64)),
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
                if vars.get_int(&key).is_some() {
                    Some(vars.get_int(&key).unwrap().ne(&Z3Int::from_i64(0)))
                } else if vars.get_real(&key).is_some() {
                    Some(vars.get_real(&key).unwrap().ne(&Z3Real::from_int(&Z3Int::from_i64(0))))
                } else {
                    let fresh = vars.get_or_create_int(&key);
                    Some(fresh.ne(&Z3Int::from_i64(0)))
                }
            }
            Expr::TupleIndex(obj, idx) => {
                let base = self.field_var_name(obj);
                let key = format!("{}_t{}", base, idx);
                if vars.get_int(&key).is_some() {
                    Some(vars.get_int(&key).unwrap().ne(&Z3Int::from_i64(0)))
                } else if vars.get_real(&key).is_some() {
                    Some(vars.get_real(&key).unwrap().ne(&Z3Real::from_int(&Z3Int::from_i64(0))))
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
            _ => false,
        }
    }

    /// Build a deterministic Z3 variable key for a function call expression.
    /// Uses the function name and field-var-name of each argument to create
    /// a unique key, so the same call with the same args maps to the same
    /// Z3 variable (functional consistency within a provedure).
    fn call_var_key(&self, name: &str, args: &[Expr]) -> String {
        let mut parts = vec![format!("call_{}", name)];
        for a in args {
            parts.push(self.field_var_name(a));
        }
        parts.join("_")
    }
}
