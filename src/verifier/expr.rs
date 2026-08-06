use crate::ast::*;
use crate::verifier::ctx::Z3VarMap;
use crate::verifier::helpers::{block_tail_expr, extract_string_empty_cmp, is_string_empty_cmp};
use std::str::FromStr;
use z3::ast::String as Z3String;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};

/// Encode an expression as a Z3 Int term.
/// May create field access variables on-the-fly when encountering Expr::Field.
///
/// Values use unbounded Z3 Int terms. Checked machine-integer definedness is
/// proved separately by `i32_definedness_obligations`, before a value equation
/// may be used to prove a postcondition.
pub(crate) fn expr_to_z3_int(expr: &Expr, vars: &mut Z3VarMap) -> Option<Z3Int> {
    match expr.unlocated() {
        Expr::Literal(Lit::Int(n)) => Some(Z3Int::from_i64(*n)),
        Expr::Ident(name) => vars.get_int(name).cloned(),
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.unlocated() {
                let old_name = format!("old.{}", name);
                return vars.get_int(&old_name).cloned();
            }
            None
        }
        Expr::Field(obj, field) => {
            let base = field_var_name(obj);
            let key = format!("{}.{}", base, field);
            Some(vars.get_or_create_int(&key))
        }
        Expr::TupleIndex(obj, idx) => {
            let base = field_var_name(obj);
            let key = format!("{}[{}]", base, idx);
            Some(vars.get_or_create_int(&key))
        }
        Expr::Binary(op, lhs, rhs) => {
            let l = expr_to_z3_int(lhs, vars)?;
            let r = expr_to_z3_int(rhs, vars)?;
            match op {
                BinOp::Add => Some(Z3Int::add(&[&l, &r])),
                BinOp::Sub => Some(Z3Int::sub(&[&l, &r])),
                BinOp::Mul => Some(Z3Int::mul(&[&l, &r])),
                BinOp::Div => {
                    // C1: Z3's `div` uses Euclidean division (floor), but
                    // C/LLVM uses truncation (toward zero). Encode truncation:
                    //   trunc_div(a,b) = let aa = abs(a), ab = abs(b);
                    //   abs_q = aa / ab;  (positive-only, Euclidean = truncation)
                    //   result = (a>=0)==(b>=0) ? abs_q : -abs_q
                    let zero = Z3Int::from_i64(0);
                    let aa = l.ge(&zero).ite(&l, &l.unary_minus());
                    let ab = r.ge(&zero).ite(&r, &r.unary_minus());
                    let abs_q = aa.div(&ab);
                    let same_sign = l.ge(&zero).eq(&r.ge(&zero));
                    Some(same_sign.ite(&abs_q, &abs_q.unary_minus()))
                }
                BinOp::Mod => {
                    // C1: Z3's `modulo` is also Euclidean. Encode C truncation
                    // modulo: trunc_mod(a,b) = a - trunc_div(a,b) * b
                    // But this creates a circular dependency. Instead use:
                    // trunc_mod(a,b) = a - trunc_div(a,b) * b
                    // where trunc_div is defined above.
                    // For a standalone encoding:
                    //   let abs_r = aa % ab;  (positive-only)
                    //   result = a>=0 ? abs_r : -abs_r
                    let zero = Z3Int::from_i64(0);
                    let aa = l.ge(&zero).ite(&l, &l.unary_minus());
                    let ab = r.ge(&zero).ite(&r, &r.unary_minus());
                    let abs_mod = aa.modulo(&ab);
                    Some(l.ge(&zero).ite(&abs_mod, &abs_mod.unary_minus()))
                }
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
            if let Expr::Ident(name) = callee.unlocated() {
                // Special-case len(s) — returns the string or list length variable.
                if name == "len" && call_args.len() == 1 {
                    if let Expr::Ident(s) = call_args[0].unlocated() {
                        if let Some(len_var) = vars.get_string_len(s) {
                            return Some(len_var.clone());
                        }
                        // Fallback for list params: len(xs) → list_len[xs]
                        if let Some(len_var) = vars.get_list_len(s) {
                            return Some(len_var.clone());
                        }
                    }
                    // len(sort(xs)) → list_len[xs] (sort preserves length)
                    if let Expr::Call(callee2, args2) = call_args[0].unlocated() {
                        if let Expr::Ident(name2) = callee2.unlocated() {
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

pub(crate) struct IntDefinedness {
    pub(crate) condition: Z3Bool,
    pub(crate) failure: &'static str,
}

/// Collect the definedness VCs for checked i32 arithmetic in evaluation order.
/// The value terms remain mathematical Ints so C-style truncating division is
/// preserved; each intermediate result must independently fit in i32.
pub(crate) fn i32_definedness_obligations(
    expr: &Expr,
    vars: &mut Z3VarMap,
) -> Option<Vec<IntDefinedness>> {
    let mut obligations = Vec::new();
    collect_i32_definedness(expr, vars, &mut obligations)?;
    Some(obligations)
}

fn collect_i32_definedness(
    expr: &Expr,
    vars: &mut Z3VarMap,
    obligations: &mut Vec<IntDefinedness>,
) -> Option<()> {
    match expr.unlocated() {
        Expr::Binary(op, lhs, rhs) => {
            collect_i32_definedness(lhs, vars, obligations)?;
            collect_i32_definedness(rhs, vars, obligations)?;
            let l = expr_to_z3_int(lhs, vars)?;
            let r = expr_to_z3_int(rhs, vars)?;
            match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul => {
                    let result = match op {
                        BinOp::Add => Z3Int::add(&[&l, &r]),
                        BinOp::Sub => Z3Int::sub(&[&l, &r]),
                        BinOp::Mul => Z3Int::mul(&[&l, &r]),
                        _ => unreachable!(
                            "checked_int_overflow: only Add/Sub/Mul are checked for i32 overflow"
                        ),
                    };
                    let lo = Z3Int::from_i64(i32::MIN as i64);
                    let hi = Z3Int::from_i64(i32::MAX as i64);
                    obligations.push(IntDefinedness {
                        condition: Z3Bool::and(&[&result.ge(&lo), &result.le(&hi)]),
                        failure: "integer overflow is not excluded by preconditions",
                    });
                }
                BinOp::Div | BinOp::Mod => {
                    let zero = Z3Int::from_i64(0);
                    let min = Z3Int::from_i64(i32::MIN as i64);
                    let neg_one = Z3Int::from_i64(-1);
                    let min_overflow = Z3Bool::and(&[&l.eq(&min), &r.eq(&neg_one)]);
                    obligations.push(IntDefinedness {
                        condition: Z3Bool::and(&[&r.ne(&zero), &min_overflow.not()]),
                        failure: "integer operation is undefined (zero divisor or MIN / -1)",
                    });
                }
                _ => {}
            }
        }
        Expr::Unary(UnOp::Neg, inner) => {
            collect_i32_definedness(inner, vars, obligations)?;
            let value = expr_to_z3_int(inner, vars)?;
            let min = Z3Int::from_i64(i32::MIN as i64);
            obligations.push(IntDefinedness {
                condition: value.ne(&min),
                failure: "integer overflow is not excluded by preconditions",
            });
        }
        Expr::If { cond, then_, else_ } => {
            let condition = expr_to_z3_bool(cond, vars)?;
            if let Some(then_expr) = block_tail_expr(then_) {
                let mut branch = i32_definedness_obligations(&then_expr, vars)?;
                for obligation in &mut branch {
                    obligation.condition = condition.implies(&obligation.condition);
                }
                obligations.extend(branch);
            }
            if let Some(else_expr) = else_.as_ref().and_then(|block| block_tail_expr(block)) {
                let mut branch = i32_definedness_obligations(&else_expr, vars)?;
                let else_condition = condition.not();
                for obligation in &mut branch {
                    obligation.condition = else_condition.implies(&obligation.condition);
                }
                obligations.extend(branch);
            }
        }
        Expr::Block(stmts) => {
            if let Some(tail) = block_tail_expr(stmts) {
                collect_i32_definedness(&tail, vars, obligations)?;
            }
        }
        Expr::Match(scrutinee, arms) => {
            // V-1 (audit 2026-08-05): this arm was missing entirely — a
            // division inside a match arm body generated no obligation and
            // `ensures` that hold under Z3's uninterpreted `div x 0` verified
            // Proven while the runtime traps with E0801.
            collect_i32_definedness(scrutinee, vars, obligations)?;
            // The scrutinee term drives pattern-condition gating; when it is
            // not int-encodable, arm obligations fall back to unconditional
            // (conservative — never weaker than the runtime semantics).
            let matched = expr_to_z3_int(scrutinee, vars);
            for arm in arms {
                let pattern_cond = matched
                    .as_ref()
                    .and_then(|m| pattern_matches_z3(m, &arm.pat, vars));

                // The guard is evaluated whenever the pattern matched, so its
                // definedness is gated by the pattern condition alone.
                if let Some(guard) = &arm.guard {
                    let mut guard_obligations = Vec::new();
                    collect_i32_definedness(guard, vars, &mut guard_obligations)?;
                    for obligation in &mut guard_obligations {
                        if let Some(pc) = &pattern_cond {
                            obligation.condition = pc.implies(&obligation.condition);
                        }
                    }
                    obligations.extend(guard_obligations);
                }

                // The body runs only when the pattern matched AND the guard
                // (if any) evaluated true.
                let mut body_obligations = Vec::new();
                collect_i32_definedness(&arm.body, vars, &mut body_obligations)?;
                let guard_cond = arm.guard.as_ref().and_then(|g| expr_to_z3_bool(g, vars));
                for obligation in &mut body_obligations {
                    let mut antecedents: Vec<&Z3Bool> = Vec::new();
                    if let Some(pc) = &pattern_cond {
                        antecedents.push(pc);
                    }
                    if let Some(gc) = &guard_cond {
                        antecedents.push(gc);
                    }
                    if !antecedents.is_empty() {
                        obligation.condition =
                            Z3Bool::and(&antecedents).implies(&obligation.condition);
                    }
                }
                obligations.extend(body_obligations);
            }
        }
        Expr::Call(callee, call_args) => {
            // V-1 (audit 2026-08-05): this arm was missing entirely — a
            // division inside a call argument generated no obligation
            // (arguments are evaluated before the call).
            collect_i32_definedness(callee, vars, obligations)?;
            for arg in call_args {
                collect_i32_definedness(arg, vars, obligations)?;
            }
        }
        Expr::Field(obj, _) | Expr::TupleIndex(obj, _) => {
            collect_i32_definedness(obj, vars, obligations)?;
        }
        // Note: Lambda bodies intentionally generate no obligations here —
        // their divisions execute in the (unknown) higher-order call context,
        // not at lambda construction. Modeling that requires HOF semantics the
        // AST path does not have (conservative gap, same as pre-audit).
        Expr::Spawn(inner) | Expr::Await(inner) => {
            collect_i32_definedness(inner, vars, obligations)?;
        }
        _ => {}
    }
    Some(())
}

/// Convert an expression to a Z3 variable name for field/identity access.
/// Handles nested identities (e.g. p.x -> "p", old(p).x -> "old.p").
fn field_var_name(expr: &Expr) -> String {
    match expr.unlocated() {
        Expr::Ident(name) => name.clone(),
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.unlocated() {
                format!("old.{}", name)
            } else {
                format!("old.{}", field_var_name(inner))
            }
        }
        Expr::Field(obj, field) => {
            format!("{}.{}", field_var_name(obj), field)
        }
        _ => format!("_{:?}", expr),
    }
}

/// 0.31.28: Check if an expression is f64-typed (Float literal or f64 variable).
/// Used to reject f64 arithmetic in the AST path (NotInTrustedSubset).
///
/// H-23 (audit 2026-08-05): recognition is RECURSIVE, mirroring
/// `is_real_expr`. The leaf-only version let composite f64 expressions
/// (match/if/block tails, call results, nested fields) bypass the P0-2
/// rejection guard in `expr_to_z3_bool` and get encoded as exact Z3 Reals
/// (`invariant` statements force the AST path, where this guard is the only
/// f64 defense). Any composite whose sub-expression is f64 is treated as f64.
fn is_f64_expr(expr: &Expr, vars: &Z3VarMap) -> bool {
    match expr.unlocated() {
        Expr::Literal(Lit::Float(_)) => true,
        Expr::Ident(name) => {
            // A variable is f64 if it's in the Real map but NOT in the Int map.
            // (i32 variables are in the Int map; f64 variables are in the Real map.)
            vars.get_real(name).is_some() && vars.get_int(name).is_none()
        }
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.unlocated() {
                let old_name = format!("old.{}", name);
                vars.get_real(&old_name).is_some() && vars.get_int(&old_name).is_none()
            } else {
                // old(p.x) — mirror is_real_expr's nested-access handling.
                let old_name = format!("old.{}", field_var_name(inner));
                vars.is_real(&old_name)
            }
        }
        Expr::Field(obj, field) => {
            let key = format!("{}.{}", field_var_name(obj), field);
            vars.is_real(&key)
        }
        Expr::TupleIndex(obj, idx) => {
            let key = format!("{}[{}]", field_var_name(obj), idx);
            vars.is_real(&key)
        }
        Expr::Binary(_, lhs, rhs) => is_f64_expr(lhs, vars) || is_f64_expr(rhs, vars),
        Expr::Unary(_, inner) => is_f64_expr(inner, vars),
        Expr::Block(stmts) => block_tail_expr(stmts).is_some_and(|e| is_f64_expr(&e, vars)),
        // Beyond is_real_expr (which has no If arm): closing the If tail here
        // pre-empts the exact-Real encoding of f64 values wrapped in if-exprs.
        Expr::If { then_, else_, .. } => {
            block_tail_expr(then_).is_some_and(|e| is_f64_expr(&e, vars))
                || else_
                    .as_ref()
                    .and_then(|b| block_tail_expr(b))
                    .is_some_and(|e| is_f64_expr(&e, vars))
        }
        Expr::Match(expr, arms) => {
            is_f64_expr(expr, vars) || arms.iter().any(|a| is_f64_expr(&a.body, vars))
        }
        Expr::Call(callee, args) => {
            if let Expr::Ident(name) = callee.unlocated() {
                if name == "len" {
                    return false; // len() always returns int
                }
                args.iter().any(|a| is_f64_expr(a, vars))
            } else {
                false
            }
        }
        Expr::Spawn(inner) | Expr::Await(inner) => is_f64_expr(inner, vars),
        _ => false,
    }
}

/// Encode an expression as a Z3 Real (for i32 values only).
///
/// 0.31.28: f64 arithmetic returns None (NotInTrustedSubset).
/// 0.31.29: f64 comparisons also return None (NotInTrustedSubset) in the AST path.
/// The VIR path handles f64 correctly with opaque sort + F64Compare.
///
/// This function is now sound: only i32 values (exact integers) are encoded
/// as Z3 Reals. All f64 paths return None → NotInTrustedSubset.
pub(crate) fn expr_to_z3_real(expr: &Expr, vars: &mut Z3VarMap) -> Option<Z3Real> {
    match expr.unlocated() {
        Expr::Literal(Lit::Int(n)) => Some(Z3Real::from_int(&Z3Int::from_i64(*n))),
        Expr::Literal(Lit::Float(f)) => {
            // 0.31.28: f64 literals are NOT in the trusted subset for arithmetic.
            // Only 0.0 is exact (encodes to Z3 zero). All other f64 literals
            // return None → NotInTrustedSubset.
            if *f == 0.0 {
                Some(Z3Real::from_int(&Z3Int::from_i64(0)))
            } else {
                None
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
            if let Expr::Ident(name) = inner.unlocated() {
                let old_name = format!("old.{}", name);
                if let Some(v) = vars.get_real(&old_name) {
                    return Some(v.clone());
                }
                return vars.get_int(&old_name).map(Z3Real::from_int);
            }
            None
        }
        Expr::Field(obj, field) => {
            let base = field_var_name(obj);
            let key = format!("{}.{}", base, field);
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
            let key = format!("{}[{}]", base, idx);
            if let Some(v) = vars.get_real(&key) {
                Some(v.clone())
            } else if let Some(v) = vars.get_int(&key) {
                Some(Z3Real::from_int(v))
            } else {
                Some(vars.get_or_create_real(&key))
            }
        }
        Expr::Binary(op, lhs, rhs) => {
            // 0.31.28: f64 arithmetic is NOT in the trusted subset.
            // Check if either operand is f64 (Float literal or f64 variable).
            let lhs_is_f64 = is_f64_expr(lhs, vars);
            let rhs_is_f64 = is_f64_expr(rhs, vars);
            if lhs_is_f64 || rhs_is_f64 {
                // f64 arithmetic → NotInTrustedSubset
                return None;
            }
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
            // 0.31.28: f64 negation is NOT in the trusted subset.
            if is_f64_expr(inner, vars) {
                return None;
            }
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
            if let Expr::Ident(name) = callee.unlocated() {
                // Special-case len(s) for string length in real context.
                if name == "len" && call_args.len() == 1 {
                    if let Expr::Ident(s) = call_args[0].unlocated() {
                        if let Some(len_var) = vars.get_string_len(s) {
                            return Some(Z3Real::from_int(len_var));
                        }
                        // Fallback for list params: len(xs) → list_len[xs]
                        if let Some(len_var) = vars.get_list_len(s) {
                            return Some(Z3Real::from_int(len_var));
                        }
                    }
                    // len(sort(xs)) → list_len[xs] (sort preserves length)
                    if let Expr::Call(callee2, args2) = call_args[0].unlocated() {
                        if let Expr::Ident(name2) = callee2.unlocated() {
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
    match expr.unlocated() {
        Expr::Literal(Lit::Bool(b)) => Some(Z3Bool::from_bool(*b)),
        Expr::Ident(name) => {
            // RT-H6 (audit): try string nonempty lookup before falling
            // back to int/real. String variables are encoded as
            // Z3Bool (nonempty) or Z3String; do not treat them as
            // "int != 0" which is type-unsound.
            // V-H5: prefer dedicated bool vars over int!=0 encoding.
            if let Some(v) = vars.get_bool(name) {
                return Some(v.clone());
            }
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
            if let Expr::Ident(name) = inner.unlocated() {
                let old_name = format!("old.{}", name);
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
            let key = format!("{}.{}", base, field);
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
            let key = format!("{}[{}]", base, idx);
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

            // 0.31.29 audit P0-2: f64 comparisons must NOT be encoded as exact
            // Z3 Reals (IEEE 754 rounding is not modeled). The VIR path handles
            // f64 comparisons correctly with F64Compare (uninterpreted predicate).
            // In the AST path, reject f64 comparisons → NotInTrustedSubset.
            let lhs_is_f64 = is_f64_expr(lhs, vars);
            let rhs_is_f64 = is_f64_expr(rhs, vars);
            if lhs_is_f64 || rhs_is_f64 {
                return None;
            }

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
            if let Expr::Ident(name) = callee.unlocated() {
                // Special-case len(s) for string length in bool context.
                if name == "len" && call_args.len() == 1 {
                    if let Expr::Ident(s) = call_args[0].unlocated() {
                        if let Some(len_var) = vars.get_string_len(s) {
                            return Some(len_var.ne(Z3Int::from_i64(0)));
                        }
                        // Fallback for list params: len(xs) → list_len[xs]
                        if let Some(len_var) = vars.get_list_len(s) {
                            return Some(len_var.ne(Z3Int::from_i64(0)));
                        }
                    }
                    // len(sort(xs)) → list_len[xs] (sort preserves length)
                    if let Expr::Call(callee2, args2) = call_args[0].unlocated() {
                        if let Expr::Ident(name2) = callee2.unlocated() {
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
    match expr.unlocated() {
        Expr::Ident(name) => vars.is_real(name),
        Expr::Literal(Lit::Float(_)) => true,
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.unlocated() {
                let old_name = format!("old.{}", name);
                vars.is_real(&old_name)
            } else {
                // Handle old(p.x) — use field_var_name for nested access
                let old_name = format!("old.{}", field_var_name(inner));
                vars.is_real(&old_name)
            }
        }
        Expr::Field(obj, field) => {
            let key = format!("{}.{}", field_var_name(obj), field);
            vars.is_real(&key)
        }
        Expr::TupleIndex(obj, idx) => {
            let key = format!("{}[{}]", field_var_name(obj), idx);
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
            if let Expr::Ident(name) = callee.unlocated() {
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
///
/// §11-#37 (audit 2026-08-05, residual): parts are joined with `#`, NOT
/// `_`. Underscore joins were ambiguous: `f(a_b, c)` and `f(a, b_c)`
/// produced the identical key `call_f_a_b_c`, aliasing two distinct call
/// results into one Z3 variable — a callee's ensures proven for one call
/// then became an axiom for the other (cross-call fake Proven). `#` is
/// outside the identifier charset `[A-Za-z0-9_]`, so no user parameter or
/// field path can ever collide with a generated key.
pub(crate) fn call_var_key(name: &str, args: &[Expr]) -> String {
    let mut parts = vec![format!("call_{}", name)];
    for a in args {
        parts.push(field_var_name(a));
    }
    parts.join("#")
}

/// Encode an f64 value as an exact Z3 rational using string representation.
/// Uses Rust's standard f64-to-string conversion which produces the shortest
/// decimal that uniquely identifies the float value, then parses it as a
/// rational (num_str / 10^precision). This avoids i64 overflow from the
/// previous PRECISION-scaling approach.
/// Encode a pattern match condition: returns a Z3 boolean that is true
/// when the pattern matches the given encoded matched term.
fn pattern_matches_z3(matched: &Z3Int, pat: &Pattern, _vars: &mut Z3VarMap) -> Option<Z3Bool> {
    match &pat.kind {
        PatternKind::Wildcard => Some(Z3Bool::from_bool(true)),
        PatternKind::Variable(_) => Some(Z3Bool::from_bool(true)),
        PatternKind::Literal(Lit::Int(n)) => Some(matched.eq(Z3Int::from_i64(*n))),
        PatternKind::Literal(Lit::Bool(b)) => {
            let b_int = Z3Int::from_i64(if *b { 1 } else { 0 });
            Some(matched.eq(&b_int))
        }
        _ => None, // Constructor, Tuple, etc. not yet supported
    }
}

/// Build a Z3 ite chain for match expression with int result type.
/// Each arm is guarded by its pattern condition, building nested ite.
fn encode_match_int(matched: &Z3Int, arms: &[MatchArm], vars: &mut Z3VarMap) -> Option<Z3Int> {
    let has_wildcard = arms
        .iter()
        .any(|a| matches!(&a.pat.kind, PatternKind::Wildcard));
    let mut result: Option<Z3Int> = None;
    for (i, arm) in arms.iter().rev().enumerate() {
        let arm_val = expr_to_z3_int(&arm.body, vars)?;
        // Last arm in reverse = first match arm (most specific).
        // If it's a Wildcard, it's also the default — just use its value.
        if i == 0 && matches!(&arm.pat.kind, PatternKind::Wildcard) {
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
        if matches!(
            &arm.pat.kind,
            PatternKind::Wildcard | PatternKind::Variable(_)
        ) {
            result = Some(arm_val);
            continue;
        }
        let base_cond = if let PatternKind::Literal(Lit::Float(f)) = &arm.pat.kind {
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
        if i == 0 && matches!(&arm.pat.kind, PatternKind::Wildcard) {
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
        // AU-C1: avoid `f64 * 1e18 as i64` overflow (debug panic / wrong constraint).
        // Encode via string split on 'e'/'E' into mantissa/exponent rationals.
        let (mant, exp) = if let Some(idx) = s.find(['e', 'E']) {
            let mant = &s[..idx];
            let exp: i32 = s[idx + 1..].parse().unwrap_or(0);
            (mant.to_string(), exp)
        } else {
            (s.clone(), 0)
        };
        // Strip sign from mantissa for digit handling.
        let (sign, mant_digits) = if let Some(rest) = mant.strip_prefix('-') {
            ("-", rest)
        } else if let Some(rest) = mant.strip_prefix('+') {
            ("", rest)
        } else {
            ("", mant.as_str())
        };
        let mant_digits = if mant_digits.is_empty() {
            "0"
        } else {
            mant_digits
        };
        if exp >= 0 {
            let zeros = "0".repeat(exp as usize);
            let num = format!("{}{}{}", sign, mant_digits, zeros);
            Z3Real::from_rational_str(&num, "1")
        } else {
            let den = format!("1{}", "0".repeat((-exp) as usize));
            let num = format!("{}{}", sign, mant_digits);
            Z3Real::from_rational_str(&num, &den)
        }
    } else {
        // Integer-valued float: use integer directly (no overflow from precise ints).
        Z3Real::from_rational_str(&s, "1")
    }
}

/// Resolve an expression to a Z3 string variable for string theory encoding.
/// Handles `Ident`, `Literal("...")`, `old(ident)`, and `char_at(s, i)`.
fn resolve_string_expr(expr: &Expr, vars: &mut Z3VarMap) -> Option<Z3String> {
    match expr.unlocated() {
        Expr::Ident(name) => vars.get_string_var(name).cloned(),
        Expr::Literal(Lit::String(s)) => Z3String::from_str(s).ok(),
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.unlocated() {
                // §11-#37: dot separator for namespace consistency.
                let old_name = format!("old.{}", name);
                vars.get_string_var(&old_name).cloned()
            } else {
                None
            }
        }
        // V4: Support field paths like p.name in string operations.
        Expr::Field(obj, field) => {
            let key = format!("{}.{}", field_var_name(obj), field);
            vars.get_string_var(&key).cloned()
        }
        Expr::Call(callee, args) => {
            if let Expr::Ident(name) = callee.unlocated() {
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
    match expr.unlocated() {
        Expr::Ident(name) => vars.get_list_len(name).cloned(),
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.unlocated() {
                let old_name = format!("old.{}", name);
                vars.get_list_len(&old_name).cloned()
            } else {
                None
            }
        }
        Expr::Call(callee, args) => {
            if let Expr::Ident(name) = callee.unlocated() {
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
