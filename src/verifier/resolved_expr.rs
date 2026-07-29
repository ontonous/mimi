//! SD-4/Body migration: Z3 encoding for ResolvedExpr (Typed Resolved IR).
//!
//! Parallel to `expr.rs` (raw AST encoding). Consumes `ResolvedExprKind`
//! instead of `Expr`, enabling the verifier to work from CheckedProgram
//! without `legacy_body_file()`.
//!
//! # Mapping (AST → Resolved IR)
//!
//! | AST (`Expr`)              | Resolved IR (`ResolvedExprKind`)       |
//! |---------------------------|----------------------------------------|
//! | `Literal(Lit::Int(n))`    | `Literal(ResolvedLiteral::Int(n))`     |
//! | `Ident(name)`             | `Load(place)` → locals[place.base]     |
//! | `Old(inner)`              | `Old(inner)`                           |
//! | `Field(obj, field)`       | `Project { value, projection }`        |
//! | `Binary(op, lhs, rhs)`    | `Binary { op, left, right }`           |
//! | `Unary(Neg, inner)`       | `Unary { op: Negate, operand }`        |
//! | `If { cond, then_, else_}`| `If { condition, then_block, else_block }` |
//! | `Block(stmts)`            | `Block(block)` → block.result          |
//! | `Match(expr, arms)`       | `Match { scrutinee, arms }`            |
//! | `Call(callee, args)`      | `Call(ResolvedCall)`                   |

use crate::ast::{BinOp, Lit, UnOp};
use crate::core::ir::{
    ResolvedBinaryOp, ResolvedBlock, ResolvedBody, ResolvedExpr, ResolvedExprKind, ResolvedLiteral,
    ResolvedPlace, ResolvedUnaryOp, ResolvedValueProjection,
};
use crate::verifier::ctx::Z3VarMap;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};

/// Resolve a place to its display name for Z3 variable lookup.
fn place_name(place: &ResolvedPlace, body: &ResolvedBody) -> Option<String> {
    body.locals
        .get(&place.base)
        .map(|local| local.display_name.clone())
}

/// Encode a ResolvedExpr as a Z3 Int term.
pub(crate) fn resolved_to_z3_int(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    match &expr.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::Int(n)) => Some(Z3Int::from_i64(*n)),
        ResolvedExprKind::Literal(ResolvedLiteral::Bool(b)) => {
            Some(Z3Int::from_i64(if *b { 1 } else { 0 }))
        }
        ResolvedExprKind::Load(place) => {
            let name = place_name(place, body)?;
            vars.get_int(&name).cloned()
        }
        ResolvedExprKind::Old(inner) => {
            if let ResolvedExprKind::Load(place) = &inner.kind {
                let name = place_name(place, body)?;
                let old_name = format!("old_{}", name);
                return vars.get_int(&old_name).cloned();
            }
            None
        }
        ResolvedExprKind::Project { value, projection } => {
            let base = resolved_field_var_name(value, body);
            let proj_name = match projection {
                ResolvedValueProjection::Field(id) => id.0.clone(),
                ResolvedValueProjection::Tuple(idx) => format!("t{}", idx),
                ResolvedValueProjection::Index(_) => "idx".to_string(),
                ResolvedValueProjection::Dereference => "deref".to_string(),
            };
            let key = format!("{}_{}", base, proj_name);
            Some(vars.get_or_create_int(&key))
        }
        ResolvedExprKind::Binary { op, left, right } => {
            let l = resolved_to_z3_int(left, body, vars)?;
            let r = resolved_to_z3_int(right, body, vars)?;
            match op {
                ResolvedBinaryOp::Add => Some(Z3Int::add(&[&l, &r])),
                ResolvedBinaryOp::Subtract => Some(Z3Int::sub(&[&l, &r])),
                ResolvedBinaryOp::Multiply => Some(Z3Int::mul(&[&l, &r])),
                ResolvedBinaryOp::Divide => {
                    // C1: truncation division (same as AST encoding)
                    let zero = Z3Int::from_i64(0);
                    let aa = l.ge(&zero).ite(&l, &l.unary_minus());
                    let ab = r.ge(&zero).ite(&r, &r.unary_minus());
                    let abs_q = aa.div(&ab);
                    let same_sign = l.ge(&zero).eq(&r.ge(&zero));
                    Some(same_sign.ite(&abs_q, &abs_q.unary_minus()))
                }
                ResolvedBinaryOp::Remainder => {
                    let zero = Z3Int::from_i64(0);
                    let aa = l.ge(&zero).ite(&l, &l.unary_minus());
                    let ab = r.ge(&zero).ite(&r, &r.unary_minus());
                    let abs_mod = aa.modulo(&ab);
                    Some(l.ge(&zero).ite(&abs_mod, &abs_mod.unary_minus()))
                }
                _ => None,
            }
        }
        ResolvedExprKind::Unary { op, operand } => match op {
            ResolvedUnaryOp::Negate => {
                let v = resolved_to_z3_int(operand, body, vars)?;
                Some(v.unary_minus())
            }
            _ => None,
        },
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let cond_z3 = resolved_to_z3_bool(condition, body, vars)?;
            let then_z3 = resolved_block_tail_int(then_block, body, vars)?;
            let else_z3 = resolved_block_tail_int(else_block, body, vars)?;
            Some(cond_z3.ite(&then_z3, &else_z3))
        }
        ResolvedExprKind::Block(block) => resolved_block_tail_int(block, body, vars),
        _ => None,
    }
}

/// Encode a ResolvedExpr as a Z3 Real term.
pub(crate) fn resolved_to_z3_real(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    vars: &mut Z3VarMap,
) -> Option<Z3Real> {
    match &expr.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::FloatBits(bits)) => {
            let val = f64::from_bits(*bits);
            // 0.31.28: f64 literals are NOT in the trusted subset for arithmetic.
            // Only 0.0 is exact. All other f64 literals → None (NotInTrustedSubset).
            if val == 0.0 {
                Some(Z3Real::from_int(&Z3Int::from_i64(0)))
            } else {
                None
            }
        }
        ResolvedExprKind::Literal(ResolvedLiteral::Int(n)) => {
            Some(Z3Real::from_int(&Z3Int::from_i64(*n)))
        }
        ResolvedExprKind::Load(place) => {
            let name = place_name(place, body)?;
            vars.get_real(&name).cloned()
        }
        ResolvedExprKind::Old(inner) => {
            if let ResolvedExprKind::Load(place) = &inner.kind {
                let name = place_name(place, body)?;
                let old_name = format!("old_{}", name);
                return vars.get_real(&old_name).cloned();
            }
            None
        }
        ResolvedExprKind::Binary { op, left, right } => {
            let l = resolved_to_z3_real(left, body, vars)?;
            let r = resolved_to_z3_real(right, body, vars)?;
            match op {
                ResolvedBinaryOp::Add => Some(Z3Real::add(&[&l, &r])),
                ResolvedBinaryOp::Subtract => Some(Z3Real::sub(&[&l, &r])),
                ResolvedBinaryOp::Multiply => Some(Z3Real::mul(&[&l, &r])),
                ResolvedBinaryOp::Divide => Some(l.div(&r)),
                _ => None,
            }
        }
        ResolvedExprKind::Unary { op, operand } => match op {
            ResolvedUnaryOp::Negate => {
                let v = resolved_to_z3_real(operand, body, vars)?;
                Some(v.unary_minus())
            }
            _ => None,
        },
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let cond_z3 = resolved_to_z3_bool(condition, body, vars)?;
            let then_z3 = resolved_block_tail_real(then_block, body, vars)?;
            let else_z3 = resolved_block_tail_real(else_block, body, vars)?;
            Some(cond_z3.ite(&then_z3, &else_z3))
        }
        ResolvedExprKind::Block(block) => resolved_block_tail_real(block, body, vars),
        _ => None,
    }
}

/// Encode a ResolvedExpr as a Z3 Bool term.
pub(crate) fn resolved_to_z3_bool(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    match &expr.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::Bool(b)) => Some(Z3Bool::from_bool(*b)),
        ResolvedExprKind::Load(place) => {
            let name = place_name(place, body)?;
            vars.get_bool(&name).cloned()
        }
        ResolvedExprKind::Binary { op, left, right } => {
            // Try int comparison first, then real, then bool equality
            if let (Some(l), Some(r)) = (
                resolved_to_z3_int(left, body, vars),
                resolved_to_z3_int(right, body, vars),
            ) {
                return match op {
                    ResolvedBinaryOp::Equal => Some(l.eq(&r)),
                    ResolvedBinaryOp::NotEqual => Some(l.eq(&r).not()),
                    ResolvedBinaryOp::Less => Some(l.lt(&r)),
                    ResolvedBinaryOp::Greater => Some(l.gt(&r)),
                    ResolvedBinaryOp::LessEqual => Some(l.le(&r)),
                    ResolvedBinaryOp::GreaterEqual => Some(l.ge(&r)),
                    ResolvedBinaryOp::LogicalAnd => {
                        let lb = resolved_to_z3_bool(left, body, vars)?;
                        let rb = resolved_to_z3_bool(right, body, vars)?;
                        Some(Z3Bool::and(&[&lb, &rb]))
                    }
                    ResolvedBinaryOp::LogicalOr => {
                        let lb = resolved_to_z3_bool(left, body, vars)?;
                        let rb = resolved_to_z3_bool(right, body, vars)?;
                        Some(Z3Bool::or(&[&lb, &rb]))
                    }
                    _ => None,
                };
            }
            // Fall back to real comparison
            if let (Some(l), Some(r)) = (
                resolved_to_z3_real(left, body, vars),
                resolved_to_z3_real(right, body, vars),
            ) {
                return match op {
                    ResolvedBinaryOp::Equal => Some(l.eq(&r)),
                    ResolvedBinaryOp::NotEqual => Some(l.eq(&r).not()),
                    ResolvedBinaryOp::Less => Some(l.lt(&r)),
                    ResolvedBinaryOp::Greater => Some(l.gt(&r)),
                    ResolvedBinaryOp::LessEqual => Some(l.le(&r)),
                    ResolvedBinaryOp::GreaterEqual => Some(l.ge(&r)),
                    _ => None,
                };
            }
            // Fall back to bool equality (P1 fix: bool == bool)
            if let (Some(l), Some(r)) = (
                resolved_to_z3_bool(left, body, vars),
                resolved_to_z3_bool(right, body, vars),
            ) {
                return match op {
                    ResolvedBinaryOp::Equal => Some(l.eq(&r)),
                    ResolvedBinaryOp::NotEqual => Some(l.eq(&r).not()),
                    _ => None,
                };
            }
            None
        }
        ResolvedExprKind::Unary { op, operand } => match op {
            ResolvedUnaryOp::Not => {
                let v = resolved_to_z3_bool(operand, body, vars)?;
                Some(v.not())
            }
            _ => None,
        },
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let cond_z3 = resolved_to_z3_bool(condition, body, vars)?;
            let then_z3 = resolved_block_tail_bool(then_block, body, vars)?;
            let else_z3 = resolved_block_tail_bool(else_block, body, vars)?;
            Some(cond_z3.ite(&then_z3, &else_z3))
        }
        ResolvedExprKind::Block(block) => resolved_block_tail_bool(block, body, vars),
        _ => None,
    }
}

// === Block tail expression helpers ===

fn resolved_block_tail_int(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    block
        .result
        .as_ref()
        .and_then(|e| resolved_to_z3_int(e, body, vars))
}

fn resolved_block_tail_real(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    vars: &mut Z3VarMap,
) -> Option<Z3Real> {
    block
        .result
        .as_ref()
        .and_then(|e| resolved_to_z3_real(e, body, vars))
}

fn resolved_block_tail_bool(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    block
        .result
        .as_ref()
        .and_then(|e| resolved_to_z3_bool(e, body, vars))
}

/// Build a variable name for a field projection (parallel to `field_var_name` in expr.rs).
fn resolved_field_var_name(expr: &ResolvedExpr, body: &ResolvedBody) -> String {
    match &expr.kind {
        ResolvedExprKind::Load(place) => place_name(place, body).unwrap_or_else(|| "_".into()),
        ResolvedExprKind::Project { value, projection } => {
            let proj_name = match projection {
                ResolvedValueProjection::Field(id) => id.0.clone(),
                ResolvedValueProjection::Tuple(idx) => format!("t{}", idx),
                ResolvedValueProjection::Index(_) => "idx".to_string(),
                ResolvedValueProjection::Dereference => "deref".to_string(),
            };
            format!("{}_{}", resolved_field_var_name(value, body), proj_name)
        }
        _ => "_expr".into(),
    }
}

// === Contract verification from Resolved IR ===

/// Z3 type category for parameter variable creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Z3TypeCategory {
    Int,
    Real,
    Bool,
}

/// Determine the Z3 type category from a ResolvedTypeId.
fn z3_type_category(
    ty: &crate::core::ir::ResolvedTypeId,
    types: &crate::core::ir::ResolvedTypeTable,
) -> Z3TypeCategory {
    match types.get(ty) {
        Some(crate::core::ir::ResolvedType::Primitive(p)) => match p {
            crate::core::ir::PrimitiveType::F64 => Z3TypeCategory::Real,
            crate::core::ir::PrimitiveType::Bool => Z3TypeCategory::Bool,
            _ => Z3TypeCategory::Int,
        },
        _ => Z3TypeCategory::Int,
    }
}

/// Create Z3 variables for all parameters in a resolved body.
/// Mirrors the variable creation in `verify_func()` (func.rs:402-487).
fn create_parameter_vars(
    body: &ResolvedBody,
    types: &crate::core::ir::ResolvedTypeTable,
    vars: &mut Z3VarMap,
    session: &mut crate::verifier::ctx::SolverSession,
) {
    for param_id in &body.parameters {
        let Some(local) = body.locals.get(param_id) else {
            continue;
        };
        let name = &local.display_name;
        let category = z3_type_category(&local.ty, types);
        match category {
            Z3TypeCategory::Real => {
                vars.insert_real(name, Z3Real::new_const(name.as_str()));
            }
            Z3TypeCategory::Bool => {
                vars.insert_bool(name, Z3Bool::new_const(name.as_str()));
            }
            Z3TypeCategory::Int => {
                let iv = Z3Int::new_const(name.as_str());
                vars.insert_int(name, iv.clone());
                // V-H4: constrain i32 params to machine range
                if let Some(crate::core::ir::ResolvedType::Primitive(
                    crate::core::ir::PrimitiveType::I32,
                )) = types.get(&local.ty)
                {
                    let lo = Z3Int::from_i64(i32::MIN as i64);
                    let hi = Z3Int::from_i64(i32::MAX as i64);
                    session.solver.assert(iv.ge(&lo));
                    session.solver.assert(iv.le(&hi));
                }
            }
        }
        // Create old_* snapshot variable
        let old_name = format!("old_{}", name);
        match category {
            Z3TypeCategory::Real => {
                vars.insert_real(&old_name, Z3Real::new_const(old_name.as_str()));
            }
            Z3TypeCategory::Bool => {
                vars.insert_bool(&old_name, Z3Bool::new_const(old_name.as_str()));
            }
            Z3TypeCategory::Int => {
                vars.insert_int(&old_name, Z3Int::new_const(old_name.as_str()));
            }
        }
    }
}

/// Check if a callable has math obligations in its body statements.
pub(crate) fn has_math_obligations(callable: &crate::core::ir::ResolvedCallable) -> bool {
    callable
        .body
        .root
        .statements
        .iter()
        .any(|stmt| matches!(stmt.kind, crate::core::ir::ResolvedStmtKind::Math(_)))
}

/// Verify contracts from Resolved IR (ResolvedCallable).
///
/// This is the Resolved IR parallel of `verify_func()` (func.rs).
/// It extracts contracts from `ResolvedCallable.contracts`, creates Z3
/// variables from `ResolvedBody.parameters`, and checks validity.
///
/// # Current coverage (vs AST path)
/// - Requires/ensures: ✅ (int/bool/real)
/// - Math obligations: ✅ (proven from preconditions before ensures)
/// - old() expressions: ✅
/// - Field projections: ✅
/// - Let-substitution: ❌ (deferred)
/// - Call-site requires checking: ❌ (deferred)
/// - Invariant checking: ❌ (deferred)
/// - Callee ensures propagation: ❌ (deferred)
///
/// # Soundness
/// If the body return expression cannot be encoded, `result` is left
/// unconstrained. To prevent false Disproven verdicts from an
/// unconstrained `result`, the function returns `NotInTrustedSubset`
/// whenever body encoding fails and ensures obligations exist.
/// Similarly, any contract encoding failure causes an immediate
/// `NotInTrustedSubset` return, regardless of whether a violation
/// was found (the violation could be an artifact of missing constraints).
pub(crate) fn verify_contracts_from_resolved(
    callable: &crate::core::ir::ResolvedCallable,
    types: &crate::core::ir::ResolvedTypeTable,
    session: &mut crate::verifier::ctx::SolverSession,
) -> Option<crate::verifier::ctx::VerifStatus> {
    use crate::core::ir::ContractKind;
    use z3::SatResult;

    let body = &callable.body;
    let mut vars = Z3VarMap::new();

    // 1. Create parameter variables
    create_parameter_vars(body, types, &mut vars, session);

    // 2. Create result variable with type inferred from signature (P1 fix).
    let result_category = z3_type_category(&callable.signature.result, types);
    match result_category {
        Z3TypeCategory::Real => {
            vars.insert_real("result", Z3Real::new_const("result"));
        }
        Z3TypeCategory::Bool => {
            vars.insert_bool("result", Z3Bool::new_const("result"));
        }
        Z3TypeCategory::Int => {
            let rv = Z3Int::new_const("result");
            vars.insert_int("result", rv.clone());
            // V-H4: constrain i32 result to machine range
            if let Some(crate::core::ir::ResolvedType::Primitive(
                crate::core::ir::PrimitiveType::I32,
            )) = types.get(&callable.signature.result)
            {
                let lo = Z3Int::from_i64(i32::MIN as i64);
                let hi = Z3Int::from_i64(i32::MAX as i64);
                session.solver.assert(rv.ge(&lo));
                session.solver.assert(rv.le(&hi));
            }
        }
    }

    // 3. Assert old(param) == param equalities
    for param_id in &body.parameters {
        let Some(local) = body.locals.get(param_id) else {
            continue;
        };
        let name = &local.display_name;
        let old_name = format!("old_{}", name);
        let category = z3_type_category(&local.ty, types);
        match category {
            Z3TypeCategory::Int => {
                if let (Some(pv), Some(ov)) = (
                    vars.get_int(name).cloned(),
                    vars.get_int(&old_name).cloned(),
                ) {
                    session.solver.assert(ov.eq(&pv));
                }
            }
            Z3TypeCategory::Real => {
                if let (Some(pv), Some(ov)) = (
                    vars.get_real(name).cloned(),
                    vars.get_real(&old_name).cloned(),
                ) {
                    session.solver.assert(ov.eq(&pv));
                }
            }
            Z3TypeCategory::Bool => {
                if let (Some(pv), Some(ov)) = (
                    vars.get_bool(name).cloned(),
                    vars.get_bool(&old_name).cloned(),
                ) {
                    session.solver.assert(ov.eq(&pv));
                }
            }
        }
    }

    // 4. Separate contracts by kind
    let mut requires = Vec::new();
    let mut ensures = Vec::new();
    for contract in &callable.contracts {
        match contract.kind {
            ContractKind::Requires => requires.push(&contract.condition),
            ContractKind::Ensures => ensures.push(&contract.condition),
            ContractKind::Invariant => {} // deferred
        }
    }

    let has_math = body
        .root
        .statements
        .iter()
        .any(|stmt| matches!(stmt.kind, crate::core::ir::ResolvedStmtKind::Math(_)));
    if requires.is_empty() && ensures.is_empty() && !has_math {
        return None; // No contracts to verify
    }

    // 5. Assert requires
    let mut encoding_failures = 0;
    for req in &requires {
        if let Some(z3_bool) = resolved_to_z3_bool(req, body, &mut vars) {
            session.solver.assert(z3_bool);
        } else {
            encoding_failures += 1;
        }
    }

    // 5b. Check math obligations (proven from preconditions before body encoding).
    for stmt in &body.root.statements {
        if let crate::core::ir::ResolvedStmtKind::Math(exprs) = &stmt.kind {
            for math_expr in exprs {
                match resolved_to_z3_bool(math_expr, body, &mut vars) {
                    Some(z3_bool) => {
                        let (result, _model) = session.check_scope(z3_bool.not());
                        match result {
                            SatResult::Unsat => {
                                // Math is implied by preconditions — assert it for ensures.
                                session.solver.assert(z3_bool);
                            }
                            SatResult::Sat => {
                                // Math NOT implied by preconditions → Disproven.
                                return Some(crate::verifier::ctx::VerifStatus::Disproven);
                            }
                            SatResult::Unknown => {
                                return Some(crate::verifier::ctx::VerifStatus::SolverUnknown);
                            }
                        }
                    }
                    None => {
                        encoding_failures += 1;
                    }
                }
            }
        }
    }

    // 6. Encode body return → result.
    //    P0 fix: track whether body encoding succeeded. If it fails and
    //    ensures exist, result is unconstrained → NotInTrustedSubset.
    let mut body_encoded = false;
    if let Some(ref result_expr) = body.root.result {
        let encoded = match result_category {
            Z3TypeCategory::Int => {
                resolved_to_z3_int(result_expr, body, &mut vars).map(|body_z3| {
                    if let Some(rv) = vars.get_int("result").cloned() {
                        session.solver.assert(rv.eq(&body_z3));
                    }
                })
            }
            Z3TypeCategory::Real => {
                resolved_to_z3_real(result_expr, body, &mut vars).map(|body_z3| {
                    if let Some(rv) = vars.get_real("result").cloned() {
                        session.solver.assert(rv.eq(&body_z3));
                    }
                })
            }
            Z3TypeCategory::Bool => {
                resolved_to_z3_bool(result_expr, body, &mut vars).map(|body_z3| {
                    if let Some(rv) = vars.get_bool("result").cloned() {
                        session.solver.assert(rv.eq(&body_z3));
                    }
                })
            }
        };
        body_encoded = encoded.is_some();
    }

    // 7. Check each ensures independently
    if ensures.is_empty() {
        // If we had math obligations that were all proven, this is a success,
        // not NoObligations — the math was a verification condition.
        if encoding_failures > 0 {
            return Some(crate::verifier::ctx::VerifStatus::NotInTrustedSubset);
        }
        if has_math {
            return Some(crate::verifier::ctx::VerifStatus::Proven);
        }
        return Some(crate::verifier::ctx::VerifStatus::NoObligations);
    }

    // P0 fix: if body encoding failed, result is unconstrained.
    // Any verdict would be unsound → bail out.
    if !body_encoded {
        return Some(crate::verifier::ctx::VerifStatus::NotInTrustedSubset);
    }

    let mut found_violation = false;
    let mut found_unknown = false;
    for ens in &ensures {
        if let Some(z3_bool) = resolved_to_z3_bool(ens, body, &mut vars) {
            let (result, _model) = session.check_scope(z3_bool.not());
            match result {
                SatResult::Sat => {
                    found_violation = true;
                    break;
                }
                SatResult::Unknown => {
                    found_unknown = true;
                }
                SatResult::Unsat => {}
            }
        } else {
            encoding_failures += 1;
        }
    }

    // P0 fix: any encoding failure → NotInTrustedSubset, regardless of
    // whether a violation was found. The violation could be an artifact
    // of missing constraints from the failed encoding.
    if encoding_failures > 0 {
        return Some(crate::verifier::ctx::VerifStatus::NotInTrustedSubset);
    }
    if found_violation {
        Some(crate::verifier::ctx::VerifStatus::Disproven)
    } else if found_unknown {
        Some(crate::verifier::ctx::VerifStatus::SolverUnknown)
    } else {
        Some(crate::verifier::ctx::VerifStatus::Proven)
    }
}

/// Convert a ResolvedExpr to AST Expr for a limited subset used by FFI call
/// site argument substitution. Returns None for unsupported expression kinds.
///
/// Supported: Int/Float/Bool/String literals, Load (identifier), Binary, Unary,
/// Call (nested), Tuple, Old, Project (field/index/tuple/deref access), Cast.
/// Used by the CheckedProgram-based FFI call site verification (C4 migration).
#[allow(dead_code)]
pub(crate) fn resolved_expr_to_ast_ffi_arg(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
) -> Option<crate::ast::Expr> {
    use crate::ast::Expr;
    let span = crate::span::Span::UNKNOWN;
    let meta = crate::ast::AstNodeMeta::inherited(
        span,
        crate::ast::AstOrigin::Desugared("resolved_expr_to_ast"),
    );
    match &expr.kind {
        ResolvedExprKind::Literal(lit) => {
            let ast_lit = match lit {
                ResolvedLiteral::Int(n) => Lit::Int(*n),
                ResolvedLiteral::FloatBits(bits) => Lit::Float(f64::from_bits(*bits)),
                ResolvedLiteral::Bool(b) => Lit::Bool(*b),
                ResolvedLiteral::String(s) => Lit::String(s.clone()),
                ResolvedLiteral::Unit => return None,
            };
            Some(Expr::Literal(ast_lit).with_meta(meta))
        }
        ResolvedExprKind::Load(place) => {
            let name = place_name(place, body)?;
            Some(Expr::Ident(name).with_meta(meta))
        }
        ResolvedExprKind::Binary { op, left, right } => {
            let l = resolved_expr_to_ast_ffi_arg(left, body)?;
            let r = resolved_expr_to_ast_ffi_arg(right, body)?;
            let bin_op = match op {
                ResolvedBinaryOp::Add => BinOp::Add,
                ResolvedBinaryOp::Subtract => BinOp::Sub,
                ResolvedBinaryOp::Multiply => BinOp::Mul,
                ResolvedBinaryOp::Divide => BinOp::Div,
                ResolvedBinaryOp::Remainder => BinOp::Mod,
                ResolvedBinaryOp::Power => BinOp::Pow,
                ResolvedBinaryOp::Equal => BinOp::EqCmp,
                ResolvedBinaryOp::NotEqual => BinOp::NeCmp,
                ResolvedBinaryOp::Less => BinOp::Lt,
                ResolvedBinaryOp::Greater => BinOp::Gt,
                ResolvedBinaryOp::LessEqual => BinOp::Le,
                ResolvedBinaryOp::GreaterEqual => BinOp::Ge,
                ResolvedBinaryOp::LogicalAnd => BinOp::And,
                ResolvedBinaryOp::LogicalOr => BinOp::Or,
                ResolvedBinaryOp::BitAnd => BinOp::BitAnd,
                ResolvedBinaryOp::BitOr => BinOp::BitOr,
                ResolvedBinaryOp::BitXor => BinOp::BitXor,
                ResolvedBinaryOp::ShiftLeft => BinOp::Shl,
                ResolvedBinaryOp::ShiftRight => BinOp::Shr,
            };
            Some(Expr::Binary(bin_op, Box::new(l), Box::new(r)).with_meta(meta))
        }
        ResolvedExprKind::Unary { op, operand } => {
            let inner = resolved_expr_to_ast_ffi_arg(operand, body)?;
            let un_op = match op {
                ResolvedUnaryOp::Negate => UnOp::Neg,
                ResolvedUnaryOp::Not => UnOp::Not,
                ResolvedUnaryOp::BorrowShared => UnOp::Ref,
                ResolvedUnaryOp::BorrowMutable => UnOp::RefMut,
                ResolvedUnaryOp::Dereference => UnOp::Deref,
            };
            Some(Expr::Unary(un_op, Box::new(inner)).with_meta(meta))
        }
        ResolvedExprKind::Call(call) => {
            // ResolvedCall stores callee as ResolvedCallee (not Expr).
            // For AST conversion, represent the callee as an identifier string.
            use crate::core::ir::ResolvedCallee;
            let callee_name = match &call.callee {
                ResolvedCallee::Function(id)
                | ResolvedCallee::Constructor(id)
                | ResolvedCallee::Extern(id) => id.0.clone(),
                ResolvedCallee::Builtin(id) => id.as_str().to_string(),
                ResolvedCallee::LocalClosure(id) => id.0 .0.clone(),
                ResolvedCallee::ProtocolMethod { method, .. }
                | ResolvedCallee::ActorMethod { method, .. } => method.as_str().to_string(),
                ResolvedCallee::Transition(id) => {
                    format!("{}::{}::{}", id.flow.0, id.event, id.source.name)
                }
            };
            let callee = Expr::Ident(callee_name).with_meta(meta);
            let args: Vec<_> = call
                .arguments
                .iter()
                .filter_map(|a| resolved_expr_to_ast_ffi_arg(&a.value, body))
                .collect();
            if args.len() != call.arguments.len() {
                return None;
            }
            Some(Expr::Call(Box::new(callee), args).with_meta(meta))
        }
        ResolvedExprKind::Tuple(items) => {
            let args: Vec<_> = items
                .iter()
                .filter_map(|i| resolved_expr_to_ast_ffi_arg(i, body))
                .collect();
            if args.len() != items.len() {
                return None;
            }
            Some(Expr::Tuple(args).with_meta(meta))
        }
        ResolvedExprKind::Old(inner) => {
            let inner = resolved_expr_to_ast_ffi_arg(inner, body)?;
            Some(Expr::Old(Box::new(inner)).with_meta(meta))
        }
        ResolvedExprKind::Project { value, projection } => {
            let obj = resolved_expr_to_ast_ffi_arg(value, body)?;
            match projection {
                ResolvedValueProjection::Field(name) => {
                    Some(Expr::Field(Box::new(obj), name.0.clone()).with_meta(meta))
                }
                ResolvedValueProjection::Index(idx) => {
                    let idx_expr = resolved_expr_to_ast_ffi_arg(idx, body)?;
                    Some(Expr::Index(Box::new(obj), Box::new(idx_expr)).with_meta(meta))
                }
                ResolvedValueProjection::Tuple(idx) => {
                    let idx_lit = Expr::Literal(Lit::Int(*idx as i64)).with_meta(meta);
                    Some(Expr::Index(Box::new(obj), Box::new(idx_lit)).with_meta(meta))
                }
                ResolvedValueProjection::Dereference => {
                    Some(Expr::Unary(UnOp::Deref, Box::new(obj)).with_meta(meta))
                }
            }
        }
        ResolvedExprKind::Cast {
            value,
            conversion: _,
        } => resolved_expr_to_ast_ffi_arg(value, body),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ir::{ResolvedLocal, ResolvedLocalId};
    use crate::core::{NodeId, Origin};
    use crate::span::Span;
    use std::collections::BTreeMap;

    fn test_origin() -> Origin {
        Origin::User(Span::UNKNOWN)
    }

    fn test_ty() -> crate::core::ir::ResolvedTypeId {
        // Create a ResolvedTypeId by interning a primitive type through the table.
        let mut table = crate::core::ir::ResolvedTypeTable::new();
        let zonked = crate::core::phase::ZonkedTy::from_resolved(crate::ast::Type::Name(
            "i32".into(),
            Vec::new(),
        ))
        .unwrap();
        table
            .intern_zonked(&zonked, &Default::default(), |name| {
                crate::core::ir::ResolvedTypeName::primitive(name)
            })
            .unwrap()
    }

    fn test_expr(kind: ResolvedExprKind) -> ResolvedExpr {
        ResolvedExpr {
            node_id: NodeId("expr:test".into()),
            origin: test_origin(),
            ty: test_ty(),
            effects: vec![],
            backend_requirements: vec![],
            kind,
        }
    }

    fn test_body_with_local(name: &str) -> ResolvedBody {
        let local_id = ResolvedLocalId(NodeId(format!("local:{}", name)));
        let mut locals = BTreeMap::new();
        locals.insert(
            local_id.clone(),
            ResolvedLocal {
                id: local_id.clone(),
                display_name: name.to_string(),
                ty: test_ty(),
                mutable: false,
                origin: test_origin(),
            },
        );
        ResolvedBody {
            owner: NodeId("function:test".into()),
            locals,
            parameters: vec![local_id],
            captures: vec![],
            place_inputs: BTreeMap::new(),
            default_values: BTreeMap::new(),
            root: ResolvedBlock {
                node_id: NodeId("block:root".into()),
                origin: test_origin(),
                ty: test_ty(),
                statements: vec![],
                result: None,
            },
        }
    }

    #[test]
    fn resolved_literal_int_encodes() {
        let body = test_body_with_local("x");
        let mut vars = Z3VarMap::new();
        let expr = test_expr(ResolvedExprKind::Literal(ResolvedLiteral::Int(42)));
        let result = resolved_to_z3_int(&expr, &body, &mut vars);
        assert!(result.is_some(), "literal int must encode to Z3 Int");
    }

    #[test]
    fn resolved_load_encodes_from_vars() {
        let body = test_body_with_local("x");
        let mut vars = Z3VarMap::new();
        vars.insert_int("x", Z3Int::new_const("x"));
        let local_id = ResolvedLocalId(NodeId("local:x".into()));
        let expr = test_expr(ResolvedExprKind::Load(ResolvedPlace::root(local_id)));
        let result = resolved_to_z3_int(&expr, &body, &mut vars);
        assert!(result.is_some(), "load must resolve to Z3 variable");
    }

    #[test]
    fn resolved_binary_add_encodes() {
        let body = test_body_with_local("x");
        let mut vars = Z3VarMap::new();
        let lit = |n: i64| test_expr(ResolvedExprKind::Literal(ResolvedLiteral::Int(n)));
        let expr = test_expr(ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::Add,
            left: Box::new(lit(3)),
            right: Box::new(lit(4)),
        });
        let result = resolved_to_z3_int(&expr, &body, &mut vars);
        assert!(result.is_some(), "binary add must encode");
    }

    #[test]
    fn resolved_bool_comparison_encodes() {
        let body = test_body_with_local("x");
        let mut vars = Z3VarMap::new();
        let lit = |n: i64| test_expr(ResolvedExprKind::Literal(ResolvedLiteral::Int(n)));
        let expr = test_expr(ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::Greater,
            left: Box::new(lit(5)),
            right: Box::new(lit(3)),
        });
        let result = resolved_to_z3_bool(&expr, &body, &mut vars);
        assert!(result.is_some(), "comparison must encode to Z3 Bool");
    }

    #[test]
    fn resolved_old_encodes() {
        let body = test_body_with_local("x");
        let mut vars = Z3VarMap::new();
        vars.insert_int("old_x", Z3Int::new_const("old_x"));
        let local_id = ResolvedLocalId(NodeId("local:x".into()));
        let inner = test_expr(ResolvedExprKind::Load(ResolvedPlace::root(local_id)));
        let expr = test_expr(ResolvedExprKind::Old(Box::new(inner)));
        let result = resolved_to_z3_int(&expr, &body, &mut vars);
        assert!(result.is_some(), "old(x) must resolve to old_x variable");
    }

    // === End-to-end: Resolved IR contract verification ===

    #[test]
    fn e2e_resolved_contract_proven() {
        // A simple contract that should be provable from Resolved IR:
        //   func abs_val(x: i32) -> i32 {
        //       requires: x >= 0
        //       ensures: result >= 0
        //       x
        //   }
        let source = r#"
func abs_val(x: i32) -> i32 {
    requires: x >= 0
    ensures: result >= 0
    x
}
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");

        let owner = NodeId("function:abs_val".into());
        let callable = program.callables().get(&owner).expect("abs_val callable");
        assert!(
            !callable.contracts.is_empty(),
            "abs_val must have contracts"
        );

        let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
        let status =
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session);
        assert_eq!(
            status,
            Some(crate::verifier::ctx::VerifStatus::Proven),
            "requires: x >= 0, ensures: result >= 0, body: x → must be Proven"
        );
    }

    #[test]
    fn e2e_resolved_contract_disproven() {
        // A contract that should be disprovable:
        //   func bad(x: i32) -> i32 {
        //       requires: x >= 0
        //       ensures: result > x
        //       x
        //   }
        // result == x, so result > x is false.
        let source = r#"
func bad(x: i32) -> i32 {
    requires: x >= 0
    ensures: result > x
    x
}
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");

        let owner = NodeId("function:bad".into());
        let callable = program.callables().get(&owner).expect("bad callable");

        let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
        let status =
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session);
        assert_eq!(
            status,
            Some(crate::verifier::ctx::VerifStatus::Disproven),
            "ensures: result > x with body: x → must be Disproven"
        );
    }

    #[test]
    fn e2e_resolved_contract_no_contracts() {
        let source = r#"
func plain(x: i32) -> i32 { x + 1 }
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");

        let owner = NodeId("function:plain".into());
        let callable = program.callables().get(&owner).expect("plain callable");

        let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
        let status =
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session);
        assert_eq!(status, None, "no contracts → None (skip verification)");
    }

    // === P0 soundness regression tests ===

    #[test]
    fn e2e_resolved_contract_bool_result() {
        // P1 fix: result variable type inferred from signature (Bool).
        // Bool equality in ensures now supported (int → real → bool fallback).
        let source = r#"
func is_positive(x: i32) -> bool {
    requires: x >= -1000
    ensures: result == true
    x > 0
}
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");

        let owner = NodeId("function:is_positive".into());
        let callable = program.callables().get(&owner).expect("callable");

        let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
        let status =
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session);
        // x > 0 does NOT always equal true (e.g. x = -1 satisfies requires but x > 0 is false).
        // So ensures: result == true should be Disproven.
        assert_eq!(
            status,
            Some(crate::verifier::ctx::VerifStatus::Disproven),
            "ensures: result == true with body: x > 0 → Disproven (x can be negative)"
        );
    }

    #[test]
    fn e2e_resolved_contract_unencodable_body_not_in_trusted_subset() {
        // P0 fix: when body encoding fails, return NotInTrustedSubset
        // instead of checking ensures with an unconstrained result.
        // A function call in the body is not encodable by the simple
        // resolved_to_z3_int encoder.
        let source = r#"
func helper(x: i32) -> i32 { x + 1 }
func caller(x: i32) -> i32 {
    requires: x >= 0
    ensures: result >= 0
    helper(x)
}
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");

        let owner = NodeId("function:caller".into());
        let callable = program.callables().get(&owner).expect("callable");

        let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
        let status =
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session);
        // Body is a function call → not encodable → NotInTrustedSubset
        assert_eq!(
            status,
            Some(crate::verifier::ctx::VerifStatus::NotInTrustedSubset),
            "unencodable body (function call) → NotInTrustedSubset, not Disproven"
        );
    }
}
