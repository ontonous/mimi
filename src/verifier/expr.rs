use crate::ast::*;
use crate::verifier::ctx::Z3VarMap;
use crate::verifier::helpers::{block_tail_expr, extract_string_empty_cmp, is_string_empty_cmp};
use std::str::FromStr;
use z3::ast::String as Z3String;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};

/// Encode an expression as a Z3 Int term.
/// May create field access variables on-the-fly when encountering Expr::Field.
pub(crate) fn expr_to_z3_int(expr: &Expr, vars: &mut Z3VarMap) -> Option<Z3Int> {
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
            let base = field_var_name(obj);
            let key = format!("{}_{}", base, field);
            Some(vars.get_or_create_int(&key))
        }
        Expr::TupleIndex(obj, idx) => {
            let base = field_var_name(obj);
            let key = format!("{}_t{}", base, idx);
            Some(vars.get_or_create_int(&key))
        }
        Expr::Binary(op, lhs, rhs) => {
            let l = expr_to_z3_int(lhs, vars)?;
            let r = expr_to_z3_int(rhs, vars)?;
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
            let v = expr_to_z3_int(inner, vars)?;
            Some(v.unary_minus())
        }
        Expr::If { cond, then_, else_ } => {
            let cond_z3 = expr_to_z3_bool(cond, vars)?;
            let then_z3 = block_tail_expr(then_).and_then(|e| expr_to_z3_int(&e, vars))?;
            let else_z3 = else_
                .as_ref()
                .and_then(|b| block_tail_expr(b))
                .and_then(|e| expr_to_z3_int(&e, vars))?;
            Some(cond_z3.ite(&then_z3, &else_z3))
        }
        Expr::Block(stmts) => block_tail_expr(stmts).and_then(|e| expr_to_z3_int(&e, vars)),
        Expr::Match(expr, arms) => {
            let matched = expr_to_z3_int(expr, vars)?;
            encode_match_int(&matched, arms, vars)
        }
        Expr::Call(callee, call_args) => {
            if let Expr::Ident(name) = callee.as_ref() {
                // Special-case len(s) — returns the string or list length variable.
                if name == "len" && call_args.len() == 1 {
                    if let Expr::Ident(s) = &call_args[0] {
                        if let Some(len_var) = vars.get_string_len(s) {
                            return Some(len_var.clone());
                        }
                        // Fallback for list params: len(xs) → list_len[xs]
                        if let Some(len_var) = vars.get_list_len(s) {
                            return Some(len_var.clone());
                        }
                    }
                    // len(sort(xs)) → list_len[xs] (sort preserves length)
                    if let Expr::Call(callee2, args2) = &call_args[0] {
                        if let Expr::Ident(name2) = callee2.as_ref() {
                            if (name2 == "sort" || name2 == "reverse") && args2.len() == 1 {
                                if let Some(list_len) = resolve_list_len(&args2[0], vars) {
                                    return Some(list_len.clone());
                                }
                            }
                        }
                    }
                }
                // sort() and reverse() preserve list length: len(result) == len(input)
                if (name == "sort" || name == "reverse") && call_args.len() == 1 {
                    if let Some(list_len) = resolve_list_len(&call_args[0], vars) {
                        return Some(list_len.clone());
                    }
                }
                let call_key = call_var_key(name, call_args);
                Some(vars.get_or_create_int(&call_key))
            } else {
                None
            }
        }
        Expr::Spawn(inner) => expr_to_z3_int(inner, vars),
        Expr::Await(inner) => expr_to_z3_int(inner, vars),
        _ => None,
    }
}

/// Convert an expression to a Z3 variable name for field/identity access.
/// Handles nested identities (e.g. p.x -> "p", old(p).x -> "old_p").
fn field_var_name(expr: &Expr) -> String {
    match expr {
        Expr::Ident(name) => name.clone(),
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.as_ref() {
                format!("old_{}", name)
            } else {
                format!("old_{}", field_var_name(inner))
            }
        }
        Expr::Field(obj, field) => {
            format!("{}_{}", field_var_name(obj), field)
        }
        _ => format!("_{:?}", expr),
    }
}

pub(crate) fn expr_to_z3_real(expr: &Expr, vars: &mut Z3VarMap) -> Option<Z3Real> {
    match expr {
        Expr::Literal(Lit::Int(n)) => Some(Z3Real::from_int(&Z3Int::from_i64(*n))),
        Expr::Literal(Lit::Float(f)) => {
            if *f == 0.0 {
                // -0.0 and 0.0 both encode to the same Z3 value (zero).
                // This is mathematically correct (0.0 == -0.0 in IEEE 754
                // except for the sign bit), so no special handling needed.
                Some(Z3Real::from_int(&Z3Int::from_i64(0)))
            } else if f.is_infinite() || f.is_nan() {
                None
            } else {
                // Encode as exact rational via Z3Real::from_rational_str(num, den).
                // Uses f64::to_string() which produces the shortest decimal
                // representation that uniquely identifies the float value.
                // This avoids the i64 overflow from the old PRECISION scaling
                // approach (which capped out around |f| > 9e3).
                float_to_z3_real(*f)
            }
        }
        Expr::Ident(name) => {
            if let Some(v) = vars.get_real(name) {
                Some(v.clone())
            } else {
                vars.get_int(name).map(Z3Real::from_int)
            }
        }
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.as_ref() {
                let old_name = format!("old_{}", name);
                if let Some(v) = vars.get_real(&old_name) {
                    return Some(v.clone());
                }
                return vars.get_int(&old_name).map(Z3Real::from_int);
            }
            None
        }
        Expr::Field(obj, field) => {
            let base = field_var_name(obj);
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
            let base = field_var_name(obj);
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
            let l = expr_to_z3_real(lhs, vars)?;
            let r = expr_to_z3_real(rhs, vars)?;
            match op {
                BinOp::Add => Some(l + r),
                BinOp::Sub => Some(l - r),
                BinOp::Mul => Some(l * r),
                BinOp::Div => Some(l / r),
                _ => None,
            }
        }
        Expr::Unary(UnOp::Neg, inner) => {
            let v = expr_to_z3_real(inner, vars)?;
            Some(-v)
        }
        Expr::If { cond, then_, else_ } => {
            let cond_z3 = expr_to_z3_bool(cond, vars)?;
            let then_z3 = block_tail_expr(then_).and_then(|e| expr_to_z3_real(&e, vars))?;
            let else_z3 = else_
                .as_ref()
                .and_then(|b| block_tail_expr(b))
                .and_then(|e| expr_to_z3_real(&e, vars))?;
            Some(cond_z3.ite(&then_z3, &else_z3))
        }
        Expr::Block(stmts) => block_tail_expr(stmts).and_then(|e| expr_to_z3_real(&e, vars)),
        Expr::Match(expr, arms) => {
            let matched = expr_to_z3_real(expr, vars)?;
            encode_match_real(&matched, arms, vars)
        }
        Expr::Call(callee, call_args) => {
            if let Expr::Ident(name) = callee.as_ref() {
                // Special-case len(s) for string length in real context.
                if name == "len" && call_args.len() == 1 {
                    if let Expr::Ident(s) = &call_args[0] {
                        if let Some(len_var) = vars.get_string_len(s) {
                            return Some(Z3Real::from_int(len_var));
                        }
                        // Fallback for list params: len(xs) → list_len[xs]
                        if let Some(len_var) = vars.get_list_len(s) {
                            return Some(Z3Real::from_int(len_var));
                        }
                    }
                    // len(sort(xs)) → list_len[xs] (sort preserves length)
                    if let Expr::Call(callee2, args2) = &call_args[0] {
                        if let Expr::Ident(name2) = callee2.as_ref() {
                            if (name2 == "sort" || name2 == "reverse") && args2.len() == 1 {
                                if let Some(list_len) = resolve_list_len(&args2[0], vars) {
                                    return Some(Z3Real::from_int(&list_len));
                                }
                            }
                        }
                    }
                }
                let call_key = call_var_key(name, call_args);
                if let Some(v) = vars.get_real(&call_key) {
                    Some(v.clone())
                } else {
                    Some(vars.get_or_create_real(&call_key))
                }
            } else {
                None
            }
        }
        Expr::Spawn(inner) => expr_to_z3_real(inner, vars),
        Expr::Await(inner) => expr_to_z3_real(inner, vars),
        _ => None,
    }
}

pub(crate) fn expr_to_z3_bool(expr: &Expr, vars: &mut Z3VarMap) -> Option<Z3Bool> {
    match expr {
        Expr::Literal(Lit::Bool(b)) => Some(Z3Bool::from_bool(*b)),
        Expr::Ident(name) => {
            // RT-H6 (audit): try string nonempty lookup before falling
            // back to int/real. String variables are encoded as
            // Z3Bool (nonempty) or Z3String; do not treat them as
            // "int != 0" which is type-unsound.
            if let Some(v) = vars.get_string_nonempty(name) {
                return Some(v.clone());
            }
            vars.get_int(name)
                .map(|v| v.ne(Z3Int::from_i64(0)))
                .or_else(|| {
                    vars.get_real(name)
                        .map(|v| v.ne(Z3Real::from_int(&Z3Int::from_i64(0))))
                })
        }
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.as_ref() {
                let old_name = format!("old_{}", name);
                // RT-H6: check string nonempty for old(string) expressions.
                if let Some(v) = vars.get_string_nonempty(&old_name) {
                    return Some(v.clone());
                }
                if let Some(v) = vars.get_int(&old_name) {
                    return Some(v.ne(Z3Int::from_i64(0)));
                }
                if let Some(v) = vars.get_real(&old_name) {
                    return Some(v.ne(Z3Real::from_int(&Z3Int::from_i64(0))));
                }
            }
            None
        }
        Expr::Field(obj, field) => {
            let base = field_var_name(obj);
            let key = format!("{}_{}", base, field);
            if let Some(v) = vars.get_int(&key) {
                Some(v.ne(Z3Int::from_i64(0)))
            } else if let Some(v) = vars.get_real(&key) {
                Some(v.ne(Z3Real::from_int(&Z3Int::from_i64(0))))
            } else {
                let fresh = vars.get_or_create_int(&key);
                Some(fresh.ne(Z3Int::from_i64(0)))
            }
        }
        Expr::TupleIndex(obj, idx) => {
            let base = field_var_name(obj);
            let key = format!("{}_t{}", base, idx);
            if let Some(v) = vars.get_int(&key) {
                Some(v.ne(Z3Int::from_i64(0)))
            } else if let Some(v) = vars.get_real(&key) {
                Some(v.ne(Z3Real::from_int(&Z3Int::from_i64(0))))
            } else {
                let fresh = vars.get_or_create_int(&key);
                Some(fresh.ne(Z3Int::from_i64(0)))
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

            let use_real = is_real_expr(lhs, vars) || is_real_expr(rhs, vars);

            match op {
                BinOp::EqCmp if use_real => {
                    let l = expr_to_z3_real(lhs, vars)?;
                    let r = expr_to_z3_real(rhs, vars)?;
                    Some(l.eq(&r))
                }
                BinOp::NeCmp if use_real => {
                    let l = expr_to_z3_real(lhs, vars)?;
                    let r = expr_to_z3_real(rhs, vars)?;
                    Some(l.eq(&r).not())
                }
                BinOp::Lt if use_real => {
                    let l = expr_to_z3_real(lhs, vars)?;
                    let r = expr_to_z3_real(rhs, vars)?;
                    Some(l.lt(&r))
                }
                BinOp::Gt if use_real => {
                    let l = expr_to_z3_real(lhs, vars)?;
                    let r = expr_to_z3_real(rhs, vars)?;
                    Some(l.gt(&r))
                }
                BinOp::Le if use_real => {
                    let l = expr_to_z3_real(lhs, vars)?;
                    let r = expr_to_z3_real(rhs, vars)?;
                    Some(l.le(&r))
                }
                BinOp::Ge if use_real => {
                    let l = expr_to_z3_real(lhs, vars)?;
                    let r = expr_to_z3_real(rhs, vars)?;
                    Some(l.ge(&r))
                }
                BinOp::EqCmp => {
                    if let Some(s_eq) = encode_string_eq(lhs, rhs, vars) {
                        return Some(s_eq);
                    }
                    let l = expr_to_z3_int(lhs, vars)?;
                    let r = expr_to_z3_int(rhs, vars)?;
                    Some(l.eq(&r))
                }
                BinOp::NeCmp => {
                    if let Some(s_eq) = encode_string_eq(lhs, rhs, vars) {
                        return Some(s_eq.not());
                    }
                    let l = expr_to_z3_int(lhs, vars)?;
                    let r = expr_to_z3_int(rhs, vars)?;
                    Some(l.eq(&r).not())
                }
                BinOp::Lt => {
                    let l = expr_to_z3_int(lhs, vars)?;
                    let r = expr_to_z3_int(rhs, vars)?;
                    Some(l.lt(&r))
                }
                BinOp::Gt => {
                    let l = expr_to_z3_int(lhs, vars)?;
                    let r = expr_to_z3_int(rhs, vars)?;
                    Some(l.gt(&r))
                }
                BinOp::Le => {
                    let l = expr_to_z3_int(lhs, vars)?;
                    let r = expr_to_z3_int(rhs, vars)?;
                    Some(l.le(&r))
                }
                BinOp::Ge => {
                    let l = expr_to_z3_int(lhs, vars)?;
                    let r = expr_to_z3_int(rhs, vars)?;
                    Some(l.ge(&r))
                }
                BinOp::And => {
                    let l = expr_to_z3_bool(lhs, vars)?;
                    let r = expr_to_z3_bool(rhs, vars)?;
                    Some(Z3Bool::and(&[&l, &r]))
                }
                BinOp::Or => {
                    let l = expr_to_z3_bool(lhs, vars)?;
                    let r = expr_to_z3_bool(rhs, vars)?;
                    Some(Z3Bool::or(&[&l, &r]))
                }
                _ => None,
            }
        }
        Expr::Unary(UnOp::Not, inner) => {
            let v = expr_to_z3_bool(inner, vars)?;
            Some(v.not())
        }
        Expr::If { cond, then_, else_ } => {
            let cond_z3 = expr_to_z3_bool(cond, vars)?;
            let then_z3 = block_tail_expr(then_).and_then(|e| expr_to_z3_bool(&e, vars))?;
            let else_z3 = else_
                .as_ref()
                .and_then(|b| block_tail_expr(b))
                .and_then(|e| expr_to_z3_bool(&e, vars))?;
            Some(cond_z3.ite(&then_z3, &else_z3))
        }
        Expr::Block(stmts) => block_tail_expr(stmts).and_then(|e| expr_to_z3_bool(&e, vars)),
        Expr::Match(expr, arms) => {
            let matched = expr_to_z3_int(expr, vars)?;
            encode_match_bool(&matched, arms, vars)
        }
        Expr::Call(callee, call_args) => {
            if let Expr::Ident(name) = callee.as_ref() {
                // Special-case len(s) for string length in bool context.
                if name == "len" && call_args.len() == 1 {
                    if let Expr::Ident(s) = &call_args[0] {
                        if let Some(len_var) = vars.get_string_len(s) {
                            return Some(len_var.ne(Z3Int::from_i64(0)));
                        }
                        // Fallback for list params: len(xs) → list_len[xs]
                        if let Some(len_var) = vars.get_list_len(s) {
                            return Some(len_var.ne(Z3Int::from_i64(0)));
                        }
                    }
                    // len(sort(xs)) → list_len[xs] (sort preserves length)
                    if let Expr::Call(callee2, args2) = &call_args[0] {
                        if let Expr::Ident(name2) = callee2.as_ref() {
                            if (name2 == "sort" || name2 == "reverse") && args2.len() == 1 {
                                if let Some(list_len) = resolve_list_len(&args2[0], vars) {
                                    return Some(list_len.ne(Z3Int::from_i64(0)));
                                }
                            }
                        }
                    }
                }
                if name == "contains" && call_args.len() == 2 {
                    if let (Some(s), Some(pat)) = (
                        resolve_string_expr(&call_args[0], vars),
                        resolve_string_expr(&call_args[1], vars),
                    ) {
                        return Some(s.contains(&pat));
                    }
                }
                if name == "starts_with" && call_args.len() == 2 {
                    if let (Some(s), Some(pat)) = (
                        resolve_string_expr(&call_args[0], vars),
                        resolve_string_expr(&call_args[1], vars),
                    ) {
                        return Some(s.prefix(&pat));
                    }
                }
                if name == "ends_with" && call_args.len() == 2 {
                    if let (Some(s), Some(pat)) = (
                        resolve_string_expr(&call_args[0], vars),
                        resolve_string_expr(&call_args[1], vars),
                    ) {
                        return Some(s.suffix(&pat));
                    }
                }
                let call_key = call_var_key(name, call_args);
                if let Some(v) = vars.get_int(&call_key) {
                    Some(v.ne(Z3Int::from_i64(0)))
                } else {
                    let fresh = vars.get_or_create_int(&call_key);
                    Some(fresh.ne(Z3Int::from_i64(0)))
                }
            } else {
                None
            }
        }
        Expr::Spawn(inner) => expr_to_z3_bool(inner, vars),
        Expr::Await(inner) => expr_to_z3_bool(inner, vars),
        _ => None,
    }
}

fn is_real_expr(expr: &Expr, vars: &Z3VarMap) -> bool {
    match expr {
        Expr::Ident(name) => vars.is_real(name),
        Expr::Literal(Lit::Float(_)) => true,
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.as_ref() {
                let old_name = format!("old_{}", name);
                vars.is_real(&old_name)
            } else {
                // Handle old(p.x) — use field_var_name for nested access
                let old_name = format!("old_{}", field_var_name(inner));
                vars.is_real(&old_name)
            }
        }
        Expr::Field(obj, field) => {
            let key = format!("{}_{}", field_var_name(obj), field);
            vars.is_real(&key)
        }
        Expr::TupleIndex(obj, idx) => {
            let key = format!("{}_t{}", field_var_name(obj), idx);
            vars.is_real(&key)
        }
        Expr::Binary(_, lhs, rhs) => is_real_expr(lhs, vars) || is_real_expr(rhs, vars),
        Expr::Unary(_, inner) => is_real_expr(inner, vars),
        Expr::Block(stmts) => block_tail_expr(stmts).is_some_and(|e| is_real_expr(&e, vars)),
        Expr::Match(expr, arms) => {
            if is_real_expr(expr, vars) {
                true
            } else {
                arms.iter().any(|a| is_real_expr(&a.body, vars))
            }
        }
        Expr::Call(callee, args) => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == "len" {
                    return false; // len() always returns int
                }
                args.iter().any(|a| is_real_expr(a, vars))
            } else {
                false
            }
        }
        Expr::Spawn(inner) => is_real_expr(inner, vars),
        Expr::Await(inner) => is_real_expr(inner, vars),
        _ => false,
    }
}

/// Build a deterministic Z3 variable key for a function call expression.
/// Uses the function name and field-var-name of each argument to create
/// a unique key, so the same call with the same args maps to the same
/// Z3 variable (functional consistency within a provedure).
pub(crate) fn call_var_key(name: &str, args: &[Expr]) -> String {
    let mut parts = vec![format!("call_{}", name)];
    for a in args {
        parts.push(field_var_name(a));
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
fn pattern_matches_z3(matched: &Z3Int, pat: &Pattern, _vars: &mut Z3VarMap) -> Option<Z3Bool> {
    match pat {
        Pattern::Wildcard => Some(Z3Bool::from_bool(true)),
        Pattern::Variable(_) => Some(Z3Bool::from_bool(true)),
        Pattern::Literal(Lit::Int(n)) => Some(matched.eq(Z3Int::from_i64(*n))),
        Pattern::Literal(Lit::Bool(b)) => {
            let b_int = Z3Int::from_i64(if *b { 1 } else { 0 });
            Some(matched.eq(&b_int))
        }
        _ => None, // Constructor, Tuple, etc. not yet supported
    }
}

/// Build a Z3 ite chain for match expression with int result type.
/// Each arm is guarded by its pattern condition, building nested ite.
fn encode_match_int(matched: &Z3Int, arms: &[MatchArm], vars: &mut Z3VarMap) -> Option<Z3Int> {
    let has_wildcard = arms.iter().any(|a| matches!(a.pat, Pattern::Wildcard));
    let mut result: Option<Z3Int> = None;
    for (i, arm) in arms.iter().rev().enumerate() {
        let arm_val = expr_to_z3_int(&arm.body, vars)?;
        // Last arm in reverse = first match arm (most specific).
        // If it's a Wildcard, it's also the default — just use its value.
        if i == 0 && matches!(arm.pat, Pattern::Wildcard) {
            result = Some(arm_val);
            continue;
        }
        let base_cond = pattern_matches_z3(matched, &arm.pat, vars)?;
        let cond = if let Some(ref guard_expr) = arm.guard {
            if let Some(g) = expr_to_z3_bool(guard_expr, vars) {
                Z3Bool::and(&[&base_cond, &g])
            } else {
                return None;
            }
        } else {
            base_cond
        };
        result = Some(match result {
            Some(prev) => cond.ite(&arm_val, &prev),
            None if has_wildcard => cond.ite(&arm_val, &Z3Int::from_i64(0)),
            None => {
                // E2: Non-exhaustive match with no wildcard — use an
                // unconstrained variable so the verifier doesn't silently
                // assume result == 0 when no arm matches.
                let fallback = vars.get_or_create_int("_match_fallback");
                cond.ite(&arm_val, &fallback)
            }
        });
    }
    result
}

/// Build a Z3 ite chain for match expression with real result type.
fn encode_match_real(matched: &Z3Real, arms: &[MatchArm], vars: &mut Z3VarMap) -> Option<Z3Real> {
    let mut result: Option<Z3Real> = None;
    for arm in arms.iter().rev() {
        let arm_val = expr_to_z3_real(&arm.body, vars)?;
        // Wildcard and Variable patterns always match — directly take arm value.
        // No need to call pattern_matches_z3 with a dummy matched_int = 0.
        if matches!(arm.pat, Pattern::Wildcard | Pattern::Variable(_)) {
            result = Some(arm_val);
            continue;
        }
        let base_cond = if let Pattern::Literal(Lit::Float(f)) = &arm.pat {
            if let Some(f_lit) = float_to_z3_real(*f) {
                matched.eq(&f_lit)
            } else {
                return None;
            }
        } else {
            // For non-float-literal patterns (Constructor, Tuple, etc.),
            // we cannot yet encode the condition — return None.
            return None;
        };
        let cond = if let Some(ref guard_expr) = arm.guard {
            if let Some(g) = expr_to_z3_bool(guard_expr, vars) {
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
fn encode_match_bool(matched: &Z3Int, arms: &[MatchArm], vars: &mut Z3VarMap) -> Option<Z3Bool> {
    let mut result: Option<Z3Bool> = None;
    for (i, arm) in arms.iter().rev().enumerate() {
        let arm_val = expr_to_z3_bool(&arm.body, vars)?;
        if i == 0 && matches!(arm.pat, Pattern::Wildcard) {
            result = Some(arm_val);
            continue;
        }
        let base_cond = pattern_matches_z3(matched, &arm.pat, vars)?;
        let cond = if let Some(ref guard_expr) = arm.guard {
            if let Some(g) = expr_to_z3_bool(guard_expr, vars) {
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

fn float_to_z3_real(f: f64) -> Option<Z3Real> {
    if f == 0.0 {
        return Some(Z3Real::from_int(&Z3Int::from_i64(0)));
    }
    if f.is_infinite() || f.is_nan() {
        return None;
    }
    // CRITICAL #17 fix: format!("{}", f) for scientific notation like
    // "1e-50" or "1e20" does not contain a '.', causing the else branch
    // to pass "1e-50" to from_rational_str which panics. Instead, use
    // a format that always produces decimal notation with a fractional
    // part, and handle the scientific notation case explicitly.
    let s = format!("{}", f);
    if let Some(dot) = s.find('.') {
        let num_str: String = s.chars().filter(|&c| c != '.').collect();
        let precision = s.len() - dot - 1;
        let den_str = format!("1{}", "0".repeat(precision));
        Z3Real::from_rational_str(&num_str, &den_str)
    } else if s.contains('e') || s.contains('E') {
        // Scientific notation without decimal point (e.g. "1e20").
        // Parse and convert to decimal fraction.
        // f is finite and not NaN (checked above), so this is safe.
        let abs_f = f.abs();
        // Multiply by 10^18 to get an integer numerator, then divide.
        // This loses precision for very large/small values, but Z3
        // real encoding is approximate anyway.
        let scaled = (abs_f * 1e18).round() as i64;
        let num = if f < 0.0 { -scaled } else { scaled };
        Z3Real::from_rational_str(&num.to_string(), "1000000000000000000")
    } else {
        // Integer-valued float: use integer directly (no overflow from precise ints).
        Z3Real::from_rational_str(&s, "1")
    }
}

/// Resolve an expression to a Z3 string variable for string theory encoding.
/// Handles `Ident`, `Literal("...")`, `old(ident)`, and `char_at(s, i)`.
fn resolve_string_expr(expr: &Expr, vars: &mut Z3VarMap) -> Option<Z3String> {
    match expr {
        Expr::Ident(name) => vars.get_string_var(name).cloned(),
        Expr::Literal(Lit::String(s)) => Z3String::from_str(s).ok(),
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.as_ref() {
                let old_name = format!("old_{}", name);
                vars.get_string_var(&old_name).cloned()
            } else {
                None
            }
        }
        // V4: Support field paths like p.name in string operations.
        Expr::Field(obj, field) => {
            let key = format!("{}_{}", field_var_name(obj), field);
            vars.get_string_var(&key).cloned()
        }
        Expr::Call(callee, args) => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == "char_at" && args.len() == 2 {
                    let s = resolve_string_expr(&args[0], vars)?;
                    let idx = expr_to_z3_int(&args[1], vars)?;
                    return Some(s.at(&idx));
                }
            }
            None
        }
        _ => None,
    }
}

/// Resolve an expression to a Z3 list-length variable.
/// Handles identity (list param name), sort/reverse (which preserve length),
/// and old() snapshots.
pub(crate) fn resolve_list_len(expr: &Expr, vars: &mut Z3VarMap) -> Option<Z3Int> {
    match expr {
        Expr::Ident(name) => vars.get_list_len(name).cloned(),
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.as_ref() {
                let old_name = format!("old_{}", name);
                vars.get_list_len(&old_name).cloned()
            } else {
                None
            }
        }
        Expr::Call(callee, args) => {
            if let Expr::Ident(name) = callee.as_ref() {
                // sort() and reverse() preserve input list length
                if (name == "sort" || name == "reverse") && args.len() == 1 {
                    return resolve_list_len(&args[0], vars);
                }
            }
            None
        }
        _ => None,
    }
}

/// Encode string equality `lhs == rhs` using Z3 string theory.
/// Returns `None` if either side is not a string expression.
fn encode_string_eq(lhs: &Expr, rhs: &Expr, vars: &mut Z3VarMap) -> Option<Z3Bool> {
    let s1 = resolve_string_expr(lhs, vars)?;
    let s2 = resolve_string_expr(rhs, vars)?;
    Some(s1.eq(&s2))
}
