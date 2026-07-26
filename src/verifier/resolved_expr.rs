//! SD-4/Body migration: Z3 encoding for ResolvedExpr (Typed Resolved IR).
//!
//! Parallel to `expr.rs` (raw AST encoding). Consumes `ResolvedExprKind`
//! instead of `Expr`, enabling the verifier to work from CheckedProgram
//! without `legacy_body_file()`.
//!
//! # Status
//! Foundation layer: encoding functions implemented and unit-tested.
//! Integration into `verify_func()` is the next step (0.31.44+).
#![allow(dead_code)]
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
            // Try int comparison first, then real
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

/// Verify contracts from Resolved IR (ResolvedCallable).
///
/// This is the Resolved IR parallel of `verify_func()` (func.rs).
/// It extracts contracts from `ResolvedCallable.contracts`, creates Z3
/// variables from `ResolvedBody.parameters`, and checks validity.
///
/// # Limitations (vs AST path)
/// - No let-substitution (body return encoding deferred)
/// - No call-site requires checking (deferred)
/// - No math obligation checking (deferred)
/// - No invariant checking (deferred)
/// - No callee ensures propagation (deferred)
///
/// These limitations mean the Resolved IR path currently handles
/// simple requires/ensures contracts on pure functions. Complex
/// contracts still use the AST path.
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

    // 2. Create result variable (default to Int)
    let result_var = Z3Int::new_const("result");
    vars.insert_int("result", result_var.clone());

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

    if requires.is_empty() && ensures.is_empty() {
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

    // 6. Encode body return → result (if available)
    if let Some(ref result_expr) = body.root.result {
        if let Some(body_z3) = resolved_to_z3_int(result_expr, body, &mut vars) {
            session.solver.assert(result_var.eq(&body_z3));
        }
    }

    // 7. Check each ensures independently
    if ensures.is_empty() {
        return Some(crate::verifier::ctx::VerifStatus::NoObligations);
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

    if encoding_failures > 0 && !found_violation {
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
}
