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
    PrimitiveType, ResolvedBinaryOp, ResolvedBlock, ResolvedBody, ResolvedExpr, ResolvedExprKind,
    ResolvedLiteral, ResolvedLocalId, ResolvedPatternKind, ResolvedPlace, ResolvedStmtKind,
    ResolvedType, ResolvedTypeTable, ResolvedUnaryOp, ResolvedValueProjection,
};
use crate::verifier::ctx::Z3VarMap;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};

/// Resolve a place to the unique variable key of its base local.
///
/// C-7 (audit 2026-08-05): keys are derived from `ResolvedLocalId`, not
/// `display_name`. Shadowed locals share the bare display name of the local
/// they shadow; keying by display name resolved shadow loads back onto the
/// shadowed parameter/local variable, fabricating Proven for contracts such
/// as `if c { let x = x + 1; x } else { x }` + `ensures: result == x`.
/// Places with projections are rejected fail-closed: they are structured
/// reads, not simple local loads.
fn local_key(place: &ResolvedPlace, body: &ResolvedBody) -> Option<String> {
    if !place.projections.is_empty() {
        return None;
    }
    body.locals
        .get(&place.base)
        .map(|local| local_var_key(&local.id, &local.display_name))
}

/// Z3 variable key for a local: `{display_name}#{local_id}`. The display-name
/// prefix keeps solver output readable; the id suffix makes the key unique
/// under shadowing.
fn local_var_key(id: &ResolvedLocalId, display_name: &str) -> String {
    format!("{}#{}", display_name, id.0 .0)
}

/// Encode a ResolvedExpr as a Z3 Int term.
pub(crate) fn resolved_to_z3_int(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    match &expr.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::Int(n)) => Some(Z3Int::from_i64(*n)),
        ResolvedExprKind::Literal(ResolvedLiteral::Bool(b)) => {
            Some(Z3Int::from_i64(if *b { 1 } else { 0 }))
        }
        ResolvedExprKind::Load(place) => {
            let key = local_key(place, body)?;
            vars.get_int(&key).cloned()
        }
        ResolvedExprKind::Old(inner) => {
            if let ResolvedExprKind::Load(place) = &inner.kind {
                let key = local_key(place, body)?;
                let old_key = format!("old_{}", key);
                return vars.get_int(&old_key).cloned();
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
            let key = format!("{}.{}", base, proj_name);
            Some(vars.get_or_create_int(&key))
        }
        ResolvedExprKind::Binary { op, left, right } => {
            let l = resolved_to_z3_int(left, body, types, vars)?;
            let r = resolved_to_z3_int(right, body, types, vars)?;
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
                let v = resolved_to_z3_int(operand, body, types, vars)?;
                Some(v.unary_minus())
            }
            _ => None,
        },
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let cond_z3 = resolved_to_z3_bool(condition, body, types, vars)?;
            let then_z3 = resolved_block_tail_int(then_block, body, types, vars)?;
            let else_z3 = resolved_block_tail_int(else_block, body, types, vars)?;
            Some(cond_z3.ite(&then_z3, &else_z3))
        }
        ResolvedExprKind::Block(block) => resolved_block_tail_int(block, body, types, vars),
        _ => None,
    }
}

/// Encode a ResolvedExpr as a Z3 Real term.
///
/// H-21 (audit 2026-08-05): this encoder is only reachable for i32-exact
/// values now — `verify_contracts_from_resolved` fail-closes any callable
/// whose contracts or body involve f64 before encoding (IEEE 754 rounding
/// and NaN are not modeled). The 0.0-only literal guard below is retained
/// as defense in depth.
pub(crate) fn resolved_to_z3_real(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
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
            let key = local_key(place, body)?;
            vars.get_real(&key).cloned()
        }
        ResolvedExprKind::Old(inner) => {
            if let ResolvedExprKind::Load(place) = &inner.kind {
                let key = local_key(place, body)?;
                let old_key = format!("old_{}", key);
                return vars.get_real(&old_key).cloned();
            }
            None
        }
        ResolvedExprKind::Binary { op, left, right } => {
            let l = resolved_to_z3_real(left, body, types, vars)?;
            let r = resolved_to_z3_real(right, body, types, vars)?;
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
                let v = resolved_to_z3_real(operand, body, types, vars)?;
                Some(v.unary_minus())
            }
            _ => None,
        },
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let cond_z3 = resolved_to_z3_bool(condition, body, types, vars)?;
            let then_z3 = resolved_block_tail_real(then_block, body, types, vars)?;
            let else_z3 = resolved_block_tail_real(else_block, body, types, vars)?;
            Some(cond_z3.ite(&then_z3, &else_z3))
        }
        ResolvedExprKind::Block(block) => resolved_block_tail_real(block, body, types, vars),
        _ => None,
    }
}

/// Encode a ResolvedExpr as a Z3 Bool term.
pub(crate) fn resolved_to_z3_bool(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    match &expr.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::Bool(b)) => Some(Z3Bool::from_bool(*b)),
        ResolvedExprKind::Load(place) => {
            let key = local_key(place, body)?;
            vars.get_bool(&key).cloned()
        }
        ResolvedExprKind::Binary { op, left, right } => {
            // Try int comparison first, then real, then bool equality
            if let (Some(l), Some(r)) = (
                resolved_to_z3_int(left, body, types, vars),
                resolved_to_z3_int(right, body, types, vars),
            ) {
                return match op {
                    ResolvedBinaryOp::Equal => Some(l.eq(&r)),
                    ResolvedBinaryOp::NotEqual => Some(l.eq(&r).not()),
                    ResolvedBinaryOp::Less => Some(l.lt(&r)),
                    ResolvedBinaryOp::Greater => Some(l.gt(&r)),
                    ResolvedBinaryOp::LessEqual => Some(l.le(&r)),
                    ResolvedBinaryOp::GreaterEqual => Some(l.ge(&r)),
                    ResolvedBinaryOp::LogicalAnd => {
                        let lb = resolved_to_z3_bool(left, body, types, vars)?;
                        let rb = resolved_to_z3_bool(right, body, types, vars)?;
                        Some(Z3Bool::and(&[&lb, &rb]))
                    }
                    ResolvedBinaryOp::LogicalOr => {
                        let lb = resolved_to_z3_bool(left, body, types, vars)?;
                        let rb = resolved_to_z3_bool(right, body, types, vars)?;
                        Some(Z3Bool::or(&[&lb, &rb]))
                    }
                    _ => None,
                };
            }
            // Fall back to real comparison
            if let (Some(l), Some(r)) = (
                resolved_to_z3_real(left, body, types, vars),
                resolved_to_z3_real(right, body, types, vars),
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
                resolved_to_z3_bool(left, body, types, vars),
                resolved_to_z3_bool(right, body, types, vars),
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
                let v = resolved_to_z3_bool(operand, body, types, vars)?;
                Some(v.not())
            }
            _ => None,
        },
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let cond_z3 = resolved_to_z3_bool(condition, body, types, vars)?;
            let then_z3 = resolved_block_tail_bool(then_block, body, types, vars)?;
            let else_z3 = resolved_block_tail_bool(else_block, body, types, vars)?;
            Some(cond_z3.ite(&then_z3, &else_z3))
        }
        ResolvedExprKind::Block(block) => resolved_block_tail_bool(block, body, types, vars),
        _ => None,
    }
}

// === Block encoding helpers ===

/// Encode a block's statements, binding let-introduced locals into the var map.
///
/// C-7 (audit 2026-08-05): previously statements were ignored entirely and
/// only `block.result` was encoded. A `let` that shadows a parameter then
/// aliased back onto the parameter's variable via the shared display name.
/// Now each plain binding inserts its encoded initializer under the local's
/// unique id key (pure term substitution — the trusted subset has no
/// mutation, so a let-bound name is a rigid abbreviation of its initializer).
/// Any statement outside this shape (assignment, loops, effectful
/// expressions, …) fails closed → NotInTrustedSubset at the caller.
fn resolved_block_stmts(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
) -> Option<()> {
    for stmt in &block.statements {
        match &stmt.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer: Some(init),
            } => {
                let ResolvedPatternKind::Binding {
                    local,
                    by_reference: None,
                } = &pattern.kind
                else {
                    // Destructuring / reference bindings are outside the
                    // trusted subset — fail closed.
                    return None;
                };
                let display = body
                    .locals
                    .get(local)
                    .map(|l| l.display_name.clone())
                    .unwrap_or_default();
                let key = local_var_key(local, &display);
                match z3_type_category(&init.ty, types) {
                    Z3TypeCategory::Int => {
                        let term = resolved_to_z3_int(init, body, types, vars)?;
                        vars.insert_int(key, term);
                    }
                    Z3TypeCategory::Real => {
                        let term = resolved_to_z3_real(init, body, types, vars)?;
                        vars.insert_real(key, term);
                    }
                    Z3TypeCategory::Bool => {
                        let term = resolved_to_z3_bool(init, body, types, vars)?;
                        vars.insert_bool(key, term);
                    }
                }
            }
            // Contracts are extracted into `callable.contracts`; math lemmas
            // are discharged separately from the body root. Skipping them here
            // is conservative: no unproven assumption enters the context.
            ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_) => {}
            // Assign/loops/effectful statements are outside the trusted subset.
            _ => return None,
        }
    }
    Some(())
}

fn resolved_block_tail_int(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    resolved_block_stmts(block, body, types, vars)?;
    block
        .result
        .as_ref()
        .and_then(|e| resolved_to_z3_int(e, body, types, vars))
}

fn resolved_block_tail_real(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
) -> Option<Z3Real> {
    resolved_block_stmts(block, body, types, vars)?;
    block
        .result
        .as_ref()
        .and_then(|e| resolved_to_z3_real(e, body, types, vars))
}

fn resolved_block_tail_bool(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    resolved_block_stmts(block, body, types, vars)?;
    block
        .result
        .as_ref()
        .and_then(|e| resolved_to_z3_bool(e, body, types, vars))
}

/// Build a variable name for a field projection (parallel to `field_var_name` in expr.rs).
fn resolved_field_var_name(expr: &ResolvedExpr, body: &ResolvedBody) -> String {
    match &expr.kind {
        ResolvedExprKind::Load(place) => local_key(place, body).unwrap_or_else(|| "_".into()),
        ResolvedExprKind::Project { value, projection } => {
            let proj_name = match projection {
                ResolvedValueProjection::Field(id) => id.0.clone(),
                ResolvedValueProjection::Tuple(idx) => format!("t{}", idx),
                ResolvedValueProjection::Index(_) => "idx".to_string(),
                ResolvedValueProjection::Dereference => "deref".to_string(),
            };
            format!("{}.{}", resolved_field_var_name(value, body), proj_name)
        }
        _ => "_expr".into(),
    }
}

// === f64 detection (H-21) ===

/// H-21 (audit 2026-08-05): true when the expression or any sub-expression
/// is f64-typed. Every `ResolvedExpr` carries its materialized type, so this
/// walk is exact (unlike the AST path's syntactic heuristics).
fn expr_involves_f64(expr: &ResolvedExpr, types: &ResolvedTypeTable) -> bool {
    if matches!(
        types.get(&expr.ty),
        Some(ResolvedType::Primitive(PrimitiveType::F64))
    ) {
        return true;
    }
    match &expr.kind {
        ResolvedExprKind::FString(parts) => parts.iter().any(|part| match part {
            crate::core::ir::ResolvedFStringPart::Text(_) => false,
            crate::core::ir::ResolvedFStringPart::Interpolation(e) => expr_involves_f64(e, types),
        }),
        ResolvedExprKind::Project { value, projection } => {
            expr_involves_f64(value, types)
                || match projection {
                    ResolvedValueProjection::Index(idx) => expr_involves_f64(idx, types),
                    _ => false,
                }
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            expr_involves_f64(left, types) || expr_involves_f64(right, types)
        }
        ResolvedExprKind::Unary { operand, .. } => expr_involves_f64(operand, types),
        ResolvedExprKind::Call(call) => call
            .arguments
            .iter()
            .any(|arg| expr_involves_f64(&arg.value, types)),
        ResolvedExprKind::Tuple(items)
        | ResolvedExprKind::List(items)
        | ResolvedExprKind::Set(items) => items.iter().any(|e| expr_involves_f64(e, types)),
        ResolvedExprKind::Map(entries) => entries
            .iter()
            .any(|(k, v)| expr_involves_f64(k, types) || expr_involves_f64(v, types)),
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            expr_involves_f64(value, types)
                || expr_involves_f64(iterable, types)
                || guard.as_ref().is_some_and(|g| expr_involves_f64(g, types))
        }
        ResolvedExprKind::OptionalChain { receiver, .. } => expr_involves_f64(receiver, types),
        ResolvedExprKind::TypeOf(inner) | ResolvedExprKind::Old(inner) => {
            expr_involves_f64(inner, types)
        }
        ResolvedExprKind::Record { fields, .. } => {
            fields.iter().any(|f| expr_involves_f64(&f.value, types))
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Comptime(block) => {
            block_involves_f64(block, types)
        }
        ResolvedExprKind::Scope { body: block, .. } => block_involves_f64(block, types),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            expr_involves_f64(condition, types)
                || block_involves_f64(then_block, types)
                || block_involves_f64(else_block, types)
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            expr_involves_f64(scrutinee, types)
                || arms.iter().any(|arm| {
                    expr_involves_f64(&arm.body, types)
                        || arm
                            .guard
                            .as_ref()
                            .is_some_and(|g| expr_involves_f64(g, types))
                })
        }
        ResolvedExprKind::Try { value, .. } | ResolvedExprKind::Cast { value, .. } => {
            expr_involves_f64(value, types)
        }
        ResolvedExprKind::Range { start, end } => {
            expr_involves_f64(start, types) || expr_involves_f64(end, types)
        }
        ResolvedExprKind::Slice { target, start, end } => {
            expr_involves_f64(target, types)
                || start.as_ref().is_some_and(|e| expr_involves_f64(e, types))
                || end.as_ref().is_some_and(|e| expr_involves_f64(e, types))
        }
        ResolvedExprKind::Spawn(inner) | ResolvedExprKind::Await(inner) => {
            expr_involves_f64(inner, types)
        }
        ResolvedExprKind::Lambda(lambda) => block_involves_f64(&lambda.body, types),
        // Quote is inert data; no runtime f64 evaluation.
        ResolvedExprKind::Quote(_) => false,
        // Leaves without sub-expressions (Literal/Load/Constant/Callable/
        // DefaultArgument/ComptimeValue/TypeValue). The node's own type was
        // checked above.
        _ => false,
    }
}

fn block_involves_f64(block: &ResolvedBlock, types: &ResolvedTypeTable) -> bool {
    for stmt in &block.statements {
        let involved = match &stmt.kind {
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|e| expr_involves_f64(e, types)),
            ResolvedStmtKind::Assign { value, .. } | ResolvedStmtKind::Expr(value) => {
                expr_involves_f64(value, types)
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                value.as_ref().is_some_and(|e| expr_involves_f64(e, types))
            }
            ResolvedStmtKind::While { condition, body } => {
                expr_involves_f64(condition, types) || block_involves_f64(body, types)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => expr_involves_f64(initializer, types) || block_involves_f64(body, types),
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_involves_f64(initializer, types)
                    || block_involves_f64(then_block, types)
                    || else_block
                        .as_ref()
                        .is_some_and(|b| block_involves_f64(b, types))
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                block_involves_f64(body, types)
            }
            ResolvedStmtKind::Pinned { value, body, .. } => {
                expr_involves_f64(value, types) || block_involves_f64(body, types)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                expr_involves_f64(iterable, types) || block_involves_f64(body, types)
            }
            ResolvedStmtKind::Math(exprs) => exprs.iter().any(|e| expr_involves_f64(e, types)),
            ResolvedStmtKind::Contract { condition, .. } => expr_involves_f64(condition, types),
            ResolvedStmtKind::Continue
            | ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::NestedCallable(_) => false,
        };
        if involved {
            return true;
        }
    }
    block
        .result
        .as_ref()
        .is_some_and(|e| expr_involves_f64(e, types))
}

// === i32 definedness obligations (H-24) ===

/// One definedness verification condition for checked i32 arithmetic.
pub(crate) struct ResolvedIntDefinedness {
    pub(crate) condition: Z3Bool,
    pub(crate) failure: &'static str,
}

/// H-24 (audit 2026-08-05): the Resolved engine previously emitted NO i32
/// definedness VCs — `ensures: result > x; x + 1` verified Proven at
/// x == i32::MAX while the runtime traps (SD-7 trap semantics). Mirror the
/// AST engine's `collect_i32_definedness` machinery: every i32-typed
/// Add/Sub/Mul must stay in range, Div/Rem needs a non-zero divisor and must
/// not be MIN / -1, Negate must not hit MIN. Only `PrimitiveType::I32`
/// sub-expressions generate obligations (i64/int remain unbounded, matching
/// the documented V-6 gap in the AST engine).
fn collect_expr_i32_definedness(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    obligations: &mut Vec<ResolvedIntDefinedness>,
) -> Option<()> {
    let is_i32 = matches!(
        types.get(&expr.ty),
        Some(ResolvedType::Primitive(PrimitiveType::I32))
    );
    match &expr.kind {
        ResolvedExprKind::Binary { op, left, right } => {
            collect_expr_i32_definedness(left, body, types, vars, obligations)?;
            collect_expr_i32_definedness(right, body, types, vars, obligations)?;
            if is_i32 {
                let l = resolved_to_z3_int(left, body, types, vars)?;
                let r = resolved_to_z3_int(right, body, types, vars)?;
                match op {
                    ResolvedBinaryOp::Add
                    | ResolvedBinaryOp::Subtract
                    | ResolvedBinaryOp::Multiply => {
                        let result = match op {
                            ResolvedBinaryOp::Add => Z3Int::add(&[&l, &r]),
                            ResolvedBinaryOp::Subtract => Z3Int::sub(&[&l, &r]),
                            ResolvedBinaryOp::Multiply => Z3Int::mul(&[&l, &r]),
                            _ => unreachable!(
                                "resolved i32 definedness: only Add/Sub/Mul push range checks"
                            ),
                        };
                        let lo = Z3Int::from_i64(i32::MIN as i64);
                        let hi = Z3Int::from_i64(i32::MAX as i64);
                        obligations.push(ResolvedIntDefinedness {
                            condition: Z3Bool::and(&[&result.ge(&lo), &result.le(&hi)]),
                            failure: "integer overflow is not excluded by preconditions",
                        });
                    }
                    ResolvedBinaryOp::Divide | ResolvedBinaryOp::Remainder => {
                        let zero = Z3Int::from_i64(0);
                        let min = Z3Int::from_i64(i32::MIN as i64);
                        let neg_one = Z3Int::from_i64(-1);
                        let min_overflow = Z3Bool::and(&[&l.eq(&min), &r.eq(&neg_one)]);
                        obligations.push(ResolvedIntDefinedness {
                            condition: Z3Bool::and(&[&r.ne(&zero), &min_overflow.not()]),
                            failure: "integer operation is undefined (zero divisor or MIN / -1)",
                        });
                    }
                    _ => {}
                }
            }
        }
        ResolvedExprKind::Unary { op, operand } => {
            collect_expr_i32_definedness(operand, body, types, vars, obligations)?;
            if *op == ResolvedUnaryOp::Negate && is_i32 {
                let v = resolved_to_z3_int(operand, body, types, vars)?;
                let min = Z3Int::from_i64(i32::MIN as i64);
                obligations.push(ResolvedIntDefinedness {
                    condition: v.ne(&min),
                    failure: "integer overflow is not excluded by preconditions",
                });
            }
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let cond = resolved_to_z3_bool(condition, body, types, vars)?;
            let mut then_obligations = Vec::new();
            collect_block_i32_definedness(then_block, body, types, vars, &mut then_obligations)?;
            for obligation in &mut then_obligations {
                obligation.condition = cond.implies(&obligation.condition);
            }
            obligations.extend(then_obligations);
            let else_condition = cond.not();
            let mut else_obligations = Vec::new();
            collect_block_i32_definedness(else_block, body, types, vars, &mut else_obligations)?;
            for obligation in &mut else_obligations {
                obligation.condition = else_condition.implies(&obligation.condition);
            }
            obligations.extend(else_obligations);
        }
        ResolvedExprKind::Block(block) => {
            collect_block_i32_definedness(block, body, types, vars, obligations)?;
        }
        ResolvedExprKind::Scope { body: block, .. } => {
            // ieee_float scopes relax f64 finiteness only; i32 arithmetic
            // definedness still applies (SD-9), so recurse unconditionally.
            collect_block_i32_definedness(block, body, types, vars, obligations)?;
        }
        ResolvedExprKind::Old(inner)
        | ResolvedExprKind::TypeOf(inner)
        | ResolvedExprKind::Cast { value: inner, .. }
        | ResolvedExprKind::Spawn(inner)
        | ResolvedExprKind::Await(inner) => {
            collect_expr_i32_definedness(inner, body, types, vars, obligations)?;
        }
        // Leaves: no arithmetic operations. Anything else (Call/Match/…) is
        // outside the encodable subset — fail closed just like the value
        // encoding does.
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Project { .. }
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => {}
        _ => return None,
    }
    Some(())
}

fn collect_block_i32_definedness(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    obligations: &mut Vec<ResolvedIntDefinedness>,
) -> Option<()> {
    // Bind statements: gather obligations from the initializer and bind the
    // local (mirrors `resolved_block_stmts`) so later loads resolve while
    // collecting branch obligations.
    for stmt in &block.statements {
        match &stmt.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer: Some(init),
            } => {
                collect_expr_i32_definedness(init, body, types, vars, obligations)?;
                if let ResolvedPatternKind::Binding {
                    local,
                    by_reference: None,
                } = &pattern.kind
                {
                    if let Some(local_info) = body.locals.get(local) {
                        let key = local_var_key(local, &local_info.display_name);
                        match z3_type_category(&init.ty, types) {
                            Z3TypeCategory::Int => {
                                if let Some(term) = resolved_to_z3_int(init, body, types, vars) {
                                    vars.insert_int(key, term);
                                }
                            }
                            Z3TypeCategory::Real => {
                                if let Some(term) = resolved_to_z3_real(init, body, types, vars) {
                                    vars.insert_real(key, term);
                                }
                            }
                            Z3TypeCategory::Bool => {
                                if let Some(term) = resolved_to_z3_bool(init, body, types, vars) {
                                    vars.insert_bool(key, term);
                                }
                            }
                        }
                    }
                }
            }
            ResolvedStmtKind::Bind {
                initializer: None, ..
            } => {}
            ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_) => {}
            _ => return None,
        }
    }
    if let Some(result) = &block.result {
        collect_expr_i32_definedness(result, body, types, vars, obligations)?;
    }
    Some(())
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
///
/// C-7 (audit 2026-08-05): variables are keyed by `ResolvedLocalId`
/// (via `local_var_key`), not by display name, so shadowed locals cannot
/// alias parameters.
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
        let key = local_var_key(&local.id, &local.display_name);
        let category = z3_type_category(&local.ty, types);
        match category {
            Z3TypeCategory::Real => {
                vars.insert_real(&key, Z3Real::new_const(key.as_str()));
            }
            Z3TypeCategory::Bool => {
                vars.insert_bool(&key, Z3Bool::new_const(key.as_str()));
            }
            Z3TypeCategory::Int => {
                let iv = Z3Int::new_const(key.as_str());
                vars.insert_int(&key, iv.clone());
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
        let old_key = format!("old_{}", key);
        match category {
            Z3TypeCategory::Real => {
                vars.insert_real(&old_key, Z3Real::new_const(old_key.as_str()));
            }
            Z3TypeCategory::Bool => {
                vars.insert_bool(&old_key, Z3Bool::new_const(old_key.as_str()));
            }
            Z3TypeCategory::Int => {
                vars.insert_int(&old_key, Z3Int::new_const(old_key.as_str()));
            }
        }
    }
}

/// Locate the `result` local that the checker installs into `ensures`
/// contracts (lower.rs creates a dedicated local with display name "result"
/// whose id ends in `/contract-result/local`). Returns its variable key, or
/// the plain "result" fallback when the callable has no ensures contracts.
fn contract_result_key(body: &ResolvedBody) -> String {
    body.locals
        .values()
        .find(|local| local.id.0 .0.ends_with("/contract-result/local"))
        .map(|local| local_var_key(&local.id, &local.display_name))
        .unwrap_or_else(|| "result".to_string())
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
/// Returns `(status, message)`; `None` means there was nothing to verify.
///
/// # Current coverage (vs AST path)
/// - Requires/ensures: ✅ (int/bool)
/// - Math obligations: ✅ (proven from preconditions before ensures)
/// - old() expressions: ✅
/// - Field projections: ✅
/// - Let-substitution: ✅ (id-keyed, C-7)
/// - i32 definedness VCs: ✅ (overflow/div-zero/MIN÷-1, H-24)
/// - f64 contracts: fail-closed NotInTrustedSubset (H-21)
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
) -> Option<(crate::verifier::ctx::VerifStatus, String)> {
    use crate::core::ir::ContractKind;
    use crate::verifier::ctx::VerifStatus;
    use z3::SatResult;

    let status_msg = |status: VerifStatus, detail: &str| -> (VerifStatus, String) {
        let message = if detail.is_empty() {
            format!("resolved IR verification: {:?}", status)
        } else {
            format!("resolved IR verification: {:?}: {}", status, detail)
        };
        (status, message)
    };

    let body = &callable.body;
    let mut vars = Z3VarMap::new();

    // 1. Create parameter variables
    create_parameter_vars(body, types, &mut vars, session);

    // 2. Create result variable keyed by the ensures-contract `result` local
    //    (C-7: id-keyed like every other local).
    let result_key = contract_result_key(body);
    let result_category = z3_type_category(&callable.signature.result, types);
    match result_category {
        Z3TypeCategory::Real => {
            vars.insert_real(&result_key, Z3Real::new_const(result_key.as_str()));
        }
        Z3TypeCategory::Bool => {
            vars.insert_bool(&result_key, Z3Bool::new_const(result_key.as_str()));
        }
        Z3TypeCategory::Int => {
            let rv = Z3Int::new_const(result_key.as_str());
            vars.insert_int(&result_key, rv.clone());
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
        let key = local_var_key(&local.id, &local.display_name);
        let old_key = format!("old_{}", key);
        let category = z3_type_category(&local.ty, types);
        match category {
            Z3TypeCategory::Int => {
                if let (Some(pv), Some(ov)) =
                    (vars.get_int(&key).cloned(), vars.get_int(&old_key).cloned())
                {
                    session.solver.assert(ov.eq(&pv));
                }
            }
            Z3TypeCategory::Real => {
                if let (Some(pv), Some(ov)) = (
                    vars.get_real(&key).cloned(),
                    vars.get_real(&old_key).cloned(),
                ) {
                    session.solver.assert(ov.eq(&pv));
                }
            }
            Z3TypeCategory::Bool => {
                if let (Some(pv), Some(ov)) = (
                    vars.get_bool(&key).cloned(),
                    vars.get_bool(&old_key).cloned(),
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

    // 4b. H-21 (audit 2026-08-05): f64 contracts FAIL CLOSED. The Resolved
    //     engine encoded f64 as exact Z3 Reals with no IEEE 754 rounding or
    //     NaN modeling, proving fake contracts such as float reassociation.
    //     The Resolved IR is fully typed, so detection is exact: any f64-typed
    //     sub-expression in the contracts or the body rejects the callable.
    //     (Full IEEE modeling is deliberately not attempted — conservative.)
    let f64_involved = requires
        .iter()
        .chain(ensures.iter())
        .any(|condition| expr_involves_f64(condition, types))
        || body
            .root
            .statements
            .iter()
            .any(|stmt| matches!(&stmt.kind, crate::core::ir::ResolvedStmtKind::Math(exprs) if exprs.iter().any(|e| expr_involves_f64(e, types))))
        || block_involves_f64(&body.root, types);
    if f64_involved {
        return Some(status_msg(
            VerifStatus::NotInTrustedSubset,
            "f64 floating-point is not verified: IEEE 754 rounding and NaN are not modeled \
             (fail-closed; exact-Real encoding removed by audit H-21)",
        ));
    }

    // 5. Assert requires
    let mut encoding_failures = 0;
    for req in &requires {
        if let Some(z3_bool) = resolved_to_z3_bool(req, body, types, &mut vars) {
            session.solver.assert(z3_bool);
        } else {
            encoding_failures += 1;
        }
    }

    // 5b. Check math obligations (proven from preconditions before body encoding).
    for stmt in &body.root.statements {
        if let crate::core::ir::ResolvedStmtKind::Math(exprs) = &stmt.kind {
            for math_expr in exprs {
                match resolved_to_z3_bool(math_expr, body, types, &mut vars) {
                    Some(z3_bool) => {
                        let (result, _model) = session.check_scope(z3_bool.not());
                        match result {
                            SatResult::Unsat => {
                                // Math is implied by preconditions — assert it for ensures.
                                session.solver.assert(z3_bool);
                            }
                            SatResult::Sat => {
                                // Math NOT implied by preconditions → Disproven.
                                return Some(status_msg(
                                    VerifStatus::Disproven,
                                    "math obligation is not implied by preconditions",
                                ));
                            }
                            SatResult::Unknown => {
                                return Some(status_msg(
                                    VerifStatus::SolverUnknown,
                                    "solver could not prove math obligation",
                                ));
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

    // 5c. H-24 (audit 2026-08-05): i32 definedness VCs. Checked i32
    //     arithmetic traps on overflow / zero divisor / MIN ÷ -1 (SD-7/SD-8);
    //     a postcondition may not be proved under "assume no trap". Mirror
    //     the AST engine: collect obligations in evaluation order, discharge
    //     each under the preconditions, and only then bind the body result.
    let mut obligations = Vec::new();
    if collect_block_i32_definedness(&body.root, body, types, &mut vars, &mut obligations).is_none()
    {
        return Some(status_msg(
            VerifStatus::NotInTrustedSubset,
            "could not encode i32 definedness obligations (fail-closed)",
        ));
    }
    for obligation in obligations {
        let (result, _model) = session.check_scope(obligation.condition.not());
        match result {
            SatResult::Unsat => {
                // Defined under the preconditions — admit for the rest of the proof.
                session.solver.assert(obligation.condition);
            }
            SatResult::Sat => {
                return Some(status_msg(VerifStatus::Disproven, obligation.failure));
            }
            SatResult::Unknown => {
                return Some(status_msg(
                    VerifStatus::SolverUnknown,
                    "solver could not prove integer definedness",
                ));
            }
        }
    }

    // 6. Encode body return → result.
    //    P0 fix: track whether body encoding succeeded. If it fails and
    //    ensures exist, result is unconstrained → NotInTrustedSubset.
    //    C-7: encode the whole root block (statements bind let-locals under
    //    their unique ids) instead of only the tail expression.
    let mut body_encoded = false;
    if body.root.result.is_some() {
        let encoded = match result_category {
            Z3TypeCategory::Int => {
                resolved_block_tail_int(&body.root, body, types, &mut vars).map(|body_z3| {
                    if let Some(rv) = vars.get_int(&result_key).cloned() {
                        session.solver.assert(rv.eq(&body_z3));
                    }
                })
            }
            Z3TypeCategory::Real => resolved_block_tail_real(&body.root, body, types, &mut vars)
                .map(|body_z3| {
                    if let Some(rv) = vars.get_real(&result_key).cloned() {
                        session.solver.assert(rv.eq(&body_z3));
                    }
                }),
            Z3TypeCategory::Bool => resolved_block_tail_bool(&body.root, body, types, &mut vars)
                .map(|body_z3| {
                    if let Some(rv) = vars.get_bool(&result_key).cloned() {
                        session.solver.assert(rv.eq(&body_z3));
                    }
                }),
        };
        body_encoded = encoded.is_some();
    }

    // 7. Check each ensures independently
    if ensures.is_empty() {
        // If we had math obligations that were all proven, this is a success,
        // not NoObligations — the math was a verification condition.
        if encoding_failures > 0 {
            return Some(status_msg(VerifStatus::NotInTrustedSubset, ""));
        }
        if has_math {
            return Some(status_msg(VerifStatus::Proven, ""));
        }
        return Some(status_msg(VerifStatus::NoObligations, ""));
    }

    // P0 fix: if body encoding failed, result is unconstrained.
    // Any verdict would be unsound → bail out.
    if !body_encoded {
        return Some(status_msg(
            VerifStatus::NotInTrustedSubset,
            "body is outside the trusted subset (unencodable constructs)",
        ));
    }

    let mut found_violation = false;
    let mut found_unknown = false;
    for ens in &ensures {
        if let Some(z3_bool) = resolved_to_z3_bool(ens, body, types, &mut vars) {
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
        return Some(status_msg(VerifStatus::NotInTrustedSubset, ""));
    }
    if found_violation {
        Some(status_msg(VerifStatus::Disproven, ""))
    } else if found_unknown {
        Some(status_msg(VerifStatus::SolverUnknown, ""))
    } else {
        Some(status_msg(VerifStatus::Proven, ""))
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
            // AST round-trip is name-based by construction: the synthesized
            // AST is consumed by the name-keyed FFI substitution layer, not
            // by the id-keyed Z3 encoding (C-7).
            if !place.projections.is_empty() {
                return None;
            }
            let name = body.locals.get(&place.base)?.display_name.clone();
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
    use crate::core::ir::{ResolvedLocal, ResolvedLocalId, ResolvedTypeId};
    use crate::core::{NodeId, Origin};
    use crate::span::Span;
    use std::collections::BTreeMap;

    fn test_origin() -> Origin {
        Origin::User(Span::UNKNOWN)
    }

    fn test_table_and_ty() -> (ResolvedTypeTable, ResolvedTypeId) {
        // Create a ResolvedTypeId by interning a primitive type through the table.
        let mut table = ResolvedTypeTable::new();
        let zonked = crate::core::phase::ZonkedTy::from_resolved(crate::ast::Type::Name(
            "i32".into(),
            Vec::new(),
        ))
        .unwrap();
        let ty = table
            .intern_zonked(&zonked, &Default::default(), |name| {
                crate::core::ir::ResolvedTypeName::primitive(name)
            })
            .unwrap();
        (table, ty)
    }

    fn test_ty() -> ResolvedTypeId {
        test_table_and_ty().1
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
        let (table, _) = test_table_and_ty();
        let body = test_body_with_local("x");
        let mut vars = Z3VarMap::new();
        let expr = test_expr(ResolvedExprKind::Literal(ResolvedLiteral::Int(42)));
        let result = resolved_to_z3_int(&expr, &body, &table, &mut vars);
        assert!(result.is_some(), "literal int must encode to Z3 Int");
    }

    #[test]
    fn resolved_load_encodes_from_vars() {
        let (table, _) = test_table_and_ty();
        let body = test_body_with_local("x");
        let mut vars = Z3VarMap::new();
        // C-7: loads resolve under the id-keyed local key, not the bare name.
        let local_id = ResolvedLocalId(NodeId("local:x".into()));
        vars.insert_int(local_var_key(&local_id, "x"), Z3Int::new_const("x#local:x"));
        let expr = test_expr(ResolvedExprKind::Load(ResolvedPlace::root(local_id)));
        let result = resolved_to_z3_int(&expr, &body, &table, &mut vars);
        assert!(result.is_some(), "load must resolve to Z3 variable");
    }

    #[test]
    fn resolved_binary_add_encodes() {
        let (table, _) = test_table_and_ty();
        let body = test_body_with_local("x");
        let mut vars = Z3VarMap::new();
        let lit = |n: i64| test_expr(ResolvedExprKind::Literal(ResolvedLiteral::Int(n)));
        let expr = test_expr(ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::Add,
            left: Box::new(lit(3)),
            right: Box::new(lit(4)),
        });
        let result = resolved_to_z3_int(&expr, &body, &table, &mut vars);
        assert!(result.is_some(), "binary add must encode");
    }

    #[test]
    fn resolved_bool_comparison_encodes() {
        let (table, _) = test_table_and_ty();
        let body = test_body_with_local("x");
        let mut vars = Z3VarMap::new();
        let lit = |n: i64| test_expr(ResolvedExprKind::Literal(ResolvedLiteral::Int(n)));
        let expr = test_expr(ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::Greater,
            left: Box::new(lit(5)),
            right: Box::new(lit(3)),
        });
        let result = resolved_to_z3_bool(&expr, &body, &table, &mut vars);
        assert!(result.is_some(), "comparison must encode to Z3 Bool");
    }

    #[test]
    fn resolved_old_encodes() {
        let (table, _) = test_table_and_ty();
        let body = test_body_with_local("x");
        let mut vars = Z3VarMap::new();
        let local_id = ResolvedLocalId(NodeId("local:x".into()));
        vars.insert_int(
            format!("old_{}", local_var_key(&local_id, "x")),
            Z3Int::new_const("old_x#local:x"),
        );
        let inner = test_expr(ResolvedExprKind::Load(ResolvedPlace::root(local_id)));
        let expr = test_expr(ResolvedExprKind::Old(Box::new(inner)));
        let result = resolved_to_z3_int(&expr, &body, &table, &mut vars);
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
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                .map(|(status, _)| status);
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
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                .map(|(status, _)| status);
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
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                .map(|(status, _)| status);
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
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                .map(|(status, _)| status);
        // Body is a function call → not encodable → NotInTrustedSubset
        assert_eq!(
            status,
            Some(crate::verifier::ctx::VerifStatus::NotInTrustedSubset),
            "unencodable body (function call) → NotInTrustedSubset, not Disproven"
        );
    }
}
